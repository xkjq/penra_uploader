#!/usr/bin/env python3
import asyncio
import pynng

# asyncio.set_event_loop_policy(asyncio.WindowsSelectorEventLoopPolicy())
from collections import defaultdict
from pathlib import Path

from bs4 import BeautifulSoup

from nicegui import ui, app, html, run, Client, native

from datetime import datetime

from loguru import logger
import sys

from anonymiser import Anonymizer
import pydicom

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

LOGIN_URL = f"{BASE_SITE_URL}/accounts/login/"

LOGIN_SUCCESS = False

LOGGED_IN_USER = "None"


REFRESH_TRIGGERED = True
SHUTDOWN = False

DELETE_FILES_ON_UPLOAD = True

DEBUG = False


if DEBUG:
    BASE_SITE_URL = "http://localhost:8000"
    EXPORT_PATH = Path("./test/export")
    PROCESS_PATH = Path("./test/to_process")
    ANON_PATH = Path("./test/anon")

rqst = None


loaded_files: dict = {}
loaded_series = defaultdict(list)
loaded_series_data = {}
uploaded_files: dict = {}
duplicate_series = set()


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


anonymizer = Anonymizer(ANON_PATH)


async def upload_files_start(progress, upload_queue):
    global uploaded_files, duplicate_series
    progress.visible = True

    results = await run.cpu_bound(upload_files, upload_queue, rqst)
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
        ui.notify("No files to upload", color="negative")
        return

    uploaded_files.update(new_uploaded_files)

    ui.notify(f"Uploaded {len(upload_file_list)} files", color="positive")

    if duplicate_file_list:
        ui.notify(f"Duplicate {len(duplicate_file_list)} files", color="warning")

    if failed:
        ui.notify(f"Failed {len(failed)} files", color="negative")

    if DELETE_FILES_ON_UPLOAD:
        clear_anonymized_files()

    loaded_series_ui.refresh()
    loaded_files_ui.refresh()
    uploaded_files_ui.refresh()


async def load_files_start(progress_bar, queue, custom_path=None):
    logger.debug("load files start")
    global loaded_files, loaded_series, loaded_series_data
    progress_bar.visible = True

    new_loaded_files, new_loaded_series, new_loaded_series_data = await run.cpu_bound(
        load_files, queue, custom_path
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
                ui.item(
                    f"Duplicate series: {series}",
                    on_click=lambda: ui.navigate.to(
                        f"{BASE_SITE_URL}{series}", new_tab=True
                    ),
                ).classes("text-red-500")
        with ui.list():
            for file, data in uploaded_files.items():
                ui.item(f"{file} - {data}")


def load_files(q: Queue, src_path: Path | None = None):
    loaded_files: dict = {}
    loaded_series = defaultdict(list)
    loaded_series_data = {}
    # Move files
    if src_path is None:
        src_path = EXPORT_PATH

    to_process = []
    for file in src_path.glob("**/*.dcm"):
        logger.debug(f"Moving {file} to {PROCESS_PATH}")

        file_to_process = PROCESS_PATH / file.name
        file.rename(file_to_process)

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


def clear_anonymized_files():
    for file in ANON_PATH.iterdir():
        if file.is_file():
            file.unlink()

    logger.debug("Deleted all files in ANON_PATH")

    return


def upload_files(q, rqst):
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
            rqst.headers["X-CSRFToken"] = rqst.cookies["csrftoken"]
            # print(self.rqst.headers)
            # print(self.rqst.cookies)
            resp = rqst.post(
                f"{BASE_SITE_URL}/api/atlas/upload_dicom",
                # data=data,
                files=files,
            )

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
        # Because I haven't gotten around to implementing a proper login system yet
        # we just log in via requests and store teh session in the global rqst variable
        user = username.value
        pw = password.value

        global rqst, LOGIN_SUCCESS, LOGGED_IN_USER
        rqst = requests.session()
        try:
            rsp = rqst.get(LOGIN_URL)
        except requests.exceptions.ConnectionError:
            ui.notify("Connection error!", color="negative")
            return

        token = (
            BeautifulSoup(rsp.content, "html.parser").find("input").attrs["value"]
        )  # , attr={"name": "csrfmiddlewaretoken"}).attrs("value")

        # token = rsp.cookies["csrftoken"]
        # header = {"X-CSRFToken": token}
        # cookies = {"csrftoken": token}

        data = {
            "username": user,
            "password": pw,
            "csrfmiddlewaretoken": token,
            "next": "/",
        }

        rqst.headers["Referer"] = LOGIN_URL

        rsp = rqst.post(
            LOGIN_URL,
            data=data,
            # headers=header,
            # cookies=cookies
        )

        print(rsp.content)

        soup = BeautifulSoup(rsp.content, "html.parser")

        if soup.find("button") and soup.find("button").find(string="Login"):
            print("login fail")
            pass
        else:
            print("login success")
            LOGIN_SUCCESS = True
            LOGGED_IN_USER = user
            user_info_ui.refresh()
            # user_label.clear()
            # user_label.text = f"User: {user}"
        # print(rqst.headers)

        rqst.headers["X-CSRFToken"] = token

        if LOGIN_SUCCESS:
            ui.notify("Logged in!", color="positive")
            ui.navigate.to("/")
        else:
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

    upload_button = ui.button(
        "Upload", on_click=partial(upload_files_start, upload_progress, upload_queue)
    )
    upload_button.bind_visibility_from(globals(), "LOGIN_SUCCESS")

    anon_progress = ui.row().classes("w-full place-content-center bg-blue-900")

    with anon_progress:
        ui.label("Anonymizing files")
        progressbar = ui.linear_progress(value=0).props("instant-feedback")
    anon_progress.visible = False

    async def load_files_from_folder() -> None:
        folder = await local_folder_picker("~")
        logger.error(folder)
        if folder is not None:
            ui.notify(f"Selected folder: {folder}")
            await load_files_start(anon_progress, queue, folder)
            # return dir
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
        ui.label(f"Export path: {EXPORT_PATH}")
        ui.label(f"Process path: {PROCESS_PATH}")
        ui.label(f"Anon path: {ANON_PATH}")
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
):
    global WORK_DIR, EXPORT_PATH, PROCESS_PATH, ANON_PATH

    WORK_DIR = work_dir
    EXPORT_PATH = WORK_DIR / Path("export/")
    PROCESS_PATH = WORK_DIR / Path("to_process/")
    ANON_PATH = WORK_DIR / Path("anon/")

    logger.debug(f"Work dir: {WORK_DIR}")

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
