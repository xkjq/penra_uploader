from markdown2 import log
import asyncio
import subprocess
import platform
import os
from loguru import logger
import pynng


def _find_pid_by_port(port: int) -> int | None:
    """Try to find a pid listening on TCP port using platform-appropriate tools.
    On Windows use `netstat -ano`. On Unix try `ss`, `netstat`, `lsof`, or `fuser`.
    Returns pid or None.
    """
    import re
    system = platform.system().lower()

    # Windows: use netstat -ano and parse PID in last column
    if system.startswith("win"):
        try:
            res = subprocess.run(["netstat", "-ano"], capture_output=True, text=True)
            if res.returncode == 0 and res.stdout:
                for line in res.stdout.splitlines():
                    if f":{port}" in line:
                        parts = line.split()
                        if parts:
                            pid_token = parts[-1]
                            if pid_token.isdigit():
                                return int(pid_token)
        except Exception:
            pass
        return None

    # 1) try ss
    try:
        res = subprocess.run(["ss", "-ltnp"], capture_output=True, text=True)
        if res.returncode == 0 and res.stdout:
            for line in res.stdout.splitlines():
                if f":{port}" in line:
                    m = re.search(r"pid=(\d+)", line)
                    if m:
                        return int(m.group(1))
    except Exception:
        pass

    # 2) try netstat
    try:
        res = subprocess.run(["netstat", "-ltnp"], capture_output=True, text=True)
        if res.returncode == 0 and res.stdout:
            for line in res.stdout.splitlines():
                if f":{port}" in line:
                    m = re.search(r"(\d+)/(?:[\w\-\.]+)", line)
                    if m:
                        return int(m.group(1))
    except Exception:
        pass

    # 3) try lsof
    try:
        res = subprocess.run(["lsof", f"-iTCP:{port}", "-sTCP:LISTEN", "-P", "-n"], capture_output=True, text=True)
        if res.returncode == 0 and res.stdout:
            lines = res.stdout.splitlines()
            for line in lines[1:]:
                parts = line.split()
                if len(parts) >= 2 and parts[1].isdigit():
                    return int(parts[1])
    except Exception:
        pass

    # 4) try fuser
    try:
        res = subprocess.run(["fuser", f"{port}/tcp"], capture_output=True, text=True)
        if res.returncode == 0 and res.stdout:
            m = re.search(r"(\d+)", res.stdout)
            if m:
                return int(m.group(1))
    except Exception:
        pass

    try:
        logger.debug(f"socket_helpers._find_pid_by_port: no pid found for port {port}")
    except Exception:
        pass
    return None


@logger.catch
async def contact_socket_owner(socket_path: str, timeout: int = 10, allow_terminate: bool = True) -> bool:
    """Dial the given socket_path, send a ping and wait up to `timeout` seconds for a reply.
    If no reply, prompt the user to terminate the process holding the port and attempt to send SIGTERM.
    Returns True if a response was received, False otherwise.
    """
    try:
        logger.debug(f"Attempting to contact socket owner at {socket_path} with timeout {timeout}s")    
        with pynng.Pair0(dial=socket_path) as sub:
            await sub.asend(b"loaded")
            try:
                msg = await asyncio.wait_for(sub.arecv_msg(), timeout=timeout)
                logger.debug(f"Received response from socket owner: {msg.bytes}")
                return True
            except asyncio.TimeoutError:
                # If termination is not allowed (e.g. autoselect enabled), don't prompt
                if not allow_terminate:
                    logger.info("No response from socket owner and termination suppressed by settings")
                    return False

                # Attempt to determine owner PID and describe both owner and current process
                try:
                    port = None
                    try:
                        port = int(socket_path.split(":")[-1])
                    except Exception:
                        port = None

                    owner_pid = None
                    if port:
                        owner_pid = _find_pid_by_port(port)

                    owner_desc = None
                    if owner_pid:
                        try:
                            res = subprocess.run(["ps", "-p", str(owner_pid), "-o", "pid=,cmd="], capture_output=True, text=True)
                            owner_desc = res.stdout.strip() if res and res.stdout else None
                        except Exception:
                            owner_desc = None

                    our_pid = os.getpid()
                    our_desc = None
                    try:
                        res2 = subprocess.run(["ps", "-p", str(our_pid), "-o", "pid=,cmd="], capture_output=True, text=True)
                        our_desc = res2.stdout.strip() if res2 and res2.stdout else None
                    except Exception:
                        our_desc = None

                    if owner_pid:
                        logger.info(f"Requested socket {socket_path} appears held by pid {owner_pid}: {owner_desc}")
                    else:
                        logger.info(f"Requested socket {socket_path} appears in use, but owner pid not found")
                    logger.info(f"Current process pid {our_pid}: {our_desc}")
                except Exception as e:
                    logger.debug(f"Failed to determine socket owner info: {e}")

                loop = asyncio.get_event_loop()
                prompt = (
                    f"No response from socket owner within {timeout}s. "
                    f"Terminate process using {socket_path.split(':')[-1]}? [y/N]: "
                )
                answer = await loop.run_in_executor(None, lambda: input(prompt))
                if answer and answer.strip().lower().startswith("y"):
                    import signal

                    # find pid in executor to avoid blocking the event loop
                    pid = None
                    if port:
                        pid = await loop.run_in_executor(None, _find_pid_by_port, port)
                    if pid:
                        try:
                            os.kill(pid, signal.SIGTERM)
                            logger.info(f"Sent SIGTERM to process {pid} holding port {socket_path.split(':')[-1]}")
                        except Exception as e:
                            logger.error(f"Failed to kill process {pid}: {e}")
                    else:
                        logger.error("Could not determine PID of the process holding the socket")
                else:
                    logger.info("User declined to terminate existing process (or no input)")
    except Exception as e:
        logger.debug(f"Error while contacting existing socket owner: {e}")
    return False


def ensure_run(maybe) -> None:
    """If `maybe` is a coroutine, run or schedule it depending on event loop state."""
    if not (asyncio.iscoroutine(maybe) or hasattr(maybe, "__await__")):
        return
    try:
        loop = asyncio.get_event_loop()
        if loop.is_running():
            try:
                asyncio.create_task(maybe)
            except Exception:
                pass
        else:
            loop.run_until_complete(maybe)
    except RuntimeError:
        try:
            asyncio.run(maybe)
        except Exception:
            pass

__all__ = ["contact_socket_owner", "_find_pid_by_port", "ensure_run"]
