from distutils.core import setup # Need this to handle modules
import py2exe 
import math # We have to import all modules used in our program

import zmq

setup(console=['anon_gui.py'], py_modules=['anon', 'anon_gui', 'reslice', 'sortList', 'uploader', 'wxDicomViewer'],   options={
        'py2exe': {
            'includes': ['zmq.backend.cython'],
            'excludes': ['zmq.libzmq'],
            'dll_excludes': ['libzmq.pyd'],
        }
    },
    data_files=[
        ('lib', (zmq.libzmq.__file__,))
    ])
