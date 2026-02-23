#!/usr/bin/env python3
from PyInstaller.log import DEBUG
import asyncio
import pynng

# asyncio.set_event_loop_policy(asyncio.WindowsSelectorEventLoopPolicy())
from collections import defaultdict
from pathlib import Path

from nicegui import ui, app, html, run, Client, native

from datetime import datetime

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

from multiprocessing import Manager, Queue, freeze_support

from functools import partial

from local_file_picker import local_file_picker
from local_folder_picker import local_folder_picker

from typing import Optional

from fastapi import Request
from fastapi.responses import RedirectResponse
from starlette.middleware.base import BaseHTTPMiddleware
import typer

SOCKET_PATH = "tcp://localhost:9976"

WORK_DIR = Path("C:/uploader")

EXPORT_PATH = WORK_DIR / Path("export/")
PROCESS_PATH = WORK_DIR / Path("to_process/")
ANON_PATH = WORK_DIR / Path("anon/")

EXPORT_PATH.mkdir(exist_ok=True, parents=True)
PROCESS_PATH.mkdir(exist_ok=True, parents=True)
ANON_PATH.mkdir(exist_ok=True, parents=True)

BASE_SITE_URL = "https://www.penracourses.org.uk"
TOKEN_AUTH_PATH = "/api/atlas/create_api_token"  # POST {username,password} -> {token: '...'}
TOKEN_CHECK_PATH = "/api/atlas/token_check"
PROD_BASE_SITE_URL = "https://www.penracourses.org.uk"

LOGIN_URL = f"{BASE_SITE_URL}/accounts/login/"

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


def token_file_path() -> Path:
    home = Path.home()
    cfg = home / ".uploader"
    cfg.mkdir(parents=True, exist_ok=True)
    return cfg / "api_token"


def save_api_token(token: str) -> None:
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

    return token


def check_token() -> dict | None:
    """Call the token_check endpoint and return parsed JSON or None on error."""
    global rqst
    try:
        # Prefer header auth if session exists
        if rqst and "Authorization" in rqst.headers:
            resp = rqst.post(f"{BASE_SITE_URL}{TOKEN_CHECK_PATH}")
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
            resp = requests.post(f"{BASE_SITE_URL}{TOKEN_CHECK_PATH}", json={"token": token}, timeout=5)

        if resp.status_code != 200:
            return None
        data = resp.json()
        return data
    except Exception as e:
        logger.debug(f"token_check failed: {e}")
        return None


def clear_api_token() -> None:
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
uploaded_files: dict = {}
duplicate_series = set()
OPEN_LINKS_PROD = False


def human_size(num, suffix="B"):
    # Convert bytes to human-readable string (KiB, MiB, ...)
    try:
        n = float(num)
    except Exception:
        return "0 B"
    for unit in ["","Ki","Mi","Gi","Ti","Pi"]:
        if abs(n) < 1024.0:
            return f"{n:3.1f} {unit}{suffix}"
        n /= 1024.0
    return f"{n:.1f} Pi{suffix}"


async def read_nng_messages():
    print("start nng")
    try:
        with pynng.Pair0(listen=SOCKET_PATH) as sub:
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
        with pynng.Pair0(dial=SOCKET_PATH) as sub:
            await sub.asend(b"loaded")

        # Trigger shutdown from in the async loop
        # This shouldn't be necessary
        global SHUTDOWN
        SHUTDOWN = True
        return

    # asyncio.create_task(read_nng_messages())


# anonymizer will be initialized at startup inside `launch_app` so debug
# mode can override the paths before the object is created.
anonymizer = None


async def upload_files_start(progress, upload_queue, case_id=None):
    global uploaded_files, duplicate_series
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
        if not client.has_socket_connection:
            continue
        with client:
            ui.notify(f"Uploaded {len(upload_file_list)} files", color="positive")

            if duplicate_file_list:
                ui.notify(f"Duplicate {len(duplicate_file_list)} files", color="warning")

            if failed:
                ui.notify(f"Failed {len(failed)} files", color="negative")

    if DELETE_FILES_ON_UPLOAD:
        # clear only files that were part of the successful upload or marked duplicate
        files_to_clear = []
        files_to_clear.extend(upload_file_list or [])
        files_to_clear.extend(duplicate_file_list or [])
        clear_anonymized_files(files_to_clear)

    loaded_series_ui.refresh()
    loaded_files_ui.refresh()
    uploaded_files_ui.refresh()


async def load_files_start(progress_bar, queue, custom_path=None, copy=False):
    logger.debug("load files start")
    global loaded_files, loaded_series, loaded_series_data
    progress_bar.visible = True

    new_loaded_files, new_loaded_series, new_loaded_series_data = await run.cpu_bound(
        load_files, queue, custom_path, copy
    )

    for client in Client.instances.values():
        if not client.has_socket_connection:
            continue
        with client:
            ui.notify(f"Files loaded: {len(new_loaded_files)}, [Series: {len(new_loaded_series)}] ", color="positive")

    loaded_files.update(new_loaded_files)
    loaded_series.update(new_loaded_series)
    loaded_series_data.update(new_loaded_series_data)

    progress_bar.visible = False
    # load_series_view(loaded_series, loaded_series_data)
    loaded_series_ui.refresh()
    loaded_files_ui.refresh()


async def reload_anonymized_start(progress_bar, queue):
    global loaded_files, loaded_series, loaded_series_data
    progress_bar.visible = True
    loaded_files, loaded_series, loaded_series_data = await run.cpu_bound(
        reload_anonymized, queue
    )
    progress_bar.visible = False
    for client in Client.instances.values():
        if not client.has_socket_connection:
            continue
        with client:
            ui.notify(f"Files reloaded: {len(loaded_files)}, [Series: {len(loaded_series)}] ", color="positive")
    # load_series_view(loaded_series, loaded_series_data)
    loaded_series_ui.refresh()
    loaded_files_ui.refresh()


@ui.refreshable
def user_info_ui():
    global LOGGED_IN_USER
    user_label = ui.label(f"User: {LOGGED_IN_USER}")
    # user_label.bind_visibility_from(globals(), "LOGIN_SUCCESS")


@ui.refreshable
def loaded_series_ui() -> None:
    global loaded_series, loaded_series_data

    series_title = ui.label("Series to upload").classes("text-h3")
    series_title.visible = False

    series = ui.row()

    # def load_series_view(loaded_series, loaded_series_data):
    logger.debug("load series view")
    logger.debug(loaded_series)
    for key in loaded_series:
        logger.debug(f"load series: {key}")
        with series:
            with ui.card():
                ui.label(loaded_series_data[key][0])
                ui.label(loaded_series_data[key][1])
                ui.label(f"Images: {len(loaded_series[key])}")
                # compute total size for the series
                total = 0
                for p in loaded_series[key]:
                    try:
                        total += Path(p).stat().st_size
                    except Exception:
                        # ignore missing files or non-path entries
                        pass
                ui.label(f"Size: {human_size(total)}")

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
    with ui.expansion("Uploaded files", icon="file_upload").classes("w-full"):
        ui.label(f"Items: {len(uploaded_files)}")
        with ui.list():
            for series in duplicate_series:
                def _open_series(s=series):
                    base = PROD_BASE_SITE_URL if OPEN_LINKS_PROD else BASE_SITE_URL
                    ui.navigate.to(f"{base}{s}", new_tab=True)

                ui.item(f"Duplicate series: {series}", on_click=_open_series).classes("text-red-500")
        with ui.list():
            for file, data in uploaded_files.items():
                ui.item(f"{file} - {data}")


def load_files(q: Queue, src_path: Path | None = None, copy: bool = False):
    loaded_files: dict = {}
    loaded_series = defaultdict(list)
    loaded_series_data = {}
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

        q.put_nowait((n + 1) / to_process_len)

        # time.sleep(0.01)

    return loaded_files, loaded_series, loaded_series_data


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

        logger.debug(f"Deleted {removed} files from ANON_PATH (of {len(names)} requested)")
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

    files_to_upload = []
    for file in ANON_PATH.iterdir():
        files_to_upload.append(("files", open(str(file), "rb")))

    if not files_to_upload:
        return None

    # chunck files
    n = 10
    chunked_files = [
        files_to_upload[i : i + n] for i in range(0, len(files_to_upload), n)
    ]

    upload_file_list = []
    duplicate_file_list = []
    failed = []
    duplicate_series = set()

    logger.debug("START UPLOAD")
    logger.debug(files_to_upload)

    to_process_len = len(chunked_files)

    for n, files in enumerate(chunked_files):

        def upload_files_(files):
            # If we're using token auth, don't set CSRF header
            if "Authorization" not in rqst.headers:
                rqst.headers["X-CSRFToken"] = rqst.cookies.get("csrftoken", "")

            # choose endpoint based on whether a case_id was supplied
            if case_id:
                endpoint = f"{BASE_SITE_URL}/api/atlas/upload_dicom_case/{case_id}"
            else:
                endpoint = f"{BASE_SITE_URL}/api/atlas/upload_dicom"

            resp = rqst.post(endpoint, files=files)

            logger.debug(f"Endpoint: {endpoint}")
            logger.debug(f"Resp: {resp}")
            logger.debug(f"{resp.content}")
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
    print(upload_file_list)
    print("dup", duplicate_file_list)
    print("failed", failed)

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





@ui.page("/login")
def login() -> Optional[RedirectResponse]:
    dark = ui.dark_mode()
    dark.enable()

    def try_login() -> (
        None
    ):  # local function to avoid passing username and password as arguments
        # Token-based login: POST credentials to TOKEN_AUTH_PATH and save returned token
        user = username.value
        pw = password.value

        global rqst, LOGIN_SUCCESS, LOGGED_IN_USER
        try:
            resp = requests.post(f"{BASE_SITE_URL}{TOKEN_AUTH_PATH}", json={"username": user, "password": pw}, timeout=10)
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
                    user_info_ui.refresh()
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
def main_page():
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

    upload_button = ui.button("Upload", on_click=lambda: asyncio.create_task(upload_files_start(upload_progress, upload_queue)))
    upload_button.bind_visibility_from(globals(), "LOGIN_SUCCESS")

    def upload_into_case():
        logger.debug("upload into case")
        if not rqst:
            ui.notify("Not logged in", color="negative")
            return
        try:
            resp = rqst.get(f"{BASE_SITE_URL}/api/atlas/get_cases_available")
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
                case_select_dialog = ui.select(options, label="Select case (searchable)").classes("w-96")
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

                        asyncio.create_task(upload_files_start(upload_progress, upload_queue, case_id_val))
                        case_dialog.close()

                    ui.button("Upload", on_click=do_upload)
                    ui.button("Cancel", on_click=case_dialog.close, color="secondary")

        case_dialog.open()

    ui.button("Upload into Case", on_click=upload_into_case).bind_visibility_from(globals(), "LOGIN_SUCCESS")

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
                                load_files_start(anon_progress, queue, folder, copy=copy_switch.value)
                            )
                            copy_dialog.close()

                        ui.button("Load", on_click=do_load)
                        ui.button("Cancel", on_click=copy_dialog.close, color="secondary")

            # show the dialog we just created
            copy_dialog.open()
        else:
            ui.notify("No folder selected", color="negative")

    with ui.expansion("Extra", icon="build").classes("w-full"):
        ui.button(
            "Load files", on_click=partial(load_files_start, anon_progress, queue)
        )
        ui.button("Load files from folder", on_click=load_files_from_folder)
        ui.button(
            "Reload anon",
            on_click=partial(reload_anonymized_start, anon_progress, queue),
        )

    with ui.expansion("File status", icon="view_list").classes("w-full"):
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
            open_prod_switch = ui.switch("Open external links on production site", value=False)
            def set_open_prod(v=open_prod_switch):
                global OPEN_LINKS_PROD
                OPEN_LINKS_PROD = v.value

            open_prod_switch.on('click', set_open_prod)
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
                ui.button("Clear token", on_click=clear_token_from_input, color="secondary")
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

            ui.button("Close", on_click=about_dialog.close, icon="close", color="secondary")
    
    with ui.footer():
        with ui.row():
            ui.button("Shutdown", on_click=shutdown_app).props("outline color=white")
            ui.button("About", on_click=about_dialog.open).props("outline color=white")


def launch_app(
    work_dir: Path = typer.Option(WORK_DIR, help="Working directory"),
    nng: bool = typer.Option(True, help="Use nng"),
    native_mode: bool = typer.Option(False, help="Use native mode"),
    debug: bool = typer.Option(False, help="Enable debug mode"),
    verbose: int = typer.Option(0, "--verbose", "-v", help="Verbosity level (0=warning,1=info,2=debug)"),
):
    global WORK_DIR, EXPORT_PATH, PROCESS_PATH, ANON_PATH

    global BASE_SITE_URL


    WORK_DIR = work_dir
    EXPORT_PATH = WORK_DIR / Path("export/")
    PROCESS_PATH = WORK_DIR / Path("to_process/")
    ANON_PATH = WORK_DIR / Path("anon/")

    # allow overriding DEBUG and switch to test endpoints/paths when requested
    if debug:
        WORK_DIR = Path("./test/work")
        BASE_SITE_URL = "http://localhost:8080"
        EXPORT_PATH = Path("./test/export")
        PROCESS_PATH = Path("./test/to_process")
        ANON_PATH = Path("./test/anon")
        verbose = 2  # force debug logging in debug mode

    # configure logging verbosity
    logger.remove()
    if verbose >= 2:
        log_level = "DEBUG"
    elif verbose == 1:
        log_level = "INFO"
    else:
        log_level = "WARNING"

    logger.add(sys.stderr, level=log_level)

    # ensure paths exist
    EXPORT_PATH.mkdir(exist_ok=True, parents=True)
    PROCESS_PATH.mkdir(exist_ok=True, parents=True)
    ANON_PATH.mkdir(exist_ok=True, parents=True)

    # initialize anonymizer now that ANON_PATH is final
    global anonymizer
    try:
        anonymizer = Anonymizer(ANON_PATH)
    except Exception as e:
        logger.error(f"Failed to initialize Anonymizer: {e}")

    logger.debug(f"Work dir: {WORK_DIR}")

    # Try to load saved API token at startup
    existing_token = load_api_token()
    if existing_token:
        logger.debug("Loaded API token from storage; validating...")
        info = check_token()
        if info and info.get("valid"):
            LOGIN_SUCCESS = True
            LOGGED_IN_USER = info.get("username") or "API token"
            try:
                user_info_ui.refresh()
            except Exception:
                pass
        else:
            logger.debug("Stored API token invalid; clearing")
            try:
                clear_api_token()
            except Exception:
                pass

    if nng:
        try:
            with pynng.Pair0(listen=SOCKET_PATH) as sub:
                pass
        except pynng.exceptions.AddressInUse:
            logger.debug("Address in use")
            with pynng.Pair0(dial=SOCKET_PATH) as sub:
                sub.send(b"loaded")
                msg = sub.recv()
                print(msg)
            sys.exit(0)

        app.on_startup(read_nng_messages)

    app.on_exception(logger.debug)

    try:
        ui.run(reload=False, show=True, port=native.find_open_port(), native=native_mode, favicon="icon/icon1.ico")
    except Exception as e:
        logger.error(e)
        pass


if __name__ in {"__main__", "__mp_main__"}:
    freeze_support()

    typer.run(launch_app)
