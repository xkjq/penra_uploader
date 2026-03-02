# -*- mode: python ; coding: utf-8 -*-

import os
import importlib.util
from pathlib import Path

here = Path(__file__).resolve().parent
project_root = here

# Prepare datas and binaries lists; include icon resources
datas = [
    (str(project_root / 'icon'), 'icon'),
    (str(project_root / 'icon' / 'icon1.png'), 'icon'),
    # include user-supplied CA bundle folder (all files) so frozen app can use it
    (str(project_root / 'cert'), 'cert')
]
binaries = []

# Try to include pythonnet runtime DLLs so Python.Runtime.dll is available
try:
    spec_py = importlib.util.find_spec("pythonnet")
    if spec_py and spec_py.submodule_search_locations:
        pn_path = Path(spec_py.submodule_search_locations[0])
        runtime_dir = pn_path / "runtime"
        if runtime_dir.exists():
            for f in runtime_dir.rglob("*.dll"):
                # destination folder inside the frozen app
                binaries.append((str(f), "pythonnet/runtime"))
except Exception:
    # best-effort; if we can't locate pythonnet at build time, user can add it manually
    pass

a = Analysis(
    ['nice.py'],
    pathex=[str(project_root)],
    binaries=binaries,
    datas=datas,
    hiddenimports=['bs4', 'requests', 'nicegui', 'pynng'],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
    optimize=0,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.datas,
    [],
    name='nice_uploader',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=True,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
)
