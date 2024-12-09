# This is a legacy app written in wxPython
# Whilst it works (or did prior to any API changes)
# it was not particularly well designed
from collections import defaultdict
import json
import os
import threading
from typing import Iterable
from bs4 import BeautifulSoup
import numpy as np
import requests
import webbrowser

# from blake3 import blake3

import zmq
from blake3 import blake3

# import time
#
# import sys
#
# from datauri import DataURI

from pathlib import Path

import pydicom
import pydicom.errors
import pydicom.encoders.gdcm
import pydicom.encoders.pylibjpeg
import dicognito.anonymizer
from six import create_bound_method

import typer

import time
import sys
import tempfile

# from typing import List
import wx
import wx.adv


from wxDicomViewer import BasicDicomViewer, ViewerFrame

# from watchdog.observers import Observer

# from watchdog.events import LoggingEventHandler
# from watchdog.events import PatternMatchingEventHandler

import hashlib

# import pywintypes
# import win32api
# import win32com


try:
    from agw import hyperlink as hl
    from agw import pygauge as PG
except ImportError:  # if it's not there locally, try the wxPython lib.
    import wx.lib.agw.hyperlink as hl
    import wx.lib.agw.pygauge as PG


import wx.lib.agw.thumbnailctrl as TC
import wx.lib.agw.ultimatelistctrl as ULC


from wx.lib.agw.scrolledthumbnail import (
    EVT_THUMBNAILS_SEL_CHANGED,
    EVT_THUMBNAILS_POINTED,
    EVT_THUMBNAILS_DCLICK,
)

import wx.lib.mixins.listctrl as listmix

import time
import datetime
import contextlib

import threading

# import watchdog.observers

__VERSION__ = "0.1"
__APP_NAME__ = "Uploader"

# Define notification event for thread completion
EVT_RESULT_ID = wx.NewId()

def EVT_RESULT(win, func):
    """Define Result Event."""
    win.Connect(-1, -1, EVT_RESULT_ID, func)

class ResultEvent(wx.PyEvent):
    """Simple event to carry arbitrary result data."""
    def __init__(self, data):
        """Init Result Event."""
        wx.PyEvent.__init__(self)
        self.SetEventType(EVT_RESULT_ID)
        self.data = data

# Thread class that executes processing
class WorkerThread(threading.Thread):
    """Worker Thread Class."""
    def __init__(self, notify_window):
        """Init Worker Thread Class."""
        threading.Thread.__init__(self)
        self._notify_window = notify_window
        self._want_abort = 0
        # This starts the thread running on creation, but you could
        # also make the GUI thread responsible for calling this
        self.start()

    def run(self):
        """Run Worker Thread."""
        # This is the code executing in the new thread. Simulation of
        # a long process (well, 10s here) as a simple loop - you will
        # need to structure your processing so that you periodically
        # peek at the abort variable
        for i in range(10):
            time.sleep(1)
            if self._want_abort:
                # Use a result of None to acknowledge the abort (of
                # course you can use whatever you'd like or even
                # a separate event type)
                wx.PostEvent(self._notify_window, ResultEvent(None))
                return
        # Here's where the result would be returned (this is an
        # example fixed result of the number 10, but it could be
        # any Python object)
        wx.PostEvent(self._notify_window, ResultEvent(10))

    def abort(self):
        """abort worker thread."""
        # Method for use by main thread to signal an abort
        self._want_abort = 1

def resource_path(relative_path):
    """ Get absolute path to resource, works for dev and for PyInstaller """
    try:
        # PyInstaller creates a temp folder and stores path in _MEIPASS
        base_path = sys._MEIPASS
    except Exception:
        base_path = os.path.abspath(".")

    return os.path.join(base_path, relative_path)

anonymizer = dicognito.anonymizer.Anonymizer()

# determine if application is a script file or frozen exe
if getattr(sys, "frozen", False):
    application_path = os.path.dirname(sys.executable)
elif __file__:
    application_path = os.path.dirname(__file__)

from loguru import logger

try:
    logger.add(
        sys.stderr, format="{time} {level} {message}", filter="my_module", level="DEBUG"
    )
except TypeError:
    pass
try:
    logger.add(os.path.join(application_path, "logs", "anon_gui_{time}.log"))
except TypeError:
    pass

logger.debug("Starting")

# import logging
# logging.basicConfig(filename=os.path.join(sys.path[0], f'anon_gui_{datetime.datetime.now().strftime("%Y-%m-%d--%H-%M-%S")}.log'), encoding='utf-8', level=logging.DEBUG)


def get_image_hash(img) -> (str, bool):
    is_dicom = False
    # Try and read the file as a dicom
    try:
        # and generate a hash from the pixel data
        # TODO: improve?
        dataset = pydicom.dcmread(img)
        # flatten = dataset.pixel_array.astype(str).flatten()
        # print("flatteded")
        # pre_join = ",".join(flatten)
        # print(pre_join)
        # hash = hashlib.md5(pre_join.encode()).hexdigest()
        # ----
        md5 = hashlib.md5()
        first = True
        for i in dataset.pixel_array.astype(str).flatten():
            if first:
                first = False
                md5.update(f"{i}".encode())
            else:
                md5.update(f",{i}".encode())

        hash = md5.hexdigest()
        is_dicom = True
        # ----

    except pydicom.errors.InvalidDicomError:
        try:  # This is horrible (but needed for current unit tests)
            # (we use a temporary file that breaks here)
            img.file.open()
            hash = hashlib.md5(img.read()).hexdigest()
        except AttributeError:
            return "12345ABCD", False

    return hash, is_dicom


app = typer.Typer(add_completion=False)


@app.command()
@logger.catch()
def annonymise(
    path: Path,
    output_dir: Path,
    run_gui: bool = True,
    file_list: bool = False,
    dev: bool = False,
    remove_files_on_annonymisation: bool = True,
):
    logger.debug(locals())
    if run_gui:  # Let the GUI handle it all
        launchGUI(
            input_dir=path,
            output_dir=output_dir,
            silent_fail=True,
            clear_files=False,
            clear_files_on_close=True,
            dev=dev,
            remove_files_on_annonymisation=remove_files_on_annonymisation,
        )
        return
    if path.is_dir():
        files = list(Path(path).glob("*.dcm"))
    elif path.is_file():
        if file_list:
            with open(path) as f:
                files = [Path(p) for p in f.read().splitlines()]
                # files = [path]
        else:
            files = [path]
    else:
        typer.echo("Invalid path")
        return

    # progress_dialog = wx.ProgressDialog("Annonymise files", "Removing data...", maximum=len(files), style=wx.PD_APP_MODAL|wx.PD_ELAPSED_TIME|wx.PD_ESTIMATED_TIME)
    for n, file in enumerate(files):
        # progress_dialog.Update(n)
        new_filepath: Path
        # Get next available filename
        i: int = 0
        while os.path.exists(new_filepath := output_dir.joinpath(f"IMG_{i:03}.dcm")):
            i += 1
            # new_filepath = Path(output_dir).joinpath(f"IMG_{i:03}.dcm")

        logger.debug(f"anon: {n}")
        anonymize_file(file, new_filepath)

    # progress_dialog.Destroy()

    return new_filepath


@logger.catch()
def anonymize_file(file: Path, new_filepath: Path, remove_original: bool = False):
    logger.debug(f"Annonymise file, in={file}, out={new_filepath}")
    # Ensure the output folder exists
    new_filepath.parents[0].mkdir(parents=True, exist_ok=True)
    # was this on windows
    # new_filepath.mkdir(parents=True, exist_ok=True)
    # with new_filepath.open() as f:
    #    f.write("", "wb")
    # new_filepath.touch()
    with pydicom.dcmread(file) as dataset:
        anonymizer.anonymize(dataset)
        dataset.save_as(new_filepath)

        if remove_original:
            os.remove(file)
        return dataset, new_filepath


def order_files_by_dicom_attribute(
    paths: Iterable[Path], dicom_attribute: str = "SliceLocation"
) -> Iterable[Path]:
    files = []

    for path in paths:
        dataset = pydicom.dcmread(path)
        files.append((path, dataset))

    try:
        sorted_datasets = sorted(
            files, key=lambda f: f[1][dicom_attribute].value, reverse=True
        )
    except KeyError:  # Happens if no slicelocation
        return paths
    return [d[0] for d in sorted_datasets]


def get_dicom_pixel_md5_hash(file: Path):
    try:
        with pydicom.dcmread(file) as dataset:
            a = ",".join([str(i) for i in dataset.pixel_array.flatten()])

            return hashlib.md5(a.encode()).hexdigest()
    except pydicom.errors.InvalidDicomError:
        logger.debug(f"Invalid dicom: {file}")
        with open(file) as f:
            return hashlib.md5(f.read()).hexdigest()


def get_file_hash(file: Path, hasher="blake3"):
    match hasher:
        case "blake3":
            file_hash = blake3()
        case "md5":
            file_hash = hashlib.md5()
        case _:
            raise Exception(f"{_} is not a valid hasher")

    with open(file, "rb") as f:
        while chunk := f.read(8192):
            file_hash.update(chunk)

    return file_hash.hexdigest()

    with open(file) as f:
        return hashlib.md5(f.read()).hexdigest()


# def get_dicom_blake_hash(file: Path)@


class LoginDialog(wx.Dialog):
    """
    Class to define login dialog
    """

    # ----------------------------------------------------------------------
    def __init__(self, parent):
        """Constructor"""
        wx.Dialog.__init__(self, None, title="Login")

        self.parent = parent

        # user info
        user_sizer = wx.BoxSizer(wx.HORIZONTAL)

        user_lbl = wx.StaticText(self, label="Username:")
        user_sizer.Add(user_lbl, 0, wx.ALL | wx.CENTER, 5)
        self.user = wx.TextCtrl(self)
        user_sizer.Add(self.user, 0, wx.ALL, 5)

        # pass info
        p_sizer = wx.BoxSizer(wx.HORIZONTAL)

        p_lbl = wx.StaticText(self, label="Password:")
        p_sizer.Add(p_lbl, 0, wx.ALL | wx.CENTER, 5)
        self.password = wx.TextCtrl(self, style=wx.TE_PASSWORD | wx.TE_PROCESS_ENTER)
        self.password.Bind(wx.EVT_TEXT_ENTER, self.on_login)
        p_sizer.Add(self.password, 0, wx.ALL, 5)

        main_sizer = wx.BoxSizer(wx.VERTICAL)
        main_sizer.Add(user_sizer, 0, wx.ALL, 5)
        main_sizer.Add(p_sizer, 0, wx.ALL, 5)

        btn = wx.Button(self, id=wx.ID_OK, label="Login")
        btn.Bind(wx.EVT_BUTTON, self.on_login)
        main_sizer.Add(btn, 0, wx.ALL | wx.CENTER, 5)

        self.main_sizer = main_sizer

        self.error_text = wx.StaticText(self, label="Error logging in.")

        self.error_text.SetForegroundColour(wx.RED)
        self.error_text.Hide()

        self.main_sizer.Add(self.error_text)

        self.Bind(wx.EVT_CLOSE, self.on_close)

        self.SetSizer(main_sizer)

    def on_close(self, event):
        self.Destroy()

    # ----------------------------------------------------------------------
    def on_login(self, event):
        """
        Check credentials and login
        """
        LOGIN_URL = f"{self.parent.base_site_url}/accounts/login/"

        user = self.user.GetValue()
        password = self.password.GetValue()

        rqst = requests.session()
        rsp = rqst.get(LOGIN_URL)

        token = (
            BeautifulSoup(rsp.content, "html.parser").find("input").attrs["value"]
        )  # , attr={"name": "csrfmiddlewaretoken"}).attrs("value")

        # token = rsp.cookies["csrftoken"]
        # header = {"X-CSRFToken": token}
        # cookies = {"csrftoken": token}

        data = {
            "username": user,
            "password": password,
            "csrfmiddlewaretoken": token,
            "next": "/",
        }

        print(data)
        rqst.headers["Referer"] = LOGIN_URL

        rsp = rqst.post(
            LOGIN_URL,
            data=data,
            # headers=header,
            # cookies=cookies
        )

        soup = BeautifulSoup(rsp.content, "html.parser")

        if soup.find("button") and soup.find("button").find(string="Login"):
            print("login fail")
            pass
        else:
            print("login success")
            self.parent.login_success = True
        # print(rqst.headers)

        rqst.headers["X-CSRFToken"] = token
        self.parent.rqst = rqst

        print(rqst.headers)
        print(rqst.cookies)
        new = rqst.get(f"{self.parent.base_site_url}/accounts/api/atlas/get_cases_user")

        print("CASESE", new)
        print(new.content)

        if self.parent.login_success:
            self.parent.upload_button.Enable()
            self.parent.login_button.Hide()
            self.parent.panel.Layout()
            self.Destroy()

            # self.parent.login_button.SetBackgroundColour(wx.BLUE)
            # self.parent.login_button.SetForegroundColour(wx.WHITE)
        else:
            self.login_fail()

    def login_fail(self):
        self.error_text.Show()
        self.Layout()
        pass


class SortableList(ULC.UltimateListCtrl, listmix.ColumnSorterMixin):
    def __init__(self, parent, col_number, DATA={}):
        ULC.UltimateListCtrl.__init__(
            self,
            parent,
            wx.ID_ANY,
            agwStyle=ULC.ULC_REPORT | ULC.ULC_VRULES | ULC.ULC_HRULES
            # | ULC.ULC_SINGLE_SEL
            | ULC.ULC_HAS_VARIABLE_ROW_HEIGHT,
        )
        self.itemDataMap = DATA
        listmix.ColumnSorterMixin.__init__(self, col_number)
        self.Bind(wx.EVT_LIST_COL_CLICK, self.OnColumn)

    def OnColumn(self, e):
        self.Refresh()
        e.Skip()

    def GetListCtrl(self):
        return self

    def UpdateData(self, DATA):
        self.itemDataMap = DATA


class SeriesGrid(wx.Panel):
    def __init__(self, parent):
        wx.Panel.__init__(self, parent, size=(-1, 200), style=wx.SUNKEN_BORDER)

        self.parent = parent

        self.main_sizer = wx.BoxSizer(wx.VERTICAL)

        self.series_sizer = wx.GridSizer(wx.HORIZONTAL)
        self.series_sizer.SetVGap(40)

        self.text = wx.StaticText(self, label="SERIES TO UPLOAD")
        self.main_sizer.Add(self.text)

        self.main_sizer.Add(self.series_sizer)

        self.SetSizerAndFit(self.main_sizer)

        self.series = []

    def display_series(self):
        self.series_sizer.Clear(True)

        for name, number in self.series:
            logger.debug(f"add series: {name}")
            panel = SeriesPanel(self, name, number)

            self.series_sizer.Add(panel)

        self.Layout()


class SeriesPanel(wx.Panel):
    def __init__(self, parent, name, number):
        wx.Panel.__init__(self, parent, size=(-1, 200), style=wx.SUNKEN_BORDER)

        self.parent = parent

        self.SetBackgroundColour(wx.BLUE)
        self.SetForegroundColour(wx.WHITE)

        sizer = wx.BoxSizer(wx.VERTICAL)

        name = wx.StaticText(self, label=name)
        number = wx.StaticText(
            self, label=f"[{number}]", style=wx.ALIGN_CENTER_HORIZONTAL
        )
        sizer.Add(name)
        sizer.Add(number)

        self.SetSizerAndFit(sizer)


class AnonGui(wx.App):
    def __init__(
        self,
        redirect=False,
        filename=None,
        input_dir="",
        output_dir="",
        silent_fail=False,
        clear_files=True,
        clear_files_on_close=True,
        dev=False,
        remove_files_on_annonymisation=False,
    ):
        self.dir = output_dir
        self.input_dir = input_dir

        logger.debug(f"Input dir: {input_dir}")
        logger.debug(f"Output dir: {output_dir}")

        self.base_site_url = "https://www.penracourses.org.uk"

        if dev:
            self.base_site_url = "http://localhost:8000"

        self.login_success = False

        self.files = {}
        self.series_map = {}
        self.series_map_details = {}

        self.active = ()

        self.silent_fail = silent_fail
        self.clear_files_on_start = clear_files
        self.clear_files_on_close = clear_files_on_close
        self.clear_files_on_upload = True
        self.remove_files_on_annonymisation = remove_files_on_annonymisation

        self.rqst = None

        self.worker = None

        wx.App.__init__(self, redirect, filename)

    def my_listener(self, message, arg2=None):
        """
        Listener function
        """
        logger.debug(f"Received the following message: {message}")
        if arg2:
            logger.debug(f"Received another arguments: {arg2}")

    def OnInit(self):
        self.name = "SingleApp-%s" % wx.GetUserId()
        self.instance = wx.SingleInstanceChecker(self.name)

        if self.instance.IsAnotherRunning():
            logger.debug("Another instance detected")
            wx.adv.NotificationMessage("Another instance detected", "Trigger refresh").Show()
            context = zmq.Context()
            socket = context.socket(zmq.PUSH)
            socket.connect("tcp://127.0.0.1:5556")
            socket.send_string("loaded")
            logger.debug("Trigger refresh")

            time.sleep(5)
            if not self.silent_fail:
                wx.MessageBox("Another instance is running", "ERROR")
            return False

        def zmq_listener(self):
            context = zmq.Context()
            self.socket = context.socket(zmq.PULL)
            self.socket.bind("tcp://127.0.0.1:5556")
            logger.debug("idle")
            while True:
                msg = self.socket.recv()
                # if msg == 'zeromq':
                logger.debug(f"{msg=}")
                if msg == b"loaded":
                    logger.debug("loaded1")
                    wx.CallAfter(self.AnnonymiseFilesInDir, self.input_dir, self.dir)

        self.zmq_thread = threading.Thread(
            target=zmq_listener, args=(self,), daemon=True
        )
        self.zmq_thread.start()

        self.frame = wx.Frame(
            None,
            wx.ID_ANY,
            title=__APP_NAME__,
            style=wx.DEFAULT_FRAME_STYLE | wx.STAY_ON_TOP,
            size=(300, 400),
        )

        self.frame.SetIcon(wx.Icon(resource_path("icon\icon1.ico")))

        self.panel = wx.Panel(self.frame, wx.ID_ANY)

        self.file_detail_map = {}
        self.file_duplicate_check = {}

        self.list_ctrl = SortableList(
            self.panel,
            6,
        )
        self.series_grid = SeriesGrid(self.panel)

        if self.clear_files_on_start:
            self.ClearFiles(load_dir=False)

        self.panel.sizer = wx.BoxSizer(wx.VERTICAL)

        #self.link_rapid_upload = hl.HyperLinkCtrl(
        #    self.panel,
        #    wx.ID_ANY,
        #    "Rapid",
        #    URL=f"{self.base_site_url}/rapids/create/",
        #)
        ## self.panel.sizer.Add(self.link_rapid_upload, 0, wx.ALL, 10)

        #self.link_long_upload = hl.HyperLinkCtrl(
        #    self.panel,
        #    wx.ID_ANY,
        #    "Long Series",
        #    URL=f"{self.base_site_url}/longs/create/series",
        #)
        #self.link_atlas_case = hl.HyperLinkCtrl(
        #    self.panel,
        #    wx.ID_ANY,
        #    "Atlas Case",
        #    URL=f"{self.base_site_url}/atlas/case/create/",
        #)
        # self.panel.sizer.Add(self.link_long_upload, 0, wx.ALL, 10)

        # Create menu bar
        self.menu_bar = wx.MenuBar()


        # Create File menu
        file_menu = wx.Menu()


        # Create Load menu item
        load_item = wx.MenuItem(file_menu, wx.ID_ANY, "&Import Directory")
        #load_item.SetToolTip("Click to manually load dicom from a directory.")
        file_menu.Append(load_item)

        self.Bind(wx.EVT_MENU, self.import_from_dir, load_item)
        #file_menu.Append(wx.ID_OPEN, "&Open")
        #file_menu.Append(wx.ID_SAVE, "&Save")
        #file_menu.AppendSeparator()
        file_menu.Append(wx.ID_EXIT, "&Exit")
        self.Bind(wx.EVT_MENU, lambda evt: wx.Exit(), id=wx.ID_EXIT)

        self.menu_bar.Append(file_menu, "&File")


        # Create Images menu
        images_menu = wx.Menu()

        # Create Clear Files menu item
        clear_files_item = wx.MenuItem(images_menu, wx.ID_ANY, "&Clear files")
        #clear_files_item.SetToolTip("Click to clear all files.")
        images_menu.Append(clear_files_item)

        self.menu_bar.Append(images_menu, "&Images")

        # Create Reorder Files menu item
        reorder_files_item = wx.MenuItem(images_menu, wx.ID_ANY, "&Reorder")
        #reorder_files_item.SetToolTip("Click to reorder files as they are shown below. This will sequentially rename the files. Please note this is usually better done once the files have been uploaded.")
        images_menu.Append(reorder_files_item)

        self.Bind(wx.EVT_MENU, self.on_reorder_files, reorder_files_item)

        # Bind Clear Files menu item event
        self.Bind(wx.EVT_MENU, self.ClearFiles, clear_files_item)


        # Create Reannonymise Files menu item
        reannonymise_files_item = wx.MenuItem(images_menu, wx.ID_ANY, "&Reannonymise")
        #reannonymise_files_item.SetToolTip("Click to reannonymise the currently loaded files.")
        images_menu.Append(reannonymise_files_item)

        self.Bind(wx.EVT_MENU, self.on_reannonymise_files, reannonymise_files_item)


        # Create Refresh Files menu item
        refresh_files_item = wx.MenuItem(images_menu, wx.ID_ANY, "&Refresh")
        images_menu.Append(refresh_files_item)

        self.Bind(wx.EVT_MENU, lambda evt: self.LoadDir(), refresh_files_item)


        # Create Item creation menu
        create_menu = wx.Menu()

        create_menu_items = (
            ("&Rapid", f"/rapids/create/"),
            ("&Long Series", f"/longs/create/series"),
            ("&Atlas Case", f"/atlas/case/create/"),
        )

        for label, url in create_menu_items:
            item = wx.MenuItem(create_menu, wx.ID_ANY, label)
            create_menu.Append(item)
            self.Bind(wx.EVT_MENU, lambda evt, url=url: webbrowser.open(f"{self.base_site_url}{url}"), item)

        self.menu_bar.Append(create_menu, "&Create")

        # Create Help menu
        help_menu = wx.Menu()
        help_menu.Append(wx.ID_HELP, "&Help")
        help_menu.Append(wx.ID_ABOUT, "&About")
        self.menu_bar.Append(help_menu, "&Help")

        self.Bind(wx.EVT_MENU, self.on_about_box, id=wx.ID_ABOUT)
        self.Bind(wx.EVT_MENU, self.on_help, id=wx.ID_HELP)

        # Set menu bar
        self.frame.SetMenuBar(self.menu_bar)

        #hoz_links = wx.BoxSizer(wx.HORIZONTAL)
        #hoz_links.Add(self.link_rapid_upload, 1, wx.EXPAND)
        #hoz_links.Add(self.link_long_upload, 1, wx.EXPAND)
        #hoz_links.Add(self.link_atlas_case, 1, wx.EXPAND)
        #self.panel.sizer.Add(hoz_links, 1, wx.EXPAND)

        # self.dirctrl = wx.GenericDirCtrl(self.panel, wx.ID_ANY,
        #                dir=dir,
        #                         style=wx.DIRCTRL_SHOW_FILTERS |
        #                               wx.DIRCTRL_3D_INTERNAL |
        #                               wx.DIRCTRL_MULTIPLE,
        #                         filter="Dicom files (*.dcm)|*.dcm")
        bottom_button_sizer = wx.BoxSizer(wx.HORIZONTAL)
        self.drag_button = wx.Button(
            self.panel, id=wx.ID_ANY, label="Drag"
        )
        self.drag_button.SetToolTip("Click here to drag files")
        self.upload_button = wx.Button(
            self.panel, id=wx.ID_ANY, label="Click here to upload files"
        )
        self.upload_button.SetToolTip("Upload files to the server. You must be logged in.")
        self.upload_button.Disable()
        bottom_button_sizer.Add(self.upload_button, 4, wx.EXPAND)
        bottom_button_sizer.Add(self.drag_button, 1, wx.EXPAND)

        # Use some sizers to see layout options
        # self.panel.sizer.Add(self.dirctrl, 1, wx.EXPAND)

        # self.Bind(wx.EVT_BUTTON, self.OnDragInit, self.drag_button)
        self.Bind(wx.EVT_LEFT_DOWN, self.OnDragInit, self.drag_button)
        self.Bind(wx.EVT_LEFT_DOWN, self.OnFileUpload, self.upload_button)
        self.Bind(wx.EVT_LIST_COL_CLICK, self.OnColumnClick)

        # self.list_ctrl = wx.ListCtrl(self.panel,
        #                 style=wx.LC_REPORT
        #                 |wx.BORDER_SUNKEN
        #                 )

        self.Bind(ULC.EVT_LIST_ITEM_RIGHT_CLICK, self.ShowPopupMenu, self.list_ctrl)

        self.list_ctrl.InsertColumn(0, "Filename")
        self.list_ctrl.InsertColumn(1, "Examination")
        self.list_ctrl.InsertColumn(2, "Instance Number")
        self.list_ctrl.InsertColumn(3, "Name")
        self.list_ctrl.InsertColumn(4, "Series Description")
        self.list_ctrl.InsertColumn(5, "Slice location")
        self.list_ctrl.InsertColumn(6, "Hash")
        self.list_ctrl.InsertColumn(7, "Series Instance UID")

        # self.UpdateListCtrl()

        self.login_button = wx.Button(self.panel, id=wx.ID_ANY, label="Login")
        self.login_button.SetForegroundColour(wx.BLUE)
        self.login_button.SetBackgroundColour(wx.WHITE)
        self.login_button.SetToolTip("Click to login to the server")


        # wx.Bind(self.clear_files_button, )
        self.Bind(wx.EVT_BUTTON, self.Login, self.login_button)
        #self.Bind(wx.EVT_BUTTON, self.ClearFiles, self.clear_files_button)
        # Use some sizers to see layout options
        # self.panel.sizer.Add(self.dirctrl, 1, wx.EXPAND)

        hoz = wx.BoxSizer(wx.HORIZONTAL)
        hoz.Add(self.login_button, 1, wx.EXPAND)

        self.panel.sizer.Add(hoz, 1, wx.EXPAND)

        self.panel.sizer.Add(self.series_grid, 2, wx.EXPAND)
        self.panel.sizer.Add(self.list_ctrl, 8, wx.EXPAND)
        #self.panel.sizer.Add(self.drag_button, 2, wx.EXPAND)
        #self.panel.sizer.Add(self.upload_button, 2, wx.EXPAND)
        self.panel.sizer.Add(bottom_button_sizer, 1, wx.EXPAND)

        self.progress_sizer = wx.BoxSizer(wx.VERTICAL)
        self.progress_bar = wx.Gauge(
            self.panel, style=wx.GA_HORIZONTAL | wx.GA_PROGRESS
        )
        # self.progress_bar = PG.PyGauge(self.panel, style=wx.GA_HORIZONTAL|wx.GA_PROGRESS)
        # self.progress_bar.SetDrawValue(draw=True, drawPercent=True, font=None, colour=wx.RED, formatString=None)
        self.progress_text = wx.StaticText(self.panel, label="Text")
        self.progress_sizer.Add(self.progress_bar, 2, wx.EXPAND)
        self.progress_sizer.Add(self.progress_text, 2, wx.EXPAND)
        self.panel.sizer.Add(self.progress_sizer, 2, wx.EXPAND)
        self.progress_sizer.ShowItems(show=False)

        # Layout sizers
        self.panel.SetSizer(self.panel.sizer)
        self.panel.SetAutoLayout(1)
        self.panel.sizer.Fit(self.panel)
        self.frame.Show()
        # self.spinner.Hide()

        wx.CallAfter(self.post_init)

        return super().OnInit()

    def post_init(self):
        self.AnnonymiseFilesInDir(self.input_dir, self.dir)

        self.Bind(wx.EVT_KEY_DOWN, self.onKeyPress)

        self.image_hash_check_in_progress = False
        self.duplicate_check_timer = wx.Timer()
        self.Bind(wx.EVT_TIMER, self.check_image_hashes, self.duplicate_check_timer)
        self.duplicate_check_timer.Start(2000)

    def populate_series_grid(self):
        self.series_grid.series = []

        for series in self.series_map:
            logger.debug(f"Create series: {series}")
            self.series_grid.series.append(
                (
                    self.series_map_details[series]["series_description"],
                    len(self.series_map[series]),
                )
            )

        self.series_grid.display_series()

    def import_from_dir(self, event):
        default_path = str(self.input_dir.resolve())
        with wx.DirDialog(
            self.frame, "Choose a directory to import dicom files from.", style=wx.DD_DIR_MUST_EXIST, defaultPath=default_path
        ) as dlg:
            if dlg.ShowModal() == wx.ID_OK:
                load_path = Path(dlg.GetPath())
                self.AnnonymiseFilesInDir(load_path, self.dir, remove_original=False)

    def AnnonymiseFilesInDir(self, input_dir, output_dir, remove_original: bool | None=None):
        logger.debug("AnnonymiseFilesInDir")
        logger.debug(f"input: {input_dir} / output {output_dir}")
        if input_dir.is_dir():
            logger.debug("is dir")
            files = list(Path(input_dir).rglob("*.dcm"))

            if not files:
                # TODO load from DICOMDIR
                to_exclude = ("DICOMDIR")
                files = [i for i in Path(input_dir).rglob("*") if i.is_file() and i.name not in to_exclude]





        else:
            return

        if not files:
            logger.debug("No files to annonymise")
            wx.adv.NotificationMessage("Annonymise files", "No files to annonymise").Show()
            return

        logger.debug(files)


        if remove_original is None:
            remove_original = self.remove_files_on_annonymisation

        # files = order_files_by_dicom_attribute(files)

        # progress_dialog = wx.ProgressDialog(
        #    "Annonymise files",
        #    "Removing data...",
        #    maximum=len(files),
        #    style=wx.PD_APP_MODAL | wx.PD_ELAPSED_TIME | wx.PD_ESTIMATED_TIME,
        # )
        with self.progress(len(files), "Annonymise files") as p:
            # p.SetRange(len(files))

            i: int = 0
            for n, file in enumerate(files):
                # progress_dialog.Update(n)
                p.update(n)
                new_filepath: Path
                # Get next available filename
                while os.path.exists(
                    new_filepath := output_dir.joinpath(f"IMG_{i:03}.dcm")
                ):
                    i += 1
                    # new_filepath = Path(output_dir).joinpath(f"IMG_{i:03}.dcm")

                logger.debug(f"anon: {n}")
                try:
                    dataset, filepath = anonymize_file(file, new_filepath, remove_original=remove_original)
                except pydicom.errors.InvalidDicomError:
                    continue
                    self.add_datails_to_file_detail_map(dataset, filepath)
                    wx.GetApp().Yield()
        # progress_dialog.Destroy()

        self.LoadDir(self.dir)

    @contextlib.contextmanager
    def progress(self, total: int, text: str = "Working"):
        # self.progress_sizer.Show()
        self.progress_text.SetLabelText(text)
        self.progress_bar.SetRange(total)
        self.progress_sizer.ShowItems(show=True)
        self.panel.sizer.Fit(self.panel)
        self.frame.Layout()
        try:

            class P:
                def __init__(self, gui, total, text):
                    self.gui = gui
                    self.text = text
                    self.total = total

                def update(self, n: int):
                    self.gui.progress_bar.SetValue(n)
                    self.gui.progress_text.SetLabelText(f"{self.text} : {n}")
                    self.gui.frame.Refresh()

            yield P(self, total, text)
        finally:
            # self.progress_sizer.Hide()
            self.progress_sizer.ShowItems(show=False)
            self.panel.Layout()
            # self.panel.SetAutoLayout(1)
            # self.panel.sizer.Fit(self.panel)

    def onKeyPress(self, event):
        keycode = event.GetKeyCode()
        logger.debug(keycode)
        if keycode == wx.WXK_F1:
            logger.debug("you pressed the F1!")
            self.on_help()
            return
        event.Skip()

    def on_help(self, event=None):
        webbrowser.open(f"{self.base_site_url}/atlas/help")

    def on_about_box(self, event=None):
        description = f"""
A dicom export/upload tool

Settings
--------
Watch/import dir: {self.input_dir}
Annonymised files dir: {self.dir}

clear files on start: {self.clear_files_on_start}
clear files on close: {self.clear_files_on_close}
remove files on annonymisations: {self.remove_files_on_annonymisation}

base site url: {self.base_site_url}
"""

        licence = """GPL"""

        info = wx.adv.AboutDialogInfo()

        # info.SetIcon(wx.Icon('hunter.png', wx.BITMAP_TYPE_PNG))
        info.SetName(__APP_NAME__)
        info.SetVersion(__VERSION__)
        info.SetDescription(description)
        icon_path = resource_path("icon\icon1.png")
        desired_width = 200
        desired_height = 200

        # Load the icon image
        image = wx.Image(icon_path)

        # Resize the image
        resized_image = image.Rescale(desired_width, desired_height)

        # Create a new icon with the resized image
        resized_icon = wx.Icon(resized_image.ConvertToBitmap())

        info.SetIcon(resized_icon)

        info.SetLicence(licence)

        wx.adv.AboutBox(info)

    def OnExit(self, event=None):
        print("on exit")
        if self.clear_files_on_close:
            self.ClearFiles(load_dir=False)

        # self.zmq_thread.join()

        return super().OnExit()

    def LoadDir(self, dir=None, refresh=True, clear_file_detail_map=False):
        """Loads data from the output directory (annonymised files)"""
        if clear_file_detail_map:
            self.file_detail_map = {}
            self.file_duplicate_check = {}
        if dir is None:
            dir = self.dir

        logger.debug(f"LoadDir : {dir} / refresh: {refresh} / clear_file_detail_map: {clear_file_detail_map}")

        if refresh:
            self.files = {}
            self.series_map = {}
            self.series_map_details = {}
        for path in Path(dir).glob("*.dcm"):
            self.AddFile(path)

        self.UpdateListCtrl()

    def AddFile(self, path):
        hash = get_file_hash(path)
        logger.debug("Hash", hash)

        if hash not in self.files.values():
            self.files[path] = hash
        # Remove duplicate files
        else:
            os.remove(path)

    def RemoveFile(self, path):
        del self.files[path]

    def OnColumnClick(self, event):
        self.list_ctrl.Refresh()
        event.Skip()

    def ShowPopupMenu(self, event):
        if self.list_ctrl.GetSelectedItemCount():
            pass
        else:
            ind = event.GetIndex()
            if ind > -1:
                self.list_ctrl.Select(ind)
            else:
                return
        menu = wx.Menu()
        menu.Append(1, "Delete selected items")
        menu.Bind(wx.EVT_MENU, self.DeleteItems, id=1)
        menu.Append(2, "View Dicom")
        menu.Bind(wx.EVT_MENU, self.ViewDicom, id=2)

        for i in range(self.list_ctrl.GetItemCount()):
            if self.list_ctrl.IsSelected(i):
                path = self.list_ctrl.GetItemPyData(i)["file"]
                if (
                    path in self.file_duplicate_check
                    and self.file_duplicate_check[path]["id"]
                ):
                    if self.file_duplicate_check[path]["type"] == "series":
                        menu.Append(3, "View on site (series)")
                        menu.Bind(
                            wx.EVT_MENU,
                            lambda evt: webbrowser.open(
                                f"{self.base_site_url}{self.file_duplicate_check[path]['url']}"
                            ),
                            id=3,
                        )
                    else:
                        menu.Append(3, "View on site (uncategorised)")
                        menu.Bind(
                            wx.EVT_MENU,
                            lambda evt: webbrowser.open(
                                f"{self.base_site_url}{self.file_duplicate_check[path]['url']}"
                            ),
                            id=3,
                        )
                break

        self.frame.PopupMenu(menu)

    def ViewDicom(self, event):
        selectedItems = []
        for i in range(self.list_ctrl.GetItemCount()):
            if self.list_ctrl.IsSelected(i):
                # path = self.list_ctrl.GetItemPyData(i["file"])
                path = self.list_ctrl.GetItemPyData(i)["file"]
                print(path)

                viewer = ViewerFrame(None, "Viewer", path)
                # viewer.MainLoop()
                break

    def on_reannonymise_files(self, event):
        global anonymizer
        anonymizer = dicognito.anonymizer.Anonymizer()
        paths = []

        # self.file_observer.pause()

        tempdir = Path(tempfile.gettempdir())
        to_move = []
        with self.progress(self.list_ctrl.GetItemCount(), "Reannonymise files") as p:
            for i in range(self.list_ctrl.GetItemCount()):
                p.update(i)
                path = self.list_ctrl.GetItemPyData(i)["file"]
                logger.debug("--", path)
                # paths.append(path)
                new_path = annonymise(path, tempdir, run_gui=False)
                os.remove(path)
                to_move.append((new_path, Path(self.dir, new_path.name)))

        for path, new_path in to_move:
            logger.debug("**", path, new_path)
            os.rename(path, new_path)

        self.LoadDir(self.dir, clear_file_detail_map=True)

    def on_reorder_files(self, event):
        paths = []

        # self.file_observer.pause()

        tempdir = Path(tempfile.gettempdir())
        to_move = []
        for i in range(self.list_ctrl.GetItemCount()):
            path = self.list_ctrl.GetItemPyData(i)["file"]
            logger.debug("--", path)
            # paths.append(path)

            os.replace(path, new_path := tempdir.joinpath(f"IMG_{i:03}.dcm"))
            # os.remove(path)
            to_move.append((new_path, Path(self.dir, new_path.name)))
            i += 1

        for path, new_path in to_move:
            logger.debug("**", path, new_path)
            os.rename(path, new_path)

        self.LoadDir(self.dir)
        # self.file_observer.resume()

    def DeleteItems(self, event):
        selectedItems = []
        for i in range(self.list_ctrl.GetItemCount()):
            if self.list_ctrl.IsSelected(i):
                selectedItems.append(self.list_ctrl.GetItemPyData(i)["file"])

        logger.debug(selectedItems)
        for f in selectedItems:
            self.RemoveFile(f)
            os.remove(f)

        wx.CallAfter(self.UpdateListCtrl)

    def Login(self, event=None):
        dlg = LoginDialog(self)

        dlg.ShowModal()

    def ClearFiles(self, event=None, load_dir: bool = True, files:None| list[(str, str)] = None):
        if files is None:
            files = Path(self.dir).glob("*")
        else:
            files = [Path(self.dir, f) for f, file_hash in files]

        for f in files:
            try:
                os.remove(f)
            except OSError:
                logger.debug(f"File to delete not found: {f}")
                pass


        if load_dir:
            self.LoadDir(clear_file_detail_map=True)

    def add_datails_to_file_detail_map(self, dataset, file):
        try:
            study_desc = dataset.StudyDescription
        except:
            study_desc = "..."
        try:
            series_desc = dataset.SeriesDescription
        except:
            series_desc = "..."
        try:
            instance_number = f"{dataset.InstanceNumber:03}"
        except:
            instance_number = "..."
        try:
            patient_name = str(dataset.PatientName)
        except:
            patient_name = "..."
        try:
            slice_location = str(dataset.SliceLocation)
        except:
            slice_location = "..."

        series_instances_uid = str(dataset.SeriesInstanceUID)
        d2 = {
            "series_instances_uid": series_instances_uid,
            "series_description": series_desc,
            "study_description": study_desc,
            "name": patient_name,
        }

        # md5 = hashlib.md5()
        # first = True
        # for i in dataset.pixel_array.astype(str).flatten():
        #    if first:
        #        first = False
        #        md5.update(f"{i}".encode())
        #    else:
        #        md5.update(f",{i}".encode())
        # md5.update(dataset.PixelData)
        # md5.update("".join(dataset.pixel_array.astype(str).flatten()).encode())
        # md5.update(np.char.join("", dataset.pixel_array.astype(str).flatten()).encode())
        # hash = md5.hexdigest()
        # print(hash)

        # md5 = hashlib.md5()
        ##hash = md5.hexdigest()
        ##md5 = hashlib.md5()
        ##first = True
        ##s = ",".join([f"{i}" for i in dataset.pixel_array.astype(str).flatten()]).encode()
        ##print(s)
        # print(dataset.PixelData[:20])
        # print(len(dataset.PixelData))
        # md5.update(dataset.PixelData)

        # hash = md5.hexdigest()

        hasher = blake3()
        hasher.update(dataset.PixelData)
        hash = hasher.hexdigest()
        # dataset.PixelData

        d = (
            file.name,
            study_desc,
            instance_number,
            patient_name,
            series_desc,
            slice_location,
            hash,
            series_instances_uid,
        )

        # hasher = blake3()
        # first = True
        # for i in dataset.pixel_array.astype(str).flatten():
        #    if first:
        #        first = False
        #        hasher.update(f"{i}".encode())
        #    else:
        #        hasher.update(f",{i}".encode())

        # hash = hasher.digest()

        # hash = get_file_hash(file, hasher="md5")

        # hash = None  # If we don't use the hash...
        self.file_detail_map[file] = d, hash, d2

        return d, hash, d2

    def UpdateListCtrl(self, event=None):
        logger.debug("update list ctrl")
        self.list_ctrl.DeleteAllItems()
        n = 0
        DATA = {}

        # progress_dialog = wx.ProgressDialog(
        #    "Loading files",
        #    "Extracting data from files",
        #    maximum=len(self.files),
        #    style=wx.PD_APP_MODAL | wx.PD_ELAPSED_TIME | wx.PD_ESTIMATED_TIME,
        # )
        with self.progress((len(self.files)), "Loading files") as p:
            # p.SetRange(len(self.files))

            self.series_map = defaultdict(list)
            self.series_map_details = defaultdict(dict)

            hashes = []

            for n, f in enumerate(self.files):
                logger.debug(f"update {n}")
                # progress_dialog.Update(n)
                #p.update(n)
                # dataset = pydicom.read_file(f)

                if f in self.file_detail_map:
                    d, hash, d2 = self.file_detail_map[f]
                    #print(f"{n}: in map")
                else:
                    logger.debug("read: {f}")
                    try:
                        dataset = pydicom.dcmread(f)
                    except FileNotFoundError:
                        return

                    #print("recalc")
                    d, hash, d2 = self.add_datails_to_file_detail_map(dataset, f)

                hashes.append(hash)

                series_instances_uid = d2["series_instances_uid"]
                try:
                    self.series_map[series_instances_uid].append(f)
                    self.series_map_details[series_instances_uid][
                        "series_description"
                    ] = d2["series_description"]
                    self.series_map_details[series_instances_uid][
                        "study_description"
                    ] = d2["study_description"]
                    self.series_map_details[series_instances_uid]["name"] = d2["name"]
                except:
                    logger.debug(f"Unable to get Series Instance UID: {f.name}")

                #logger.debug(d)
                #DATA[n] = d
                #self.list_ctrl.Append(d)
                #self.list_ctrl.SetItemPyData(n, {"file": f, "hash": hash})
                ##self.list_ctrl.SetItemData(n, n)

                if f in self.file_duplicate_check:
                    if self.file_duplicate_check[f]["id"]:
                        self.list_ctrl.SetItemBackgroundColour(n, wx.RED)

                n = n + 1
        #self.list_ctrl.UpdateData(DATA)
        self.populate_series_grid()

        self.frame.Refresh()
        self.frame.Layout()

        # progress_dialog.Destroy()

        # if hashes:
        #    self.check_image_hashes(hashes)

    def check_image_hashes(self, evt):
        if self.login_success and not self.image_hash_check_in_progress:
            self.image_hash_check_in_progress = True

            to_check = self.file_detail_map.keys() - self.file_duplicate_check.keys()

            if to_check:
                hash_file_map = {}
                for file in to_check:
                    hash_file_map[self.file_detail_map[file][1]] = file

                hashes = list(hash_file_map.keys())

                self.rqst.headers["X-CSRFToken"] = self.rqst.cookies["csrftoken"]
                # print(self.rqst.headers)
                # print(self.rqst.cookies)
                resp = self.rqst.post(
                    f"{self.base_site_url}/api/atlas/check_image_hashes/",
                    data=json.dumps(hashes),
                    # files=files,
                )

                for hash, dup in resp.json().items():
                    self.file_duplicate_check[hash_file_map[hash]] = dup

                if resp.json().items():
                    wx.CallAfter(self.UpdateListCtrl)
            self.image_hash_check_in_progress = False

    def OnColRightClick(self, event):
        item = self.list_ctrl.GetColumn(event.GetColumn())
        logger.debug(item)

    def OnFileUpload(self, event):
        # self.spinner.Show()
        wx.CallAfter(self.upload)

    def upload(self):
        files_to_upload = []
        duplicate_files_not_uploaded = []
        logger.debug("Upload files")
        for f in self.files:
            if f in self.file_duplicate_check and self.file_duplicate_check[f]["id"]:
                    # duplicate the api return format so ClearFiles works
                    duplicate_files_not_uploaded.append((f.name, "..."))
            else:
                files_to_upload.append(("files", open(str(f), "rb")))

        # chunck files
        n = 10
        chunked_files = [
            files_to_upload[i : i + n] for i in range(0, len(files_to_upload), n)
        ]

        # progress_dialog = wx.ProgressDialog(
        #    "Uploading files",
        #    "Uploading files. This is done in batches...",
        #    maximum=len(chunked_files),
        #    style=wx.PD_ELAPSED_TIME | wx.PD_ESTIMATED_TIME,
        # )
        with self.progress(len(chunked_files), "Uploading files (in batches)") as p:
            # p.SetRange(len(chunked_files))

            upload_file_list = []
            duplicate_file_list = []
            failed = []

            for n, files in enumerate(chunked_files):

                def upload_files(files):
                    self.rqst.headers["X-CSRFToken"] = self.rqst.cookies["csrftoken"]
                    # print(self.rqst.headers)
                    # print(self.rqst.cookies)
                    resp = self.rqst.post(
                        f"{self.base_site_url}/api/atlas/upload_dicom",
                        # data=data,
                        files=files,
                    )

                    logger.debug(f"Resp: {resp}")
                    logger.debug(f"{resp.content}")
                    return resp

                p.update(n)
                # progress_dialog.Update(n, f"Uploading batch {n}/{len(chunked_files)}")

                logger.debug(f"n: {n}")
                # try to upload the files

                for i in range(3):
                    resp = upload_files(files)
                    if resp.status_code == 200:
                        upload_file_list.extend(resp.json()["uploaded"])
                        duplicate_file_list.extend(resp.json()["duplicates"])
                        failed.extend(resp.json()["failed"])

                        continue

                    logger.debug(f"n: {n} fail (attempt {i})")

        # progress_dialog.Destroy()

        print(upload_file_list)
        print("dup", duplicate_file_list)
        print("failed", failed)



        wx.CallAfter(
            self.upload_complete_message, upload_file_list, duplicate_file_list, failed, duplicate_files_not_uploaded
        )

        if self.clear_files_on_upload:
            all_files = upload_file_list + duplicate_file_list + duplicate_files_not_uploaded
            print(all_files)
            wx.CallAfter(self.ClearFiles,event=None, load_dir=True, files=all_files)

    def upload_complete_message(self, upload_file_list, duplicate_file_list, failed, duplicate_files_not_uploaded):
        wx.MessageBox(
            f"""Files uploaded: {len(upload_file_list)}
Duplicate files (not uploaded): {len(duplicate_file_list)}
Failed to upload: {len(failed)}
Duplicate files (not uploaded): {len(duplicate_files_not_uploaded)}"""
        )
        self.file_duplicate_check = {}

    def OnDragInit(self, event):
        my_data = wx.FileDataObject()
        # files = pathlib.Path(self.dir).glob("*.dcm")
        for f in self.files:
            my_data.AddFile(str(f))

        dragSource = wx.DropSource(self.panel)
        dragSource.SetData(my_data)

        result = dragSource.DoDragDrop(True)


# ---------------------------------------------------------------------------


@app.command()
@app.callback(invoke_without_command=True)
def runGUI(
    ctx: typer.Context,
    input_dir: Path = "C:\\Temp\\",
    output_dir: Path = "C:\\temp_upload\\",
    silent_fail: bool = False,
    clear_files: bool = True,
    clear_files_on_close: bool = True,
    remove_files_on_annonymisation: bool = True,
    dev: bool = False,
):
    if "linux" in sys.platform:
        input_dir = Path("/home/ross/temp/in/")
        output_dir = Path("/home/ross/temp/out/")

    if ctx.invoked_subcommand is None:
        try:
            typer.echo("run gui")
        except AttributeError:  # No console
            pass
        launchGUI(
            input_dir,
            output_dir,
            silent_fail,
            clear_files,
            clear_files_on_close,
            dev,
            remove_files_on_annonymisation,
        )


def launchGUI(
    input_dir,
    output_dir,
    silent_fail,
    clear_files,
    clear_files_on_close,
    dev,
    remove_files_on_annonymisation,
):
    app = AnonGui(
        input_dir=input_dir,
        output_dir=output_dir,
        silent_fail=silent_fail,
        clear_files=clear_files,
        clear_files_on_close=clear_files_on_close,
        dev=dev,
        remove_files_on_annonymisation=remove_files_on_annonymisation,
    )
    app.MainLoop()


if __name__ == "__main__":
    app()

