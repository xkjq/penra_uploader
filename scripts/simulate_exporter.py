#!/usr/bin/env python3
"""Simulate an external exporter: copy DICOM sets into the uploader `export/` folder
and notify the GUI using the same local IPC or a filesystem sentinel file.

Usage examples:
    python scripts/simulate_exporter.py --source test_dicoms --interval 5 --repeat 3

The default export path matches the GUI expectation: C:/uploader/export/
"""
from pathlib import Path
import shutil
import argparse
import time
# pynng/NNG support removed; uploader_rs now uses local IPC
import socket
import tempfile
import os
import getpass
import sys
import subprocess
import logging

logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("simulate_exporter")


def find_sets(source: Path):
    """Yield subdirectories containing .dcm files. If source itself contains .dcm files,
    it will be yielded as a single set."""
    if not source.exists():
        return
    # If source contains .dcm files at the top level, treat it as a single set
    try:
        top_level = [p for p in source.iterdir() if p.is_file() and p.suffix.lower() == ".dcm"]
    except Exception:
        top_level = []

    if top_level:
        yield source
        return

    # otherwise yield each immediate child directory that contains .dcm files
    for child in sorted(source.iterdir()):
        if child.is_dir():
            try:
                has_dcm = any(child.rglob("*.dcm"))
            except Exception:
                has_dcm = False
            if has_dcm:
                yield child


def copy_set_to_export(set_path: Path, export_path: Path):
    export_path.mkdir(parents=True, exist_ok=True)
    logger.info(f"Copying from {set_path} -> {export_path}")
    copied = []
    for p in sorted(set_path.glob("**/*.dcm")):
        if p.is_file():
            dest = export_path / p.name
            try:
                shutil.copy2(p, dest)
                copied.append(dest)
            except Exception as e:
                logger.warning(f"Failed to copy {p}: {e}")
    logger.info(f"Copied {len(copied)} files")
    return copied


def clear_export(export_path: Path):
    if not export_path.exists():
        return
    for p in list(export_path.iterdir()):
        try:
            if p.is_file():
                p.unlink()
            elif p.is_dir():
                shutil.rmtree(p)
        except Exception as e:
            logger.warning(f"Failed to remove {p}: {e}")




def send_ipc(ipc_name: str, message: bytes = b"loaded", timeout: float = 2.0) -> bool:
    """Attempt to connect to the per-user interprocess local socket used by the Rust app.
    Tries abstract namespace first, then temp dir path. Returns True on success.
    """
    logger.info(f"Sending IPC to {ipc_name}")
    uid = os.getuid() if hasattr(os, "getuid") else None
    candidates = []
    # abstract namespace (Linux)
    candidates.append("\0" + ipc_name)
    # common filesystem locations: temp dir, /tmp, /var/tmp, run user, cwd
    candidates.append(os.path.join(tempfile.gettempdir(), ipc_name))
    candidates.append(os.path.join("/tmp", ipc_name))
    candidates.append(os.path.join("/var/tmp", ipc_name))
    if uid is not None:
        candidates.append(os.path.join("/run/user", str(uid), ipc_name))
    candidates.append(os.path.join(os.getcwd(), ipc_name))

    last_err = None
    for addr in candidates:
        s = None
        try:
            s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            s.settimeout(timeout)
            # If the address uses the abstract namespace (leading NUL), pass bytes
            if isinstance(addr, str) and addr.startswith("\0"):
                s.connect(addr.encode())
            else:
                s.connect(addr)
            s.sendall(message)
            s.close()
            logger.info(f"IPC sent to {addr}")
            return True
        except Exception as e:
            last_err = e
            logger.debug(f"IPC try {addr!r} failed: {e}")
            try:
                if s:
                    s.close()
            except Exception:
                pass
            continue
    logger.error(f"All IPC connection attempts failed: {last_err}")
    return False


def main():
    ap = argparse.ArgumentParser(description="Simulate exporting DICOM sets and notifying uploader")
    ap.add_argument("--source", default="test_dicoms", help="Directory containing example DICOM sets")
    ap.add_argument("--export", default="/home/ross/uploader/uploader_rs/export", help="Uploader export folder to copy into")
    ap.add_argument("--interval", type=float, default=5.0, help="Seconds to wait after notifying uploader before next set")
    ap.add_argument("--repeat", type=int, default=1, help="How many times to cycle through available sets (0 = infinite)")
    ap.add_argument("--clear", action="store_true", help="Clear export folder before copying each set")
    ap.add_argument("--send-ipc", dest="send_ipc", action="store_true", default=True,
                    help="Send a 'loaded' message over local IPC (default: enabled)")
    ap.add_argument("--no-send-ipc", dest="send_ipc", action="store_false",
                    help="Do not send local IPC notifications")
    ap.add_argument("--ipc-name", default=None, help="IPC name to connect to (defaults to uploader_rs_<USER>)")
    ap.add_argument("--run-uploader", action="store_true", help="Run the main Rust `uploader_rs` binary to trigger IPC if an instance is running")
    ap.add_argument("--uploader-bin", default="../uploader_rs/target/debug/uploader_rs", help="Path to compiled uploader_rs binary (falls back to `cargo run --bin uploader_rs`)")
    args = ap.parse_args()

    # Expand user (~) and resolve paths to avoid confusion when users pass ~/... on the CLI
    source = Path(args.source).expanduser().resolve(strict=False)
    export = Path(args.export).expanduser().resolve(strict=False)

    sets = list(find_sets(source))
    if not sets:
        logger.error(f"No DICOM sets found under {source} (resolved path)")
        sys.exit(2)

    logger.info(f"Found {len(sets)} set(s) to publish: {[str(s) for s in sets]}")
    logger.info(f"IPC notifications enabled: {args.send_ipc}")

    loop_forever = args.repeat == 0
    cycles = 0
    while loop_forever or cycles < args.repeat:
        for s in sets:
            if args.clear:
                clear_export(export)
            copy_set_to_export(s, export)
            # give the filesystem a short pause to settle
            time.sleep(0.2)

            if args.send_ipc:
                ipc_name = args.ipc_name
                if not ipc_name:
                    user = os.environ.get("USER") or os.environ.get("USERNAME") or f"pid{os.getpid()}"
                    ipc_name = f"uploader_rs_{user}"
                try:
                    ok = send_ipc(ipc_name, b"loaded", timeout=2.0)
                    logger.info(f"IPC send ok={ok}")
                except Exception as e:
                    logger.warning(f"IPC send failed: {e}")
            else:
                logger.info("IPC send skipped (--no-send-ipc)")

            if args.run_uploader:
                ub = Path(args.uploader_bin).expanduser()
                if ub.exists() and os.access(ub, os.X_OK):
                    try:
                        env = os.environ.copy()
                        if "USER" not in env and "USERNAME" in env:
                            env["USER"] = env["USERNAME"]
                        res = subprocess.run([str(ub)], capture_output=True, text=True, timeout=30, env=env)
                        logger.info(f"Ran uploader binary {ub}, returncode={res.returncode}")
                        if res.stdout:
                            logger.info(res.stdout)
                        if res.stderr:
                            logger.info(res.stderr)
                    except Exception as e:
                        logger.warning(f"Failed to run uploader binary {ub}: {e}")
                else:
                    try:
                        repo_dir = Path(__file__).resolve().parent.parent
                        uploader_rs_dir = repo_dir / "uploader_rs"
                        logger.info("uploader binary not found; running `cargo run --bin uploader_rs` in uploader_rs")
                        env = os.environ.copy()
                        if "USER" not in env and "USERNAME" in env:
                            env["USER"] = env["USERNAME"]
                        res = subprocess.run(["cargo", "run", "--bin", "uploader_rs"], cwd=str(uploader_rs_dir), capture_output=True, text=True, timeout=120, env=env)
                        logger.info(f"cargo uploader returncode={res.returncode}")
                        if res.stdout:
                            logger.info(res.stdout)
                        if res.stderr:
                            logger.info(res.stderr)
                    except Exception as e:
                        logger.warning(f"Failed to run cargo uploader: {e}")


            logger.info(f"Waiting {args.interval}s before next set")
            time.sleep(args.interval)
            if not (loop_forever or cycles < args.repeat):
                break
        cycles += 1


if __name__ == "__main__":
    main()
