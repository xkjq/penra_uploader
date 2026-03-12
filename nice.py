#!/usr/bin/env python3
import asyncio
import pynng

# asyncio.set_event_loop_policy(asyncio.WindowsSelectorEventLoopPolicy())
from collections import defaultdict
from pathlib import Path

from nicegui import ui, app, html, run, Client, native

from datetime import datetime

import logging

from loguru import logger
import sys
import os

from anonymiser import Anonymizer
import pydicom
import subprocess
import platform
import shutil

import requests
import time
import blake3

from multiprocessing import Manager, Queue, freeze_support

from functools import partial

from local_file_picker import local_file_picker
from local_folder_picker import local_folder_picker

from typing import Optional

from fastapi import Request
from fastapi.responses import RedirectResponse
from starlette.middleware.base import BaseHTTPMiddleware
import typer
import logging

from socket_helpers import contact_socket_owner, ensure_run as _ensure_run
import config as cfg


WORK_DIR = Path("C:/uploader")

EXPORT_PATH = WORK_DIR / Path("export/")
PROCESS_PATH = WORK_DIR / Path("to_process/")
ANON_PATH = WORK_DIR / Path("anon/")

EXPORT_PATH.mkdir(exist_ok=True, parents=True)
PROCESS_PATH.mkdir(exist_ok=True, parents=True)
ANON_PATH.mkdir(exist_ok=True, parents=True)


# Server endpoints and defaults
TOKEN_AUTH_PATH = "/api/atlas/create_api_token"  # POST {username,password} -> {token: '...'}
TOKEN_CHECK_PATH = "/api/atlas/token_check"
HASH_CHECK_PATH = "/api/atlas/check_image_hashes/"

SOCKET_PATH = "tcp://localhost:9976"

# Use cfg.BASE_SITE_URL or cfg.PROD_BASE_SITE_URL where needed; LOGIN URL is
# constructed at time of use (e.g. f"{cfg.BASE_SITE_URL}/accounts/login/").

LOGIN_SUCCESS = False

LOGGED_IN_USER = "None"

REFRESH_TRIGGERED = True
SHUTDOWN = False

DELETE_FILES_ON_UPLOAD = True
rqst = None
API_TOKEN = None
APP_NAME = "uploader_tool"

try:
    import keyring

    KEYRING_AVAILABLE = True
except Exception:
    KEYRING_AVAILABLE = False

logging.getLogger("niceGUI").setLevel(logging.INFO)

# Choose a log file path that works both during development and when the
# application is frozen into a single executable (PyInstaller --onefile).
try:
    if getattr(sys, "frozen", False):
        # When frozen by PyInstaller, data files are extracted to a temporary
        # folder available as sys._MEIPASS. Fall back to the executable
        # directory when _MEIPASS is not present.
        meipass = getattr(sys, "_MEIPASS", None)
        if meipass:
            base_path = Path(meipass)
        else:
            base_path = Path(sys.executable).resolve().parent
    else:
        base_path = Path(__file__).resolve().parent
except Exception:
    base_path = Path.cwd()



def token_file_path() -> Path:
    home = Path.home()
    cfg = home / ".uploader"
    cfg.mkdir(parents=True, exist_ok=True)
    return cfg / "api_token"


def save_api_token(token: str) -> None:
    logger.trace(f"Saving API token, length={len(token)}")
    global API_TOKEN, rqst
    API_TOKEN = token
    if KEYRING_AVAILABLE:
        try:
            keyring.set_password(APP_NAME, "api_token", token)
        except Exception:
            logger.error("Failed to save API token to keyring")
    else:
        logger.warning("Keyring not available, saving API token to file (less secure)")
        try:
            p = token_file_path()
            p.write_text(token)
            try:
                os.chmod(p, 0o600)
            except Exception:
                logger.warning("Failed to set file permissions on API token file")
                pass
        except Exception:
            logger.error("Failed to save API token to file")
            pass

    rqst = requests.session()
    rqst.headers["Authorization"] = f"Bearer {token}"

    # validate token and set login state
    info = check_token()
    global LOGIN_SUCCESS, LOGGED_IN_USER
    if info and info.get("valid"):
        LOGIN_SUCCESS = True
        LOGGED_IN_USER = info.get("username") or "API token"
    else:
        LOGIN_SUCCESS = False
        LOGGED_IN_USER = "None"


def load_api_token() -> str | None:
    logger.trace("Loading API token")
    global API_TOKEN, rqst
    if KEYRING_AVAILABLE:
        try:
            token = keyring.get_password(APP_NAME, "api_token")
        except Exception:
            token = None
    else:
        p = token_file_path()
        token = p.read_text() if p.exists() else None

    if token:
        API_TOKEN = token
        rqst = requests.session()
        rqst.headers["Authorization"] = f"Bearer {token}"

        # validate token
        info = check_token()
        logger.debug(f"Token check info: {info}")
        global LOGIN_SUCCESS, LOGGED_IN_USER
        if info and info.get("valid"):
            LOGIN_SUCCESS = True
            LOGGED_IN_USER = info.get("username")
        else:
            logger.debug("Stored API token invalid on load")
            # invalid token, clear
            API_TOKEN = None
            rqst = None
            try:
                clear_api_token()
            except Exception:
                logger.error("Failed to clear invalid API token")

    logger.trace(f"Loaded API token: {token}")
    return token


def check_token() -> dict | None:
    """Call the token_check endpoint and return parsed JSON or None on error."""
    logger.trace("Checking API token validity")
    global rqst
    try:
        # Prefer header auth if session exists
        if rqst and "Authorization" in rqst.headers:
            resp = rqst.post(f"{cfg.BASE_SITE_URL}{TOKEN_CHECK_PATH}")
        else:
            # try to read token from storage and POST as json
            token = None
            if KEYRING_AVAILABLE:
                try:
                    token = keyring.get_password(APP_NAME, "api_token")
                except Exception:
                    token = None
            else:
                p = token_file_path()
                token = p.read_text() if p.exists() else None

            if not token:
                return None
            resp = requests.post(
                f"{cfg.BASE_SITE_URL}{TOKEN_CHECK_PATH}", json={"token": token}, timeout=5
            )

        if resp.status_code != 200:
            return None
        data = resp.json()
        return data
    except Exception as e:
        logger.debug(f"token_check failed: {e}")
        return None


def clear_api_token() -> None:
    logger.trace("Clearing API token")
    global API_TOKEN, rqst
    API_TOKEN = None
    if KEYRING_AVAILABLE:
        try:
            keyring.delete_password(APP_NAME, "api_token")
        except Exception:
            pass
    else:
        p = token_file_path()
        try:
            if p.exists():
                p.unlink()
        except Exception:
            pass

    if rqst:
        rqst.headers.pop("Authorization", None)
    global LOGIN_SUCCESS, LOGGED_IN_USER
    LOGIN_SUCCESS = False
    LOGGED_IN_USER = "None"


loaded_files: dict = {}
loaded_series = defaultdict(list)
loaded_series_data = {}
loaded_duplicate_series = set()
loaded_duplicate_series_links = defaultdict(set)
uploaded_files: dict = {}
duplicate_series = set()
OPEN_LINKS_PROD = False
AUTSELECT_NNG_PORT = False
ALLOW_INSECURE_RETRY = False
_requests_orig_request = None

def enable_skip_verify():
    """Globally disable SSL verification for requests.
    Monkeypatches `requests.sessions.Session.request` to default `verify=False`
    and silences the InsecureRequestWarning.
    """
    logger.trace("Enabling global skip-verify for requests")
    global _requests_orig_request
    try:
        import urllib3

        urllib3.disable_warnings(urllib3.exceptions.InsecureRequestWarning)
    except Exception:
        pass

    if _requests_orig_request is None:
        try:
            _requests_orig_request = requests.sessions.Session.request

            def _patched_request(self, method, url, *args, **kwargs):
                logger.debug(f"Patched request called: {method} {url} verify={kwargs.get('verify', 'not set')}")
                if "verify" not in kwargs:
                    kwargs["verify"] = False
                return _requests_orig_request(self, method, url, *args, **kwargs)

            requests.sessions.Session.request = _patched_request
            logger.warning("Global requests verification disabled (skip-verify enabled)")
        except Exception as e:
            logger.debug(f"Failed to enable global skip-verify: {e}")


def disable_skip_verify():
    """Restore original requests behavior (re-enable verification)."""
    logger.trace("Disabling global skip-verify for requests")
    global _requests_orig_request
    try:
        if _requests_orig_request is not None:
            requests.sessions.Session.request = _requests_orig_request
            _requests_orig_request = None
            logger.info("Global requests verification re-enabled")
    except Exception as e:
        logger.debug(f"Failed to disable global skip-verify: {e}")

def human_size(num, suffix="B"):
    # Convert bytes to human-readable string (KiB, MiB, ...)
    try:
        n = float(num)
    except Exception:
        return "0 B"
    for unit in ["", "Ki", "Mi", "Gi", "Ti", "Pi"]:
        if abs(n) < 1024.0:
            return f"{n:3.1f} {unit}{suffix}"
        n /= 1024.0
    return f"{n:.1f} Pi{suffix}"


import socket


def find_free_tcp_port(start_port: int, max_tries: int = 100) -> int | None:
    """Scan forward from start_port to find a free TCP port on localhost.
    Returns the first free port found or None if none available within max_tries.
    """
    logger.debug(f"Scanning for free TCP port starting at {start_port} up to {start_port + max_tries - 1}")
    for p in range(start_port, start_port + max_tries):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            try:
                s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                s.bind(("127.0.0.1", p))
                return p
            except OSError:
                continue
    return None




async def read_nng_messages():
    logger.debug("start nng")
    try:
        with pynng.Pair0(listen=SOCKET_PATH) as sub:
            logger.debug(f"Successfully bound NNG socket at {SOCKET_PATH}")
            # sub.subscribe('')
            while not app.is_stopped:
                msg = await sub.arecv_msg()
                data = msg.bytes.decode()
                logger.debug(f"Received msg {data}")
                match data:
                    case "loaded":
                        for client in Client.instances.values():
                            if not client.has_socket_connection:
                                continue
                            with client:
                                ui.notify("Export done", color="positive")
                        global REFRESH_TRIGGERED
                        REFRESH_TRIGGERED = True
                        await sub.asend(b"done")
                        logger.debug("loaded")
                    case _:
                        logger.error("invalid message")
    except pynng.exceptions.AddressInUse:
        logger.debug("Address in use")
        await contact_socket_owner(SOCKET_PATH, timeout=10, allow_terminate=(not AUTSELECT_NNG_PORT))

        # Trigger shutdown from in the async loop
        global SHUTDOWN
        SHUTDOWN = True
        return

    # asyncio.create_task(read_nng_messages())


# anonymizer will be initialized at startup inside `launch_app` so debug
# mode can override the paths before the object is created.
anonymizer: Anonymizer | None = None


async def upload_files_start(progress, upload_queue, case_id=None):
    logger.debug("upload files start")
    logger.trace(f"Preparing to upload files, number: {len(upload_queue)}, case_id: {case_id}")
    global \
        uploaded_files, \
        duplicate_series, \
        loaded_files, \
        loaded_series, \
        loaded_series_data
    progress.visible = True

    # case_id: optional - if provided files will be uploaded into that case
    results = await run.cpu_bound(upload_files, upload_queue, rqst, case_id)
    progress.visible = False

    if results:
        (
            new_uploaded_files,
            upload_file_list,
            duplicate_file_list,
            failed,
            duplicate_series,
        ) = results
    else:
        for client in Client.instances.values():
            if not client.has_socket_connection:
                continue
            with client:
                ui.notify("No files to upload", color="negative")
        return

    uploaded_files.update(new_uploaded_files)

    for client in Client.instances.values():
        logger.debug(
            f"Notify client of upload results: {len(upload_file_list)} uploaded, {len(duplicate_file_list)} duplicates, {len(failed)} failed"
        )
        if not client.has_socket_connection:
            logger.debug("Client has no socket connection, skipping notification")
            continue
        with client:
            uploaded_count = len(upload_file_list)
            duplicate_count = len(duplicate_file_list)
            failed_count = len(failed)
            logger.debug(
                f"Upload summary: {uploaded_count} uploaded, {duplicate_count} duplicates, {failed_count} failed"
            )

            if duplicate_count or failed_count:
                notification_color = "negative" if failed_count else "warning"
                summary = (
                    f"Upload complete: {uploaded_count} uploaded, "
                    f"{duplicate_count} skipped as duplicates, {failed_count} failed"
                )

                if duplicate_series:
                    logger.debug(f"Duplicate series: {duplicate_series}")
                    series_text = "\n".join(sorted(duplicate_series))
                    ui.notify(
                        f"{summary}\n\nDuplicate series:\n{series_text}\n\nUse Uploaded files to open links.",
                        color=notification_color,
                        timeout=0,
                        close_button="Dismiss",
                        multi_line=True,
                    )
                else:
                    logger.debug("No duplicate series")
                    if duplicate_count > 0:
                        summary = (
                            f"{summary}\n\n"
                            "Duplicate series are not available yet. "
                            "Files were are already waiting to be imported on the server."
                        )
                    ui.notify(
                        summary,
                        color=notification_color,
                        timeout=0,
                        close_button="Dismiss",
                        multi_line=True,
                    )
            else:
                logger.debug(
                    "All files uploaded successfully with no duplicates or failures"
                )
                ui.notify(f"Uploaded {uploaded_count} files", color="positive")

    if DELETE_FILES_ON_UPLOAD:
        # clear only files that were part of the successful upload or marked duplicate
        files_to_clear = []
        files_to_clear.extend(upload_file_list or [])
        files_to_clear.extend(duplicate_file_list or [])
        clear_anonymized_files(files_to_clear)

        # keep in-memory loaded state in sync with files that actually still exist
        loaded_files = {
            file: data for file, data in loaded_files.items() if Path(file).exists()
        }

        filtered_loaded_series = defaultdict(list)
        for series_uid, series_files in loaded_series.items():
            remaining_series_files = [
                series_file
                for series_file in series_files
                if Path(series_file).exists()
            ]
            if remaining_series_files:
                filtered_loaded_series[series_uid] = remaining_series_files

        loaded_series = filtered_loaded_series
        loaded_series_data = {
            series_uid: series_data
            for series_uid, series_data in loaded_series_data.items()
            if series_uid in loaded_series
        }

    for client in Client.instances.values():
        if not client.has_socket_connection:
            continue
        with client:
            logger.debug("Refreshing post-upload UI sections")
            await loaded_series_ui.refresh()
            await loaded_files_ui.refresh()
            await uploaded_files_ui.refresh()
            ui.run_javascript(
                """
                setTimeout(() => {
                    const openExpansionIfClosed = (id) => {
                        const section = document.getElementById(id);
                        if (!section) return null;
                        const toggle = section.querySelector('.q-item[aria-expanded]');
                        if (toggle && toggle.getAttribute('aria-expanded') !== 'true') {
                            toggle.click();
                        }
                        return section;
                    };

                    openExpansionIfClosed('file-status-section');
                    const uploadedSection = document.getElementById('uploaded-files-section');
                    openExpansionIfClosed('uploaded-files-section');
                    if (uploadedSection) {
                        uploadedSection.scrollIntoView({ behavior: 'smooth', block: 'center' });
                    }
                }, 0);
                """
            )


async def load_files_start(progress_bar, queue, custom_path=None, copy=False):
    logger.debug("load files start")
    global \
        loaded_files, \
        loaded_series, \
        loaded_series_data, \
        loaded_duplicate_series, \
        loaded_duplicate_series_links
    progress_bar.visible = True

    (
        new_loaded_files,
        new_loaded_series,
        new_loaded_series_data,
        new_loaded_duplicate_series,
        new_loaded_duplicate_series_links,
    ) = await run.cpu_bound(load_files, queue, custom_path, copy, rqst)

    for client in Client.instances.values():
        if not client.has_socket_connection:
            continue
        with client:
            ui.notify(
                f"Files loaded: {len(new_loaded_files)}, [Series: {len(new_loaded_series)}] ",
                color="positive",
            )

    loaded_files.update(new_loaded_files)
    loaded_series.update(new_loaded_series)
    loaded_series_data.update(new_loaded_series_data)
    loaded_duplicate_series.update(new_loaded_duplicate_series)
    for series_uid, links in new_loaded_duplicate_series_links.items():
        loaded_duplicate_series_links[series_uid].update(links)

    progress_bar.visible = False
    # load_series_view(loaded_series, loaded_series_data)
    await loaded_series_ui.refresh()
    await loaded_files_ui.refresh()


async def reload_anonymized_start(progress_bar, queue):
    global \
        loaded_files, \
        loaded_series, \
        loaded_series_data, \
        loaded_duplicate_series, \
        loaded_duplicate_series_links
    progress_bar.visible = True
    loaded_files, loaded_series, loaded_series_data = await run.cpu_bound(
        reload_anonymized, queue
    )
    loaded_duplicate_series = set()
    loaded_duplicate_series_links = defaultdict(set)
    progress_bar.visible = False
    for client in Client.instances.values():
        if not client.has_socket_connection:
            continue
        with client:
            ui.notify(
                f"Files reloaded: {len(loaded_files)}, [Series: {len(loaded_series)}] ",
                color="positive",
            )
    # load_series_view(loaded_series, loaded_series_data)
    await loaded_series_ui.refresh()
    await loaded_files_ui.refresh()


@ui.refreshable
def user_info_ui():
    global LOGGED_IN_USER
    user_label = ui.label(f"User: {LOGGED_IN_USER}")
    # user_label.bind_visibility_from(globals(), "LOGIN_SUCCESS")


@ui.refreshable
def loaded_series_ui() -> None:
    global \
        loaded_series, \
        loaded_series_data, \
        loaded_duplicate_series, \
        loaded_duplicate_series_links, \
        loaded_files

    series_title = ui.label("Series to upload").classes("text-h3")
    series_title.visible = False

    series = ui.row()

    # def load_series_view(loaded_series, loaded_series_data):
    logger.debug("load series view")
    logger.debug(f"Loaded series count: {len(loaded_series)}")
    for key in loaded_series:
        logger.debug(f"load series: {key}")
        with series:
            with ui.card():

                def remove_series(series_uid=key):
                    global \
                        loaded_series, \
                        loaded_series_data, \
                        loaded_duplicate_series, \
                        loaded_duplicate_series_links, \
                        loaded_files

                    series_files = list(loaded_series.get(series_uid, []))
                    removed_count = 0
                    for series_file in series_files:
                        try:
                            series_path = Path(series_file)
                            if series_path.exists():
                                series_path.unlink()
                                removed_count += 1
                        except Exception as e:
                            logger.error(f"Failed to delete file {series_file}: {e}")

                    series_file_paths = {
                        Path(series_file) for series_file in series_files
                    }
                    loaded_files = {
                        file_path: file_data
                        for file_path, file_data in loaded_files.items()
                        if Path(file_path) not in series_file_paths
                    }

                    loaded_series.pop(series_uid, None)
                    loaded_series_data.pop(series_uid, None)
                    loaded_duplicate_series.discard(series_uid)
                    loaded_duplicate_series_links.pop(series_uid, None)

                    ui.notify(
                        f"Removed series {series_uid} and {removed_count} file(s)",
                        color="warning",
                    )

                    _ensure_run(loaded_series_ui.refresh())
                    _ensure_run(loaded_files_ui.refresh())

                ui.label(loaded_series_data[key][0])
                ui.label(loaded_series_data[key][1])
                if key in loaded_duplicate_series:
                    ui.label("Duplicate detected on server").classes("text-red-500")
                    series_links = loaded_duplicate_series_links.get(key, set())
                    if series_links:
                        base = cfg.PROD_BASE_SITE_URL if OPEN_LINKS_PROD else cfg.BASE_SITE_URL
                        for link in sorted(series_links):
                            series_link = (
                                link
                                if str(link).startswith("http")
                                else f"{base}{link}"
                            )
                            ui.link(
                                f"Open duplicate series: {link}",
                                series_link,
                                new_tab=True,
                            ).classes("text-red-500")
                    else:
                        ui.label(
                            "Series link not available yet (pending import)"
                        ).classes("text-orange-500")
                ui.label(f"Images: {len(loaded_series[key])}")
                # compute total size for the series
                total = 0
                for p in loaded_series[key]:
                    file_path = Path(p)
                    if file_path.exists():
                        total += file_path.stat().st_size
                ui.label(f"Size: {human_size(total)}")

                with ui.dialog() as remove_series_dialog:
                    with ui.card().classes("w-96"):
                        ui.label("Remove series?").classes("text-h6")
                        ui.label(f"Series: {key}")
                        ui.label(f"This will delete {len(loaded_series[key])} file(s).")

                        def confirm_remove_series(
                            series_uid=key, dialog=remove_series_dialog
                        ):
                            dialog.close()
                            remove_series(series_uid)

                        with ui.row().classes("items-center gap-2"):
                            ui.button(
                                "Cancel",
                                on_click=remove_series_dialog.close,
                                color="secondary",
                            )
                            ui.button(
                                "Remove",
                                on_click=confirm_remove_series,
                                color="negative",
                            )

                ui.button(
                    "Remove series",
                    on_click=remove_series_dialog.open,
                    color="negative",
                )

    if loaded_series:
        series_title.visible = True


@ui.refreshable
def loaded_files_ui() -> None:
    with ui.expansion("Loaded files", icon="drive_folder_upload").classes("w-full"):
        ui.label(f"Items: {len(loaded_files)}")
        with ui.list():
            for file, data in loaded_files.items():
                ui.item(f"{file} - {data[0]} - {data[1]}")


@ui.refreshable
def uploaded_files_ui() -> None:
    with (
        ui.expansion("Uploaded files", icon="file_upload")
        .classes("w-full")
        .props("id=uploaded-files-section")
    ):
        ui.label(f"Items: {len(uploaded_files)}")
        with ui.list():
            for series in duplicate_series:
                base = cfg.PROD_BASE_SITE_URL if OPEN_LINKS_PROD else cfg.BASE_SITE_URL
                series_link = (
                    series if str(series).startswith("http") else f"{base}{series}"
                )
                ui.link(
                    f"Duplicate series: {series}", series_link, new_tab=True
                ).classes("text-red-500 block")
        with ui.list():
            for file, data in uploaded_files.items():
                ui.item(f"{file} - {data}")


def load_files(q: Queue, src_path: Path | None = None, copy: bool = False, rqst=None):
    loaded_files: dict = {}
    loaded_series = defaultdict(list)
    loaded_series_data = {}
    loaded_duplicate_series = set()
    loaded_duplicate_series_links = defaultdict(set)
    output_file_hashes = {}
    output_file_series = {}
    # Move files
    if src_path is None:
        src_path = EXPORT_PATH

    to_process = []
    for file in src_path.glob("**/*.dcm"):
        if copy:
            logger.debug(f"Copying {file} to {PROCESS_PATH}")
        else:
            logger.debug(f"Moving {file} to {PROCESS_PATH}")

        file_to_process = PROCESS_PATH / file.name
        try:
            if copy:
                shutil.copy2(file, file_to_process)
            else:
                file.rename(file_to_process)
        except Exception as e:
            logger.error(f"Failed to copy/move {file}: {e}")
            continue

        to_process.append(file_to_process)

    to_process_len = len(to_process)
    for n, file in enumerate(to_process):
        # `anonymizer` is initialized in `launch_app` after paths are set.
        # Add a runtime check so static type checkers know it's not None here.
        assert anonymizer is not None, "Anonymizer not initialized"
        dataset, output_file = anonymizer.anonymize_file(file, remove_original=True)
        # with processed_files:
        #    ui.item(f"{datetime.now().strftime('%H:%M:%S')} - {file.name} -> {output_file.name}")

        loaded_files[output_file] = (
            dataset.StudyDescription,
            dataset.SeriesDescription,
            str(dataset.SeriesInstanceUID),
        )

        loaded_series[dataset.SeriesInstanceUID].append(output_file)
        loaded_series_data[dataset.SeriesInstanceUID] = (
            dataset.StudyDescription,
            dataset.SeriesDescription,
        )
        output_file_series[output_file] = dataset.SeriesInstanceUID

        try:
            with pydicom.dcmread(str(output_file), force=True) as hashed_dataset:
                if "PixelData" in hashed_dataset and hashed_dataset.PixelData:
                    hash_payload = hashed_dataset.PixelData
                else:
                    hash_payload = Path(output_file).read_bytes()
            output_file_hashes[output_file] = blake3.blake3(hash_payload).hexdigest()
        except Exception:
            try:
                output_file_hashes[output_file] = blake3.blake3(
                    Path(output_file).read_bytes()
                ).hexdigest()
            except Exception as e:
                logger.warning(f"Hashing failed for {output_file}: {e}")

        q.put_nowait((n + 1) / to_process_len)

        # time.sleep(0.01)

    if rqst and output_file_hashes:
        try:
            if "Authorization" not in rqst.headers:
                rqst.headers["X-CSRFToken"] = rqst.cookies.get("csrftoken", "")

            hash_payload = list(set(output_file_hashes.values()))
            resp = rqst.post(f"{cfg.BASE_SITE_URL}{HASH_CHECK_PATH}", json=hash_payload)
            if resp.status_code in (400, 422):
                resp = rqst.post(
                    f"{cfg.BASE_SITE_URL}{HASH_CHECK_PATH}", json={"hashes": hash_payload}
                )

            if resp.status_code == 200:
                hash_status = resp.json()
                if isinstance(hash_status, dict):
                    for output_file, file_hash in output_file_hashes.items():
                        hash_info = hash_status.get(file_hash)
                        if not isinstance(hash_info, dict):
                            continue
                        if not hash_info.get("id"):
                            continue

                        series_uid = output_file_series.get(output_file)
                        if not series_uid:
                            continue

                        loaded_duplicate_series.add(series_uid)
                        hash_url = hash_info.get("url")
                        if hash_url:
                            loaded_duplicate_series_links[series_uid].add(hash_url)

                    logger.debug(
                        f"Load hash check summary: checked={len(output_file_hashes)}, duplicate_series={len(loaded_duplicate_series)}"
                    )
        except Exception as e:
            logger.warning(f"Load-time hash check failed: {e}")

    return (
        loaded_files,
        loaded_series,
        loaded_series_data,
        loaded_duplicate_series,
        loaded_duplicate_series_links,
    )


def reload_anonymized(q: Queue):
    loaded_files = {}
    loaded_series = defaultdict(list)
    loaded_series_data = {}

    # processed_files.clear()

    to_process = list(ANON_PATH.glob("**/*.dcm"))
    to_process_len = len(to_process)
    for n, file in enumerate(to_process):
        with pydicom.dcmread(file) as dataset:
            loaded_files[file] = (
                dataset.StudyDescription,
                dataset.SeriesDescription,
                str(dataset.SeriesInstanceUID),
            )

            loaded_series[dataset.SeriesInstanceUID].append(file)
            loaded_series_data[dataset.SeriesInstanceUID] = (
                dataset.StudyDescription,
                dataset.SeriesDescription,
            )

        # with processed_files:
        #    ui.item(f"{datetime.now().strftime('%H:%M:%S')} - {file.name} -> Reloaded")

        q.put_nowait(f"{(n + 1) / to_process_len * 100:.0f}%")
        logger.debug(file)

        # time.sleep(0.01)

    logger.debug("end reload")
    logger.debug(loaded_files)
    logger.debug(loaded_series)
    logger.debug("-----")

    return loaded_files, loaded_series, loaded_series_data


def clear_anonymized_files(uploaded_file_list=None):
    """Remove files from ANON_PATH.

    If `uploaded_file_list` is provided (list of filenames or tuples where the
    first element is the filename), only those files will be removed. If not
    provided, all files in ANON_PATH are removed.
    """
    if uploaded_file_list:
        # normalize names to remove: handle tuples like (filename, hash)
        names = set()
        for item in uploaded_file_list:
            try:
                if isinstance(item, (list, tuple)) and len(item) > 0:
                    candidate = item[0]
                else:
                    candidate = item
                candidate = str(candidate)
                # take basename if a path was provided
                names.add(Path(candidate).name)
            except Exception:
                continue

        removed = 0
        for file in ANON_PATH.iterdir():
            if not file.is_file():
                continue
            if file.name in names or str(file) in names:
                try:
                    file.unlink()
                    removed += 1
                except Exception as e:
                    logger.error(f"Failed to delete {file}: {e}")

        logger.debug(
            f"Deleted {removed} files from ANON_PATH (of {len(names)} requested)"
        )
    else:
        for file in ANON_PATH.iterdir():
            if file.is_file():
                try:
                    file.unlink()
                except Exception as e:
                    logger.error(f"Failed to delete {file}: {e}")

        logger.debug("Deleted all files in ANON_PATH")

    return


def upload_files(q, rqst, case_id=None):
    # global rqst#, uploaded_files

    uploaded_files = {}

    files_in_anon = [file for file in ANON_PATH.iterdir() if file.is_file()]
    if not files_in_anon:
        return None

    def calculate_dicom_image_hash(file_path: Path) -> str | None:
        try:
            dataset = pydicom.dcmread(str(file_path), force=True)
            if "PixelData" in dataset and dataset.PixelData:
                payload = dataset.PixelData
            else:
                payload = file_path.read_bytes()
            return blake3.blake3(payload).hexdigest()
        except Exception:
            try:
                return blake3.blake3(file_path.read_bytes()).hexdigest()
            except Exception as e:
                logger.error(f"Failed to hash {file_path}: {e}")
                return None

    def precheck_duplicate_hashes(file_paths: list[Path]):
        hash_to_paths = defaultdict(list)
        path_to_hash = {}

        for file_path in file_paths:
            file_hash = calculate_dicom_image_hash(file_path)
            if not file_hash:
                continue
            path_to_hash[file_path] = file_hash
            hash_to_paths[file_hash].append(file_path)

        if not hash_to_paths:
            return file_paths, [], set()

        try:
            if "Authorization" not in rqst.headers:
                rqst.headers["X-CSRFToken"] = rqst.cookies.get("csrftoken", "")

            hash_payload = list(hash_to_paths.keys())
            resp = rqst.post(f"{cfg.BASE_SITE_URL}{HASH_CHECK_PATH}", json=hash_payload)
            if resp.status_code in (400, 422):
                resp = rqst.post(
                    f"{cfg.BASE_SITE_URL}{HASH_CHECK_PATH}", json={"hashes": hash_payload}
                )
            if resp.status_code != 200:
                logger.warning(f"Pre-upload hash check failed: {resp.status_code}")
                return file_paths, [], set()

            status = resp.json()
            duplicate_hashes = set()
            pre_duplicate_files = []
            pre_duplicate_series = set()

            if isinstance(status, dict):
                for hash_value, hash_info in status.items():
                    if not isinstance(hash_info, dict):
                        continue
                    if not hash_info.get("id"):
                        continue

                    duplicate_hashes.add(hash_value)
                    hash_url = hash_info.get("url")
                    if hash_url:
                        pre_duplicate_series.add(hash_url)

                    for duplicate_path in hash_to_paths.get(hash_value, []):
                        pre_duplicate_files.append((duplicate_path.name, hash_value))

            files_after_precheck = [
                file_path
                for file_path in file_paths
                if path_to_hash.get(file_path) not in duplicate_hashes
            ]

            logger.debug(
                f"Pre-upload hash check summary: checked={len(hash_to_paths)}, pre_duplicates={len(pre_duplicate_files)}, remaining={len(files_after_precheck)}"
            )
            return files_after_precheck, pre_duplicate_files, pre_duplicate_series
        except Exception as e:
            logger.warning(f"Pre-upload hash check error: {e}")
            return file_paths, [], set()

    files_after_precheck, pre_duplicate_file_list, pre_duplicate_series = (
        precheck_duplicate_hashes(files_in_anon)
    )

    files_to_upload = []
    for file in files_after_precheck:
        files_to_upload.append(("files", open(str(file), "rb")))

    # chunck files
    n = 10
    chunked_files = [
        files_to_upload[i : i + n] for i in range(0, len(files_to_upload), n)
    ]

    upload_file_list = []
    duplicate_file_list = list(pre_duplicate_file_list)
    failed = []
    duplicate_series = set(pre_duplicate_series)

    logger.debug("START UPLOAD")
    logger.debug(f"Files queued for upload after pre-check: {len(files_to_upload)}")

    if not files_to_upload:
        logger.debug("All files were identified as duplicates by pre-upload hash check")
        for f, hash in duplicate_file_list:
            uploaded_files[f] = "duplicate"

        return (
            uploaded_files,
            upload_file_list,
            duplicate_file_list,
            failed,
            duplicate_series,
        )

    to_process_len = len(chunked_files)

    try:
        for n, files in enumerate(chunked_files):

            def upload_files_(files):
                # If we're using token auth, don't set CSRF header
                if "Authorization" not in rqst.headers:
                    rqst.headers["X-CSRFToken"] = rqst.cookies.get("csrftoken", "")

                # choose endpoint based on whether a case_id was supplied
                if case_id:
                    endpoint = f"{cfg.BASE_SITE_URL}/api/atlas/upload_dicom_case/{case_id}"
                else:
                    endpoint = f"{cfg.BASE_SITE_URL}/api/atlas/upload_dicom"

                resp = rqst.post(endpoint, files=files)

                logger.debug(f"Endpoint: {endpoint}")
                logger.debug(f"Resp: {resp}")
                logger.debug(f"Resp bytes: {len(resp.content)}")
                return resp

            # progress_dialog.Update(n, f"Uploading batch {n}/{len(chunked_files)}")

            logger.debug(f"n: {n}")
            # try to upload the files

            for i in range(3):
                resp = upload_files_(files)
                if resp.status_code == 200:
                    upload_file_list.extend(resp.json()["uploaded"])
                    duplicate_file_list.extend(resp.json()["duplicates"])
                    failed.extend(resp.json()["failed"])
                    duplicate_series.update(resp.json()["duplicate_series"])

                    break

                logger.error(f"i: {i}")
                logger.debug(f"n: {n} fail (attempt {i})")

            q.put_nowait((n + 1) / to_process_len)
            # progress_dialog.Destroy()
    finally:
        for _, file_handle in files_to_upload:
            try:
                file_handle.close()
            except Exception:
                pass
    logger.debug(
        f"Upload result summary: uploaded={len(upload_file_list)}, duplicates={len(duplicate_file_list)}, failed={len(failed)}, duplicate_series={len(duplicate_series)}"
    )

    for f, hash in upload_file_list:
        uploaded_files[f] = "success"

    for f, hash in duplicate_file_list:
        uploaded_files[f] = "duplicate"

    for f in failed:
        uploaded_files[f] = "failed"

    return (
        uploaded_files,
        upload_file_list,
        duplicate_file_list,
        failed,
        duplicate_series,
    )


@logger.catch
@ui.page("/login")
def login() -> Optional[RedirectResponse]:
    dark = ui.dark_mode()
    dark.enable()

    @logger.catch
    def try_login() -> (
        None
    ):  # local function to avoid passing username and password as arguments
        # Token-based login: POST credentials to TOKEN_AUTH_PATH and save returned token
        user = username.value
        pw = password.value

        global rqst, LOGIN_SUCCESS, LOGGED_IN_USER
        logger.debug(f"Attempting token auth login for user: {user}")

        # Use a session so we can configure SSL verification or CA bundle
        # to handle corporate SSL proxies. By default requests will respect
        # HTTP(S)_PROXY environment variables; to provide a custom CA bundle
        # set `UPLOADER_CACERT=/path/to/ca_bundle.pem`. To disable verification
        # (NOT RECOMMENDED) set `UPLOADER_SKIP_SSL_VERIFY=1`.
        session = requests.Session()
        # honor explicit CA bundle path
        ca_bundle = os.environ.get("UPLOADER_CACERT")
        if ca_bundle:
            session.verify = ca_bundle
        elif os.environ.get("UPLOADER_SKIP_SSL_VERIFY", "").lower() in ("1", "true", "yes", "y"):
            session.verify = False

        try:
            resp = session.post(
                f"{cfg.BASE_SITE_URL}{TOKEN_AUTH_PATH}",
                json={"username": user, "password": pw},
                timeout=10,
            )
        except requests.exceptions.SSLError as e:
            logger.debug(f"Token auth SSL error: {e}")
            logger.error(
                "SSL error when connecting to token endpoint; if you're behind a corporate proxy, "
                "set UPLOADER_CACERT to your proxy CA bundle or enable 'Allow insecure connections' in Settings to disable verification (not recommended)."
            )
            ui.notify("SSL connection error (proxy?). Redirecting to Settings...", color="negative")
            try:
                # Navigate user to main page where Settings expansion lives and
                # attempt to scroll to/open it for quick access.
                ui.navigate.to("/")
                ui.run_javascript(
                    """
                    (function(){
                        // Find an element with text 'Settings' and try to open/scroll it
                        const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_ELEMENT, null, false);
                        let node;
                        while(node = walker.nextNode()){
                            try{
                                if(node.innerText && node.innerText.trim().startsWith('Settings')){
                                    node.scrollIntoView({behavior:'smooth', block:'center'});
                                    try{ node.click(); } catch(e){}
                                    break;
                                }
                            } catch(e){}
                        }
                    })();
                    """,
                )
            except Exception:
                logger.debug("Failed to navigate to settings after SSL error")
            return
        except requests.exceptions.RequestException as e:
            logger.debug(f"Token auth request failed: {e}")
            ui.notify("Connection error!", color="negative")
            return

        if resp.status_code in (200, 201):
            try:
                data = resp.json()
                token = data.get("token") or data.get("access_token")
                if token:
                    save_api_token(token)
                    LOGIN_SUCCESS = True
                    LOGGED_IN_USER = user
                    maybe = user_info_ui.refresh()
                    _ensure_run(maybe)
                    ui.notify("Logged in (token)!", color="positive")
                    ui.navigate.to("/")
                    return
                else:
                    logger.debug(f"Token endpoint returned no token: {data}")
            except Exception as e:
                logger.debug(f"Failed to parse token response: {e}")
        else:
            logger.debug(f"Token auth failed: {resp.status_code} {resp.text}")
            ui.notify("Wrong username or password", color="negative")

    # if app.storage.user.get('authenticated', False):
    #    return RedirectResponse('/')
    with ui.card().classes("absolute-center"):
        username = ui.input("Username").on("keydown.enter", try_login)
        password = ui.input("Password", password=True, password_toggle_button=True).on(
            "keydown.enter", try_login
        )
        with ui.row():
            ui.button("Log in", on_click=try_login)
            ui.button("Cancel", on_click=lambda: ui.navigate.to("/"), color="red")
    return None


@ui.page("/")
async def main_page():
    async def watch_for_shutdown():
        global SHUTDOWN
        while not app.is_stopped:
            if SHUTDOWN:
                logger.debug("Trigger app shutdown")
                app.shutdown()
            await asyncio.sleep(1)

    asyncio.create_task(watch_for_shutdown())

    dark = ui.dark_mode()
    dark.enable()
    ui.page_title("Uploader tool")
    ui.label("Uploader tool").classes("text-h2")

    user_info_ui()

    login_button = ui.button("LOGIN", on_click=lambda: ui.navigate.to("/login"))
    login_button.bind_visibility_from(
        globals(), "LOGIN_SUCCESS", backward=lambda v: not v
    )

    updates = ui.list()

    loaded_series_ui()

    #    typer_app()
    # Create a queue to communicate with the heavy computation process
    queue = Manager().Queue()
    # Update the progress bar on the main process
    ui.timer(
        0.1,
        callback=lambda: progressbar.set_value(
            queue.get() if not queue.empty() else progressbar.value
        ),
    )

    # Create a queue to communicate with the heavy computation process
    upload_queue = Manager().Queue()
    # Update the progress bar on the main process
    ui.timer(
        0.1,
        callback=lambda: upload_progressbar.set_value(
            upload_queue.get() if not upload_queue.empty() else upload_progressbar.value
        ),
    )

    upload_progress = ui.row().classes("w-full place-content-center bg-blue-900")

    with upload_progress:
        ui.label("Uploading files")
        upload_progressbar = ui.linear_progress(value=0).props("instant-feedback")
    upload_progress.visible = False

    def upload_into_case():
        logger.debug("upload into case")
        if not rqst:
            ui.notify("Not logged in", color="negative")
            return
        try:
            resp = rqst.get(f"{cfg.BASE_SITE_URL}/api/atlas/get_cases_available")
            resp.raise_for_status()
            data = resp.json()
        except Exception as e:
            logger.error(e)
            ui.notify("Failed to load cases", color="negative")
            return

        # build options showing id and title and a mapping from label->id
        case_options_map = {}
        options = []
        for c in data:
            label = f"{c['id']} - {c['title']}"
            try:
                case_options_map[label] = int(c["id"])
            except Exception:
                case_options_map[label] = c["id"]
            options.append(label)

        with ui.dialog() as case_dialog:
            with ui.card().classes("absolute-center"):
                ui.label("Upload into Case").classes("text-h6")
                case_select_dialog = ui.select(
                    options, label="Select case (searchable)"
                ).classes("w-96")
                with ui.row():

                    def do_upload():
                        raw = case_select_dialog.value
                        if not raw:
                            ui.notify("No case selected", color="negative")
                            return

                        # Prefer a direct integer selection; otherwise map label->id
                        case_id_val = None
                        if isinstance(raw, int):
                            case_id_val = raw
                        else:
                            case_id_val = case_options_map.get(str(raw))

                        # final fallback: try to parse an integer from the selection
                        if case_id_val is None:
                            try:
                                case_id_val = int(str(raw).split("-", 1)[0].strip())
                            except Exception:
                                ui.notify("Invalid case selection", color="negative")
                                return

                        asyncio.create_task(
                            upload_files_start(
                                upload_progress, upload_queue, case_id_val
                            )
                        )
                        case_dialog.close()

                    ui.button("Upload", on_click=do_upload)
                    ui.button("Cancel", on_click=case_dialog.close, color="secondary")

        case_dialog.open()

    def clear_queue():
        global loaded_files, loaded_series, loaded_series_data
        global loaded_duplicate_series, loaded_duplicate_series_links

        clear_anonymized_files()
        loaded_files = {}
        loaded_series = defaultdict(list)
        loaded_series_data = {}
        loaded_duplicate_series = set()
        loaded_duplicate_series_links = defaultdict(set)

        _ensure_run(loaded_series_ui.refresh())
        _ensure_run(loaded_files_ui.refresh())
        ui.notify("Queue cleared", color="warning")

    with ui.dialog() as clear_queue_dialog:
        with ui.card().classes("w-96"):
            ui.label("Clear queue?").classes("text-h6")
            ui.label(
                "This will remove all queued anonymized files and clear loaded studies."
            )
            with ui.row().classes("items-center gap-2"):
                ui.button(
                    "Cancel", on_click=clear_queue_dialog.close, color="secondary"
                )
                ui.button(
                    "Clear",
                    on_click=lambda: (clear_queue_dialog.close(), clear_queue()),
                    color="negative",
                )

    with ui.row().classes("items-center gap-2 flex-wrap"):
        upload_button = ui.button(
            "Upload",
            on_click=lambda: asyncio.create_task(
                upload_files_start(upload_progress, upload_queue)
            ),
        )
        upload_button.bind_visibility_from(globals(), "LOGIN_SUCCESS")
        ui.button("Upload into Case", on_click=upload_into_case).bind_visibility_from(
            globals(), "LOGIN_SUCCESS"
        )
        ui.button(
            "Clear queue", on_click=clear_queue_dialog.open, color="warning"
        ).bind_visibility_from(globals(), "LOGIN_SUCCESS")

    anon_progress = ui.row().classes("w-full place-content-center bg-blue-900")

    with anon_progress:
        ui.label("Anonymizing files")
        progressbar = ui.linear_progress(value=0).props("instant-feedback")
    anon_progress.visible = False

    async def load_files_from_folder() -> None:
        folder = await local_folder_picker("~")
        logger.debug(folder)
        if folder is not None:
            ui.notify(f"Selected folder: {folder}")

            # Prompt user whether to copy or move files
            with ui.dialog() as copy_dialog:
                with ui.card().classes("absolute-center"):
                    ui.label("Load files from folder").classes("text-h6")
                    ui.label(f"Folder: {folder}")
                    copy_switch = ui.switch("Copy files instead of move", value=True)
                    with ui.row():

                        def do_load():
                            # schedule the load with the chosen copy flag
                            asyncio.create_task(
                                load_files_start(
                                    anon_progress, queue, folder, copy=copy_switch.value
                                )
                            )
                            copy_dialog.close()

                        ui.button("Load", on_click=do_load)
                        ui.button(
                            "Cancel", on_click=copy_dialog.close, color="secondary"
                        )

            # show the dialog we just created
            copy_dialog.open()
        else:
            ui.notify("No folder selected", color="negative")

    with ui.expansion("Extra", icon="build").classes("w-full"):
        with ui.row().classes("items-center gap-2 flex-wrap"):
            ui.button(
                "Load files", on_click=partial(load_files_start, anon_progress, queue)
            )
            ui.button("Load files from folder", on_click=load_files_from_folder)
            ui.button(
                "Reload anon",
                on_click=partial(reload_anonymized_start, anon_progress, queue),
            )

    with (
        ui.expansion("File status", icon="view_list")
        .classes("w-full")
        .props("id=file-status-section")
    ):
        loaded_files_ui()
        uploaded_files_ui()

        # with ui.expansion('Processed files!', icon='work').classes('w-full'):
        #    processed_files = ui.list()

    with ui.expansion("Settings", icon="settings").classes("w-full"):
        ui.label("Settings").classes("text-h4")

        def open_path(path: Path):
            try:
                system = platform.system()
                if system == "Windows":
                    subprocess.Popen(["explorer", str(path)])
                elif system == "Darwin":
                    subprocess.Popen(["open", str(path)])
                else:
                    # assume linux
                    subprocess.Popen(["xdg-open", str(path)])
                ui.notify(f"Opened {path}", color="positive")
            except Exception as e:
                logger.error(e)
                ui.notify(f"Failed to open {path}", color="negative")

        with ui.row():
            ui.label(f"Export path: {EXPORT_PATH}")
            ui.button("Open", on_click=lambda: open_path(EXPORT_PATH))

        with ui.row():
            ui.label(f"Process path: {PROCESS_PATH}")
            ui.button("Open", on_click=lambda: open_path(PROCESS_PATH))

        with ui.row():
            ui.label(f"Anon path: {ANON_PATH}")
            ui.button("Open", on_click=lambda: open_path(ANON_PATH))
        with ui.row():
            ui.label("Base site URL:")
            # Radio selection for prod/dev/custom
            site_choice = ui.radio(
                {
                    "production": f"Production ({cfg.PROD_BASE_SITE_URL})",
                    "development": f"Development ({cfg.DEV_BASE_SITE_URL})",
                    "custom": "Custom",
                },
                value=("production") if cfg.BASE_SITE_URL == cfg.PROD_BASE_SITE_URL else ("development" if cfg.BASE_SITE_URL == cfg.DEV_BASE_SITE_URL else "custom"),
            )

        with ui.row():
            custom_url_input = ui.input("Custom base URL", value=(cfg.BASE_SITE_URL if cfg.BASE_SITE_URL not in (cfg.PROD_BASE_SITE_URL, cfg.DEV_BASE_SITE_URL) else ""))
            save_url_btn = ui.button("Save URL")

        def on_site_choice_change(e=None):
            v = site_choice.value
            if v == "production":
                cfg.set_base_site_url(cfg.PROD_BASE_SITE_URL)
                custom_url_input.value = ""
                custom_url_input.visible = False
                ui.notify(f"Base site set to Production: {cfg.PROD_BASE_SITE_URL}", color="positive")
            elif v == "development":
                cfg.set_base_site_url(cfg.DEV_BASE_SITE_URL)
                custom_url_input.value = ""
                custom_url_input.visible = False
                ui.notify(f"Base site set to Development: {cfg.DEV_BASE_SITE_URL}", color="positive")
            else:
                custom_url_input.visible = True

        site_choice.on("click", on_site_choice_change)

        def save_custom_url():
            val = custom_url_input.value.strip()
            if not val:
                ui.notify("Custom URL cannot be empty", color="negative")
                return
            cfg.set_base_site_url(val)
            ui.notify(f"Custom base site URL set to {val}", color="positive")

        save_url_btn.on("click", lambda e=None: save_custom_url())

        # hide custom input unless 'custom' is selected
        custom_url_input.visible = site_choice.value == "custom"
        with ui.row():
            open_prod_switch = ui.switch(
                "Open external links on production site", value=False
            )

        with ui.row():
            # initialize skip-verify switch based on env var
            skip_verify_initial = os.environ.get("UPLOADER_SKIP_SSL_VERIFY", "").lower() in ("1", "true", "yes", "y")

            if skip_verify_initial:
                logger.debug("SSL verification is initially disabled based on environment variable")
                enable_skip_verify()

            skip_verify_switch = ui.switch("Allow insecure connections (disable SSL verification)", value=skip_verify_initial)

            def on_skip_verify_change(e=None):
                v = skip_verify_switch.value
                if v:
                    os.environ["UPLOADER_SKIP_SSL_VERIFY"] = "1"
                    enable_skip_verify()
                    ui.notify("Insecure connections enabled (verification disabled)", color="warning")
                else:
                    os.environ.pop("UPLOADER_SKIP_SSL_VERIFY", None)
                    disable_skip_verify()
                    ui.notify("Insecure connections disabled (verification enabled)", color="positive")

            # use 'click' event to catch toggle changes reliably
            skip_verify_switch.on("click", on_skip_verify_change)

            def set_open_prod(v=open_prod_switch):
                global OPEN_LINKS_PROD
                OPEN_LINKS_PROD = v.value

            open_prod_switch.on("click", set_open_prod)
            # API token management
            with ui.row():
                token_input = ui.input("API token (paste here)")

                def save_token_from_input():
                    t = token_input.value
                    if not t:
                        ui.notify("No token provided", color="negative")
                        return
                    save_api_token(t)
                    ui.notify("API token saved", color="positive")

                def clear_token_from_input():
                    token_input.value = ""
                    clear_api_token()
                    ui.notify("API token cleared", color="positive")

                ui.button("Save token", on_click=save_token_from_input)
                ui.button(
                    "Clear token", on_click=clear_token_from_input, color="secondary"
                )
                # populate input from stored token (first 8 chars shown)
                existing = load_api_token()
                if existing:
                    token_input.value = existing[:8] + "..."
        ui.button("Clear anon path", on_click=clear_anonymized_files).tooltip(
            "Delete all files in ANON_PATH"
        )

    def shutdown_app():
        app.shutdown()
        ui.run_javascript("window.close()")

    async def watch_for_refresh():
        global REFRESH_TRIGGERED
        while not app.is_stopped:
            if REFRESH_TRIGGERED:
                REFRESH_TRIGGERED = False
                await load_files_start(anon_progress, queue)
            await asyncio.sleep(1)

    asyncio.create_task(watch_for_refresh())

    with ui.dialog() as about_dialog:
        with ui.card().classes("absolute-center"):
            ui.label("Uploader tool").classes("text-h2")
            ui.label("A tool to help upload files to penracourses.org.uk")
            ui.image("icon/icon1.png")
            ui.label("Version: 0.1")
            ui.label("Author: Ross Kruger")

            ui.button(
                "Close", on_click=about_dialog.close, icon="close", color="secondary"
            )

    with ui.footer():
        with ui.row():
            ui.button("Shutdown", on_click=shutdown_app).props("outline color=white")
            ui.button("About", on_click=about_dialog.open).props("outline color=white")


@logger.catch
def launch_app(
    work_dir: Path = typer.Option(WORK_DIR, help="Working directory"),
    nng: bool = typer.Option(True, help="Use nng"),
    native_mode: bool = typer.Option(False, help="Use native mode"),
    debug: bool = typer.Option(False, help="Enable debug mode"),
    base_site_url: str = typer.Option(None, help="Base site URL to use (overrides default)"),
    verbose: int = typer.Option(
        0, "--verbose", "-v", help="Verbosity level (0=warning,1=info,2=debug, 3+=trace)"
    ),
    autoselect_nng_port: bool = typer.Option(
        False,
        help="If the configured nng port is in use, auto-select the next free port",
    ),
    autoselect_nng_maxtries: int = typer.Option(
        100, help="How many ports to probe when auto-selecting"
    ),
    allow_insecure_retry: bool = typer.Option(
        False, help="If an SSL error occurs, retry the request with verification disabled"
    ),
):
    global WORK_DIR, EXPORT_PATH, PROCESS_PATH, ANON_PATH

    logger.debug(f"launch_app called with work_dir={work_dir}, nng={nng}, native_mode={native_mode}, debug={debug}, base_site_url={base_site_url}, verbose={verbose}, autoselect_nng_port={autoselect_nng_port}, autoselect_nng_maxtries={autoselect_nng_maxtries}, allow_insecure_retry={allow_insecure_retry}")

    WORK_DIR = work_dir
    EXPORT_PATH = WORK_DIR / Path("export/")
    PROCESS_PATH = WORK_DIR / Path("to_process/")
    ANON_PATH = WORK_DIR / Path("anon/")

    # initialize config from CLI args and debug flag
    cfg.init_from_cli(base_site_url, debug)

    # allow overriding DEBUG and switch to test endpoints/paths when requested
    if debug:
        WORK_DIR = Path("./test/work")
        cfg.BASE_SITE_URL = "http://localhost:8080"
        EXPORT_PATH = Path("./test/export")
        PROCESS_PATH = Path("./test/to_process")
        ANON_PATH = Path("./test/anon")
        verbose = 2  # force debug logging in debug mode

    EXPORT_PATH.mkdir(exist_ok=True, parents=True)
    PROCESS_PATH.mkdir(exist_ok=True, parents=True)
    ANON_PATH.mkdir(exist_ok=True, parents=True)

    # initialize anonymizer now that ANON_PATH is final
    global anonymizer
    try:
        anonymizer = Anonymizer(ANON_PATH)
        logger.debug("Anonymizer initialized successfully")
    except Exception as e:
        logger.error(f"Failed to initialize Anonymizer: {e}")

    logger.debug(f"Work dir: {WORK_DIR}")


    # configure logging verbosity
    logger.remove()
    if verbose >= 3:
        log_level = "TRACE"
    elif verbose == 2:
        log_level = "DEBUG"
    elif verbose == 1:
        log_level = "INFO"
    else:
        log_level = "WARNING"

    def setup_logging(level: str):
        # If running as a PyInstaller frozen executable on Windows (no console),
        # Prefer writing to an AppData-based file when frozen on Windows.
        is_frozen = getattr(sys, "frozen", False)
        system = platform.system().lower()

        # Candidate directories to try for log file (Windows frozen: prefer APPDATA/LOCALAPPDATA)
        candidates = []
        if is_frozen and system.startswith("win"):
            appdata = os.getenv("APPDATA")
            localapp = os.getenv("LOCALAPPDATA")
            if appdata:
                candidates.append(Path(appdata) / APP_NAME)
            if localapp:
                candidates.append(Path(localapp) / APP_NAME)
            # Try typical fallback locations under the user profile
            home = Path.home()
            candidates.append(home / "AppData" / "Roaming" / APP_NAME)
            candidates.append(home / "AppData" / "Local" / APP_NAME)

        # Always include cwd and tmp as fallbacks
        candidates.append(Path.cwd())
        candidates.append(Path(os.getenv("TMP", "/tmp")))

        chosen = None
        for d in candidates:
            try:
                d.mkdir(parents=True, exist_ok=True)
                test_file = d / "uploader.log"
                # try opening for append to ensure writable
                with open(test_file, "a"):
                    pass
                chosen = test_file
                break
            except Exception:
                continue

        if chosen:
            logger.add(
                str(chosen),
                level=level,
                rotation="10 MB",
                retention="7 days",
                enqueue=True,
                backtrace=True,
                diagnose=False,
            )
        logger.add(sys.stderr, level=level, enqueue=True)

        logger.debug(f"Logging initialized at {level} level")

    setup_logging(log_level)

    # ensure paths exist

    # allow insecure retry behavior to be controlled via CLI or env var
    global ALLOW_INSECURE_RETRY
    ALLOW_INSECURE_RETRY = allow_insecure_retry or os.environ.get("UPLOADER_ALLOW_INSECURE_RETRY", "").lower() in ("1", "true", "yes", "y")
    # Configure global requests behavior for CA bundle or disabling verification
    ca_bundle = os.environ.get("UPLOADER_CACERT")
    if ca_bundle:
        os.environ.setdefault("REQUESTS_CA_BUNDLE", ca_bundle)

    # If verification is disabled, enable skip-verify globally
    skip_verify = os.environ.get("UPLOADER_SKIP_SSL_VERIFY", "").lower() in ("1", "true", "yes", "y")
    if skip_verify:
        enable_skip_verify()

    logger.debug(f"NNG enabled: {nng}")
    if nng:
        # Attempt to bind the NNG listening socket. If it's already in use we can
        # optionally autoselect the next free TCP port and retry.
        # record autoselect flag globally for other code to reference
        global AUTSELECT_NNG_PORT, SOCKET_PATH
        AUTSELECT_NNG_PORT = autoselect_nng_port
        logger.debug(f"Attempting to bind NNG socket at {SOCKET_PATH} (autoselect={AUTSELECT_NNG_PORT})")
        bound = False
        attempt_count = 0
        while not bound:
            logger.debug(f"Binding attempt {attempt_count} to {SOCKET_PATH}")
            try:
                with pynng.Pair0(listen=SOCKET_PATH) as sub:
                    logger.debug(f"Successfully bound NNG socket at {SOCKET_PATH}")
                    bound = True
            except pynng.exceptions.AddressInUse:
                logger.debug("Address in use")
                # contact owner but do not block forever; suppress terminate if autoselect requested
                try:
                    asyncio.run(contact_socket_owner(SOCKET_PATH, timeout=10, allow_terminate=(not AUTSELECT_NNG_PORT)))
                except Exception:
                    logger.debug("Failed to contact socket owner from sync context")

                if autoselect_nng_port:
                    # parse the numeric port out of SOCKET_PATH and try to find a free one
                    try:
                        current_port = int(SOCKET_PATH.split(":")[-1])
                    except Exception:
                        current_port = None
                    if current_port is None:
                        logger.error("Could not parse numeric port from SOCKET_PATH")
                        sys.exit(1)

                    attempt_count += 1
                    new_port = find_free_tcp_port(
                        current_port + attempt_count, max_tries=autoselect_nng_maxtries
                    )
                    if new_port:
                        old = SOCKET_PATH
                        SOCKET_PATH = f"tcp://localhost:{new_port}"
                        logger.info(
                            f"Autoselected new NNG socket port: {SOCKET_PATH} (was {old})"
                        )
                        # loop and try to bind to the new SOCKET_PATH
                        continue
                    else:
                        logger.error("Autoselect requested but no free port found")
                        sys.exit(1)
                else:
                    sys.exit(0)

        app.on_startup(read_nng_messages)


    # Try to load saved API token at startup
    existing_token = load_api_token()
    if existing_token:
        logger.debug("Loaded API token from storage; validating...")
        info = check_token()
        if info and info.get("valid"):
            logger.debug(f"Stored API token is valid for user: {info.get('username')}")
            #LOGIN_SUCCESS = True
            #LOGGED_IN_USER = info.get("username") or "API token"
        else:
            logger.debug("Stored API token invalid; clearing")
            try:
                clear_api_token()
            except Exception:
                logger.error("Failed to clear invalid API token from storage")
    #app.on_exception(logger.debug)

    try:
        ui.run(
            reload=False,
            show=True,
            port=native.find_open_port(),
            native=native_mode,
            favicon="icon/icon1.ico",
        )
    except Exception as e:
        logger.error(e)
        pass


if __name__ == "__main__":
    freeze_support()

    typer.run(launch_app)
