#[cfg(windows)]
mod windows_impl {
    use super::*;
    use std::ffi::c_void;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::ptr::null_mut;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Controls::{CreateWindowExW, EDIT_CLASSW, ES_MULTILINE, ES_AUTOVSCROLL, ES_WANTRETURN};
    use windows::Win32::UI::WindowsAndMessaging::*;

    fn to_wstring(s: &str) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        std::ffi::OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
    }

    pub fn run() {
        unsafe {
            let hinstance = GetModuleHandleW(None).unwrap();
            let class_name = to_wstring("DragonOverlayWndClass");

            let wnd_class = WNDCLASSW {
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance,
                lpszClassName: PCWSTR(class_name.as_ptr()),
                ..Default::default()
            };

            RegisterClassW(&wnd_class);

            let window_name = to_wstring("Dragon Overlay");
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW.0,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(window_name.as_ptr()),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE | WS_POPUP,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                600,
                300,
                None,
                None,
                hinstance,
                null_mut(),
            );

            if hwnd.0 == 0 {
                eprintln!("Failed to create window");
                return;
            }

            // Create edit control
            let edit = CreateWindowExW(
                0,
                PCWSTR(EDIT_CLASSW),
                PCWSTR(to_wstring("").as_ptr()),
                WS_CHILD | WS_VISIBLE | ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN,
                0,
                0,
                600,
                300,
                hwnd,
                HMENU(1),
                hinstance,
                null_mut(),
            );

            // Connect to app
            let stream = TcpStream::connect(("127.0.0.1", 54231)).ok();
            let stream = Arc::new(Mutex::new(stream));

            // Thread: read incoming messages from the socket and apply commands
            if let Some(s_arc) = stream.clone().lock().unwrap().as_ref() {
                let mut s_clone = s_arc.try_clone().unwrap();
                let hwnd_edit = edit;
                thread::spawn(move || {
                    let mut buf = String::new();
                    loop {
                        buf.clear();
                        match s_clone.read_to_string(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                for line in buf.split('\n') {
                                    if line.trim().is_empty() {
                                        continue;
                                    }
                                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                                        if let Some(cmd) = v.get("cmd").and_then(|c| c.as_str()) {
                                            match cmd {
                                                "show_overlay" | "set_text" => {
                                                    if let Some(t) = v.get("text").and_then(|tt| tt.as_str()) {
                                                        let w = to_wstring(t);
                                                        unsafe {
                                                            SetWindowTextW(hwnd_edit, PCWSTR(w.as_ptr()));
                                                        }
                                                    }
                                                }
                                                "hide_overlay" => unsafe {
                                                    ShowWindow(hwnd, SW_HIDE);
                                                },
                                                "set_overlay_position" => {
                                                    let x = v.get("x").and_then(|n| n.as_i64()).unwrap_or(0) as i32;
                                                    let y = v.get("y").and_then(|n| n.as_i64()).unwrap_or(0) as i32;
                                                    let w = v.get("w").and_then(|n| n.as_i64()).unwrap_or(600) as i32;
                                                    let h = v.get("h").and_then(|n| n.as_i64()).unwrap_or(200) as i32;
                                                    unsafe {
                                                        SetWindowPos(hwnd, HWND(0), x, y, w, h, SWP_NOZORDER);
                                                    }
                                                }
                                                _ => {}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            }

            // Hook for EN_CHANGE: poll edit periodically and send text back on change
            let stream_for_poll = stream.clone();
            thread::spawn(move || {
                let mut last = String::new();
                loop {
                    unsafe {
                        let len = GetWindowTextLengthW(edit) as usize + 1;
                        let mut buf: Vec<u16> = vec![0; len + 1];
                        let read = GetWindowTextW(edit, &mut buf);
                        if read > 0 {
                            let s = String::from_utf16_lossy(&buf[..read as usize]);
                            if s != last {
                                last = s.clone();
                                // send to app
                                if let Some(opt) = stream_for_poll.lock().unwrap().as_ref() {
                                    let mut ss = opt.try_clone().ok();
                                    if let Some(mut sref) = ss {
                                        let _ = sref.write_all(s.as_bytes());
                                    }
                                }
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            });

            // Show window
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, HWND(0), 0, 0).into() {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    extern "system" fn wndproc(hwnd: HWND, msg: u32, _wparam: WPARAM, _lparam: LPARAM) -> LRESULT {
        unsafe {
            match msg {
                WM_DESTROY => {
                    PostQuitMessage(0);
                    LRESULT(0)
                }
                _ => DefWindowProcW(hwnd, msg, _wparam, _lparam),
            }
        }
    }
}

#[cfg(windows)]
fn main() {
    windows_impl::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("dragon_overlay_windows is a Windows-only helper. Use the cross-platform `dragon_overlay` for testing on Linux.");
}
