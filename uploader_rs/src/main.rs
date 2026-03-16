use eframe::{egui, NativeOptions};
use egui::CentralPanel;
mod anonymizer;
use anonymizer::anonymize_file;
mod upload;
use upload::{upload_anon_dir, UploadResult, scan_for_upload, SeriesInfo, FileEntry};
mod dicom_viewer;
use dicom_viewer::{read_metadata, read_metadata_all};
mod meta_viewer;
use meta_viewer::run_meta_viewer;
use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use blake3;
use std::fs;
use rfd::FileDialog;
use nng::{Protocol, Socket};

struct AppState {
    last_msg: String,
    export_dir: PathBuf,
    rx: Option<Receiver<String>>,
    processed: Vec<String>,
    seed: Option<String>,
    username: String,
    password: String,
    logged_in_user: Option<String>,
    move_files: bool,
    recurse_depth: i32,
    ext_filter: String,
    notify_on_process: bool,
    ready_series: Vec<SeriesInfo>,
    selected_series: Vec<bool>,
    // metadata viewer state
    metadata_window_open: bool,
    metadata_compare_open: bool,
    metadata_single: Option<(String, HashMap<String,String>)>,
    metadata_compare: Vec<(String, HashMap<String,String>)>,
    selected_files_for_meta: HashSet<String>,
    metadata_select_mode: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_msg: String::new(),
            export_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("export"),
            rx: None,
            processed: Vec::new(),
            seed: None,
            username: String::new(),
            password: String::new(),
            logged_in_user: upload::token_username(),
            move_files: false,
            recurse_depth: -1,
            ext_filter: "dcm".to_string(),
            notify_on_process: false,
            ready_series: Vec::new(),
            selected_series: Vec::new(),
            metadata_window_open: false,
            metadata_compare_open: false,
            metadata_single: None,
            metadata_compare: Vec::new(),
            selected_files_for_meta: HashSet::new(),
            metadata_select_mode: false,
        }
    }
}

impl eframe::App for AppState {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        CentralPanel::default().show(ctx, |ui| {
            ui.heading("Uploader (Rust) - skeleton");

            if ui.button("Launch backend (Python)").clicked() {
                match Command::new("python3").arg("../nice.py").arg("--work-dir").arg(".").spawn() {
                    Ok(_) => self.last_msg = "Launched python backend".to_string(),
                    Err(e) => self.last_msg = format!("Failed to launch backend: {}", e),
                }
            }

            ui.horizontal(|ui| {
                ui.label("Seed:");
                let mut s = self.seed.clone().unwrap_or_default();
                if ui.text_edit_singleline(&mut s).changed() {
                    self.seed = if s.is_empty() { None } else { Some(s.clone()) };
                }
            });

            ui.collapsing("Login", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Username:");
                    ui.text_edit_singleline(&mut self.username);
                });
                ui.horizontal(|ui| {
                    ui.label("Password:");
                    ui.add(egui::widgets::TextEdit::singleline(&mut self.password).password(true));
                });

                if ui.button("Login").clicked() {
                    let user = self.username.clone();
                    let pass = self.password.clone();
                    let (tx, rx) = mpsc::channel::<String>();
                    self.rx = Some(rx);
                    thread::spawn(move || {
                        let base = std::env::var("UPLOADER_BASE_URL").unwrap_or_else(|_| "https://www.penracourses.org.uk".to_string());
                        let url = format!("{}{}", base, "/api/atlas/create_api_token");
                        let token_check = format!("{}{}", base, "/api/atlas/token_check");
                        let client = reqwest::blocking::Client::new();
                        let body = serde_json::json!({"username": user, "password": pass});
                        match client.post(&url).json(&body).send() {
                            Ok(r) => {
                                if r.status().is_success() {
                                    if let Ok(v) = r.json::<serde_json::Value>() {
                                        if let Some(t) = v.get("token").and_then(|x| x.as_str()) {
                                            if upload::save_api_token(t) {
                                                let _ = tx.send("Login successful".to_string());
                                                // validate token and fetch username
                                                match client.post(&token_check).header("Authorization", format!("Bearer {}", t)).send() {
                                                    Ok(vc) => {
                                                        if vc.status().is_success() {
                                                            if let Ok(info) = vc.json::<serde_json::Value>() {
                                                                if info.get("valid").and_then(|b| b.as_bool()).unwrap_or(false) {
                                                                    let uname = info.get("username").and_then(|s| s.as_str()).unwrap_or("API token");
                                                                    let _ = tx.send(format!("LOGIN_USER:{}", uname));
                                                                } else {
                                                                    let _ = tx.send("Token invalid after login".to_string());
                                                                }
                                                            }
                                                        } else {
                                                            let _ = tx.send(format!("Token check failed: HTTP {}", vc.status()));
                                                        }
                                                    }
                                                    Err(e) => { let _ = tx.send(format!("Token check request error: {}", e)); }
                                                }
                                            } else {
                                                let _ = tx.send("Login received token but failed to save".to_string());
                                            }
                                        } else {
                                            let _ = tx.send("Login response missing token".to_string());
                                        }
                                    } else {
                                        let _ = tx.send("Failed to parse login response".to_string());
                                    }
                                } else {
                                    let _ = tx.send(format!("Login failed: HTTP {}", r.status()));
                                }
                            }
                            Err(e) => { let _ = tx.send(format!("Login request error: {}", e)); }
                        }
                        let _ = tx.send("done".to_string());
                    });
                }

                if ui.button("Logout").clicked() {
                    if upload::clear_api_token() {
                        self.logged_in_user = None;
                        self.last_msg = "Logged out".to_string();
                    } else {
                        self.last_msg = "Failed to clear token".to_string();
                    }
                }

                ui.label(format!("Logged in: {}", self.logged_in_user.clone().unwrap_or_else(|| "no".to_string())));
            });

            if ui.button("Process export (anonymize + notify)").clicked() {
                // spawn a background thread to process .dcm files in export_dir via the Python directory wrapper
                let export = self.export_dir.clone();
                    let anon_dir = export
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join("anon");
                    let (tx, rx) = mpsc::channel::<String>();
                    // capture notify flag before moving into the thread
                    let notify_flag = self.notify_on_process;
                    self.rx = Some(rx);
                let seed_clone = self.seed.clone();
                thread::spawn(move || {
                        if let Ok(entries) = fs::read_dir(&export) {
                            for ent in entries.flatten() {
                                let p = ent.path();
                                if p.extension().map(|e| e == "dcm").unwrap_or(false) {
                                match anonymize_file(&p, &anon_dir, true, seed_clone.as_deref()) {
                                        Ok(out) => {
                                            let _ = tx.send(format!("Anonymized: {}", out.display()));
                                            if let Ok(bytes) = fs::read(&out) {
                                                let hash = blake3::hash(&bytes);
                                                let _ = tx.send(format!("Hash {}: {}", out.display(), hash.to_hex()));
                                            }
                                        }
                                        Err(e) => {
                                            let _ = tx.send(format!("Anon failed {}: {}", p.display(), e));
                                        }
                                    }
                                }
                            }
                        } else {
                            let _ = tx.send("No export dir or read error".to_string());
                        }

                        // optionally send NNG 'loaded' message to notify other components
                        if notify_flag {
                            match Socket::new(Protocol::Pair0) {
                                Ok(s) => {
                                    if s.dial("tcp://127.0.0.1:9976").is_ok() {
                                        let _ = s.send(&b"loaded"[..]);
                                        let _ = tx.send("Sent NNG 'loaded'".to_string());
                                    } else {
                                        let _ = tx.send("Failed to dial NNG socket".to_string());
                                    }
                                }
                                Err(e) => {
                                    let _ = tx.send(format!("Failed to create NNG socket: {:?}", e));
                                }
                            }
                        }

                        let _ = tx.send("done".to_string());
                    });
            }

            if ui.button("Import from folder").clicked() {
                // Pick a source folder and copy .dcm files into the export dir in background
                if let Some(src) = FileDialog::new().pick_folder() {
                    // capture options
                    let do_move = self.move_files;
                    let depth = self.recurse_depth;
                    let ext = self.ext_filter.clone();
                    let export = self.export_dir.clone();
                    let (tx, rx) = mpsc::channel::<String>();
                    self.rx = Some(rx);
                    thread::spawn(move || {
                        // collect files (optionally recurse with depth)
                        let mut stack: Vec<(PathBuf, i32)> = vec![(src.clone(), depth)];
                        let mut found = Vec::new();
                        while let Some((dir, dleft)) = stack.pop() {
                            if let Ok(entries) = fs::read_dir(&dir) {
                                for e in entries.flatten() {
                                    let p = e.path();
                                    if p.is_dir() && (dleft != 0) {
                                        // if dleft < 0 it's infinite
                                        let next = if dleft > 0 { dleft - 1 } else { dleft };
                                        stack.push((p, next));
                                    } else if p.is_file() {
                                        if let Some(exts) = p.extension().and_then(|s| s.to_str()) {
                                            if exts.eq_ignore_ascii_case(&ext) {
                                                found.push(p);
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        if found.is_empty() {
                            let _ = tx.send("No .dcm files found in selected folder".to_string());
                        } else {
                            for p in found {
                                let fname = p.file_name().and_then(|s| s.to_str()).unwrap_or("file").to_string();
                                let dest = export.join(&fname);
                                if do_move {
                                    // try rename, fallback to copy+remove
                                    match fs::rename(&p, &dest) {
                                        Ok(_) => { let _ = tx.send(format!("Moved {} -> {}", p.display(), dest.display())); }
                                        Err(_) => match fs::copy(&p, &dest) {
                                            Ok(_) => {
                                                let _ = fs::remove_file(&p);
                                                let _ = tx.send(format!("Copied+removed {} -> {}", p.display(), dest.display()));
                                            }
                                            Err(e) => { let _ = tx.send(format!("Failed to move {}: {}", p.display(), e)); }
                                        }
                                    }
                                } else {
                                    match fs::copy(&p, &dest) {
                                        Ok(_) => { let _ = tx.send(format!("Copied {} -> {}", p.display(), dest.display())); }
                                        Err(e) => { let _ = tx.send(format!("Failed to copy {}: {}", p.display(), e)); }
                                    }
                                }
                            }
                        }
                        let _ = tx.send("done".to_string());
                    });
                } else {
                    self.last_msg = "No folder selected".to_string();
                }
            }

            ui.horizontal(|ui| {
                ui.checkbox(&mut self.move_files, "Move files (don\'t keep originals)");
                ui.add(egui::widgets::DragValue::new(&mut self.recurse_depth).clamp_range(-1..=100).speed(1.0));
                ui.label("Recursion depth (-1 = infinite)");
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.notify_on_process, "Notify exporters after processing");
            });
            ui.horizontal(|ui| {
                ui.label("Extension filter:");
                ui.text_edit_singleline(&mut self.ext_filter);
            });

            if ui.button("Upload anonymized files").clicked() {
                let anon_dir = self.export_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")).join("anon");
                let (tx, rx) = mpsc::channel::<String>();
                self.rx = Some(rx);
                thread::spawn(move || {
                    match upload_anon_dir(&anon_dir, None) {
                        Ok(res) => {
                            let _ = tx.send(format!("Uploaded: {}", res.uploaded.len()));
                            let _ = tx.send(format!("Duplicates: {}", res.duplicates.len()));
                            let _ = tx.send(format!("Failed: {}", res.failed.len()));
                        }
                        Err(e) => {
                            let _ = tx.send(format!("Upload failed: {}", e));
                        }
                    }

                    let _ = tx.send("done".to_string());
                });
            }

            if ui.button("Refresh ready-to-upload").clicked() {
                let anon_dir = self.export_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")).join("anon");
                let (tx, rx) = mpsc::channel::<String>();
                self.rx = Some(rx);
                thread::spawn(move || {
                    match scan_for_upload(&anon_dir) {
                        Ok(series) => {
                            // write series summary to temp JSON for GUI to load
                            let list: Vec<(String, Vec<(String,String,bool)>, Vec<String>)> = series.into_iter().map(|s| {
                                let files = s.files.into_iter().map(|f| (f.path.to_string_lossy().to_string(), f.hash, f.is_duplicate)).collect();
                                (s.series_uid, files, s.duplicate_series_urls)
                            }).collect();
                            if let Ok(json) = serde_json::to_string(&list) {
                                let _ = std::fs::write(".last_scan.json", json);
                                let _ = tx.send("scan_written".to_string());
                            } else {
                                let _ = tx.send("scan_serialize_failed".to_string());
                            }
                        }
                        Err(e) => { let _ = tx.send(format!("Scan failed: {}", e)); }
                    }
                    let _ = tx.send("done".to_string());
                });
            }

            if ui.button("Send 'loaded' message").clicked() {
                // placeholder for NNG send
                self.last_msg = "(placeholder) send 'loaded'".to_string();
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.label(format!("Export dir: {}", self.export_dir.display()));
                if ui.small_button("Open").clicked() {
                    // Open folder using the platform default command (explorer / open / xdg-open)
                    let res = if cfg!(target_os = "windows") {
                        let p = self.export_dir.to_string_lossy().to_string().replace('/', "\\");
                        std::process::Command::new("explorer").arg(p).spawn()
                    } else if cfg!(target_os = "macos") {
                        std::process::Command::new("open").arg(self.export_dir.to_string_lossy().to_string()).spawn()
                    } else {
                        std::process::Command::new("xdg-open").arg(self.export_dir.to_string_lossy().to_string()).spawn()
                    };
                    match res {
                        Ok(_) => self.last_msg = format!("Opened {}", self.export_dir.display()),
                        Err(e) => self.last_msg = format!("Failed to open export dir: {}", e),
                    }
                }
            });
            ui.label(format!("Last: {}", self.last_msg));
            ui.separator();
            if let Some(rx) = &self.rx {
                match rx.try_recv() {
                    Ok(m) => {
                        if m == "done" {
                            self.last_msg = "Processing complete".to_string();
                        } else if m == "scan_written" {
                            if let Ok(txt) = std::fs::read_to_string(".last_scan.json") {
                                if let Ok(v) = serde_json::from_str::<Vec<(String, Vec<(String,String,bool)>, Vec<String>)>>(&txt) {
                                    let mut cols = Vec::new();
                                    for (suid, files, urls) in v {
                                        let mut entries = Vec::new();
                                        for (p, h, dup) in files {
                                            entries.push(FileEntry { path: PathBuf::from(p), hash: h, is_duplicate: dup });
                                        }
                                        cols.push(SeriesInfo { series_uid: suid, files: entries, duplicate_series_urls: urls });
                                    }
                                    self.ready_series = cols;
                                    self.selected_series = vec![true; self.ready_series.len()];
                                    self.last_msg = "Ready-to-upload refreshed".to_string();
                                }
                            }
                        } else if m.starts_with("LOGIN_USER:") {
                            if let Some(name) = m.strip_prefix("LOGIN_USER:") {
                                self.logged_in_user = Some(name.to_string());
                                self.last_msg = format!("Logged in as {}", name);
                            }
                        } else {
                            self.processed.push(m.clone());
                            self.last_msg = m;
                        }
                    }
                    Err(_) => {}
                }
            }

            if !self.processed.is_empty() {
                ui.collapsing("Processed items", |ui| {
                    egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        for it in &self.processed {
                            ui.label(it);
                        }
                    });
                });
            }

            ui.separator();
            ui.collapsing("Ready to Upload", |ui| {
                egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if !self.metadata_select_mode {
                            if ui.button("Compare selected metadata").clicked() {
                                self.metadata_select_mode = true;
                                self.selected_files_for_meta.clear();
                                self.last_msg = "Select files for metadata compare".to_string();
                            }
                        } else {
                            if ui.button("Launch metadata viewer").clicked() {
                                let mut paths: Vec<String> = Vec::new();
                                for series in &self.ready_series {
                                    for f in &series.files {
                                        let pstr = f.path.to_string_lossy().to_string();
                                        if self.selected_files_for_meta.contains(&pstr) {
                                            paths.push(pstr);
                                        }
                                    }
                                }
                                if paths.is_empty() {
                                    self.last_msg = "No files selected for metadata compare".to_string();
                                } else {
                                    match std::env::current_exe() {
                                        Ok(exe) => {
                                            let mut cmd = Command::new(exe);
                                            cmd.arg("--meta-view");
                                            for p in paths { cmd.arg(p); }
                                            match cmd.spawn() {
                                                Ok(_) => { self.last_msg = "Launched metadata viewer".to_string(); self.metadata_select_mode = false; }
                                                Err(e) => { self.last_msg = format!("Failed to launch metadata viewer: {}", e); }
                                            }
                                        }
                                        Err(e) => { self.last_msg = format!("Failed to find executable: {}", e); }
                                    }
                                }
                            }
                            if ui.button("Cancel compare").clicked() {
                                self.metadata_select_mode = false;
                                self.selected_files_for_meta.clear();
                                self.last_msg = "Metadata compare cancelled".to_string();
                            }
                        }
                    });
                    for (si, series) in self.ready_series.iter().enumerate() {
                        let mut checked = *self.selected_series.get(si).unwrap_or(&true);
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut checked, format!("Series: {} ({} files)", series.series_uid, series.files.len())).changed() {
                                if si < self.selected_series.len() { self.selected_series[si] = checked; }
                            }
                        });
                        if !series.duplicate_series_urls.is_empty() {
                            ui.colored_label(egui::Color32::YELLOW, format!("{} duplicate(s) found on server", series.duplicate_series_urls.len()));
                            for url in &series.duplicate_series_urls {
                                ui.hyperlink(url);
                            }
                        }
                        ui.indent(format!("files-{}", si), |ui| {
                            for f in &series.files {
                                ui.horizontal(|ui| {
                                    // selection checkbox for metadata compare (visible only in selection mode)
                                    let pstr = f.path.to_string_lossy().to_string();
                                    if self.metadata_select_mode {
                                        let mut sel = self.selected_files_for_meta.contains(&pstr);
                                        if ui.checkbox(&mut sel, "").changed() {
                                            if sel { self.selected_files_for_meta.insert(pstr.clone()); } else { self.selected_files_for_meta.remove(&pstr); }
                                        }
                                    } else {
                                        ui.add_space(16.0);
                                    }
                                    if f.is_duplicate {
                                        ui.colored_label(egui::Color32::LIGHT_RED, "DUP");
                                    }
                                    ui.label(f.path.file_name().and_then(|s| s.to_str()).unwrap_or("file"));
                                    if ui.small_button("View meta").clicked() {
                                        // launch standalone viewer for a single file
                                        match std::env::current_exe() {
                                            Ok(exe) => {
                                                match Command::new(exe).arg("--meta-view").arg(pstr.clone()).spawn() {
                                                    Ok(_) => { self.last_msg = format!("Opened metadata viewer for {}", f.path.display()); }
                                                    Err(e) => { self.last_msg = format!("Failed to spawn viewer: {}", e); }
                                                }
                                            }
                                            Err(e) => { self.last_msg = format!("Failed to locate executable: {}", e); }
                                        }
                                    }
                                    ui.label(format!("hash: {}", f.hash));
                                });
                            }
                        });
                        ui.separator();
                    }
                });
            });

            // Metadata single-view window
            if self.metadata_window_open {
                if let Some((title, map)) = &self.metadata_single {
                    egui::Window::new(format!("Metadata: {}", title)).open(&mut self.metadata_window_open).show(ctx, |ui| {
                        egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                            for (k, v) in map {
                                ui.horizontal(|ui| { ui.label(format!("{}:", k)); ui.label(v); });
                            }
                        });
                    });
                }
            }

            // Metadata compare window (side-by-side)
            if self.metadata_compare_open {
                let comps = self.metadata_compare.clone();
                egui::Window::new("Compare metadata").open(&mut self.metadata_compare_open).show(ctx, |ui| {
                    if comps.is_empty() {
                        ui.label("No files to compare");
                        return;
                    }
                    // build union of keys
                    let mut keys: Vec<String> = Vec::new();
                    let mut keyset: HashSet<String> = HashSet::new();
                    for (_name, map) in &comps {
                        for k in map.keys() {
                            if !keyset.contains(k) { keyset.insert(k.clone()); keys.push(k.clone()); }
                        }
                    }
                    // header row
                    ui.horizontal(|ui| {
                        ui.label("");
                        for (name, _map) in &comps {
                            ui.label(name);
                        }
                    });
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(600.0).show(ui, |ui| {
                        for k in &keys {
                            ui.horizontal(|ui| {
                                ui.label(k);
                                // collect values for this key
                                let mut vals: Vec<Option<String>> = Vec::new();
                                for (_name, map) in &comps {
                                    vals.push(map.get(k).cloned());
                                }
                                // detect differences
                                let mut same = true;
                                let mut prev: Option<&String> = None;
                                for v in &vals {
                                    if let Some(s) = v {
                                        if let Some(pv) = prev { if pv != s { same = false; break; } } else { prev = Some(s); }
                                    } else {
                                        if prev.is_some() { same = false; break; }
                                    }
                                }
                                for v in vals {
                                    if same {
                                        ui.label(v.unwrap_or_default());
                                    } else {
                                        ui.colored_label(egui::Color32::YELLOW, v.unwrap_or_default());
                                    }
                                }
                            });
                            ui.separator();
                        }
                    });
                });
            }
        });
    }
}

fn main() {
    // if started with --meta-view, run the separate metadata viewer window and exit
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 && args[1] == "--meta-view" {
        let paths: Vec<String> = args.iter().skip(2).cloned().collect();
        run_meta_viewer(paths);
        return;
    }
    // if started with --anon, anonymize a single file and write to the given output path
    if args.len() >= 4 && args[1] == "--anon" {
        let in_path = std::path::Path::new(&args[2]);
        let out_path = std::path::Path::new(&args[3]);
        match anonymizer::anonymize_file(in_path, out_path.parent().unwrap_or_else(||std::path::Path::new(".")), false, None) {
            Ok(p) => {
                // if anonymizer wrote a file with same name under output dir, move/rename to requested path
                if p != out_path {
                    let _ = std::fs::rename(&p, out_path);
                }
                println!("OK:{}", out_path.display());
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("ERROR:{}", e);
                std::process::exit(2);
            }
        }
    }
    let native_options = NativeOptions::default();
    let _ = eframe::run_native("Uploader (Rust)", native_options, Box::new(|_cc| {
        // create app and a channel for background notifications (NNG and tasks)
        let mut app = AppState::default();
        let (tx, rx) = mpsc::channel::<String>();
        app.rx = Some(rx);

        // Initial scan for existing anonymised files to show ready-to-upload series
        let anon_dir = app.export_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")).join("anon");
        match scan_for_upload(&anon_dir) {
            Ok(series) => {
                app.ready_series = series;
                app.selected_series = vec![true; app.ready_series.len()];
                app.last_msg = format!("Loaded {} series from {}", app.ready_series.len(), anon_dir.display());
            }
            Err(e) => {
                app.last_msg = format!("Initial scan failed: {}", e);
            }
        }

        // Spawn NNG listener thread to accept notifications from exporter app.
        // Binds to tcp://127.0.0.1:9976 and forwards received messages to the GUI via `tx`.
        let tx_clone = tx.clone();
        thread::spawn(move || {
            match Socket::new(Protocol::Pair0) {
                Ok(s) => {
                    if let Err(e) = s.listen("tcp://127.0.0.1:9976") {
                        let _ = tx_clone.send(format!("NNG bind failed: {:?}", e));
                        return;
                    }
                    let _ = tx_clone.send("NNG listener bound on tcp://127.0.0.1:9976".to_string());
                    loop {
                        match s.recv() {
                            Ok(msg) => {
                                // try interpret as utf8, fallback to hex
                                let text = match std::str::from_utf8(&msg) {
                                    Ok(t) => t.to_string(),
                                    Err(_) => format!("<bin:{} bytes>", msg.len()),
                                };
                                let _ = tx_clone.send(format!("NNG msg: {}", text));
                                // reply ack
                                let _ = s.send(&b"ack"[..]);
                            }
                            Err(e) => {
                                let _ = tx_clone.send(format!("NNG recv error: {:?}", e));
                                std::thread::sleep(std::time::Duration::from_millis(200));
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = tx_clone.send(format!("Failed to create NNG socket: {:?}", e));
                }
            }
        });

        Box::new(app)
    }));
}
