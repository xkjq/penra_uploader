DATA = {
0 : ("3", "3", "1"),
1 : ("2", "1", "2"),
2 : ("1", "2", "3")
}

import wx
import wx.lib.mixins.listctrl as listmix
from wx.lib.agw import ultimatelistctrl as ULC

class MyList(ULC.UltimateListCtrl, listmix.ColumnSorterMixin):
    def __init__(self, parent, columns):
        ULC.UltimateListCtrl.__init__(self, parent, agwStyle=ULC.ULC_REPORT | ULC.ULC_HAS_VARIABLE_ROW_HEIGHT)
        self.itemDataMap = DATA
        listmix.ColumnSorterMixin.__init__(self, columns)
        self.Bind(wx.EVT_LIST_COL_CLICK, self.OnColumn)

    def OnColumn(self, e):
        self.Refresh()
        e.Skip()

    def GetListCtrl(self):
        return self

class MainWindow(wx.Frame):
    def __init__(self, *args, **kwargs):
        wx.Frame.__init__(self, *args, **kwargs)

        self.list = MyList(self, 3)
        self.list.InsertColumn(0, "A")
        self.list.InsertColumn(1, "B")
        self.list.InsertColumn(2, "C")

        items = DATA.items()
        for key, data in items:
            index = self.list.Append(data)
            self.list.SetItemData(index, key)

        self.Show()

app = wx.App(False)
win = MainWindow(None)
app.MainLoop()