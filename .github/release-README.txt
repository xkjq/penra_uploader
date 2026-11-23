Uploader RS - release bundle
============================

Contents
--------
  uploader_rs[.exe]   Main application (scan, anonymise, upload).
  diviz-rs[.exe]      DICOM viewer. The uploader launches it automatically; keep
                      it in the same directory as uploader_rs (it is also looked
                      up on PATH if you move it elsewhere).
  divue[.exe]         Standalone metadata viewer.
  diface[.exe]        Standalone facial-defacing CLI.
  notify[.exe]        Small IPC helper used by external exporters to tell a
                      running uploader that new files are ready.

Running
-------
Windows
  Double-click uploader_rs.exe.

Linux
  chmod +x uploader_rs diviz-rs divue diface notify
  ./uploader_rs

Configuration and logs are written to ~/.uploader (0700 on Unix):
  config.json         server URL, theme, parallelism, ...
  api_token           API token (0600 on Unix)
  last_scan.json      last scan result
  request_log.txt     request/debug log

Notes
-----
- Keep the binaries together: the uploader looks for diviz-rs next to itself
  first, then on PATH.
- Linux builds link against the system glibc and the usual X11/Wayland/GPU
  libraries. If the window does not open, check the log in ~/.uploader.
- The Windows build is unsigned; SmartScreen may warn on first launch.
