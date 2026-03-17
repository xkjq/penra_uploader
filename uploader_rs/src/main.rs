use eframe::{egui, NativeOptions};
use egui::CentralPanel;
use dicor_rs::anonymize_file;
mod upload;
use upload::{upload_anon_dir, UploadResult, scan_for_upload, SeriesInfo, FileEntry};
use dicom_viewer::{read_metadata, read_metadata_all};
use divue_rs::run_meta_viewer;
use std::collections::{HashMap, HashSet};
use std::process::{Command, Stdio};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use blake3;
use std::fs;
use rfd::FileDialog;
use nng::{Protocol, Socket};
use chrono::Utc;

fn human_size(bytes: u64) -> String {
    if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{} B", bytes)
    }
}

struct AppState {
    last_msg: String,
    export_dir: PathBuf,
    rx: Option<Receiver<String>>,
    // processing progress/state shown in UI
    processing_step: Option<String>,
    processing_progress: f32,
    // shared sender for background tasks -> GUI
    tx: Option<mpsc::Sender<String>>,
    processed: Vec<String>,
    seed: Option<String>,
    username: String,
    password: String,
    logged_in_user: Option<String>,
    move_files: bool,
    recurse_depth: i32,
    ext_filter: String,
    notify_on_process: bool,
    login_open: bool,
    ready_series: Vec<SeriesInfo>,
    selected_series: Vec<bool>,
    base_url_mode: i32,
    custom_base_url: String,
    skip_ssl: bool,
    // metadata viewer state
    metadata_window_open: bool,
    metadata_compare_open: bool,
    metadata_single: Option<(String, HashMap<String,String>)>,
    metadata_compare: Vec<(String, HashMap<String,String>)>,
    selected_files_for_meta: HashSet<String>,
    metadata_select_mode: bool,
    log_window_open: bool,
    confirm_remove_all: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            last_msg: String::new(),
            export_dir: {
                if cfg!(target_os = "windows") {
                    let base = PathBuf::from(r"C:\uploader");
                    let export = base.join("export");
                    let anon = base.join("anon");
                    let _ = std::fs::create_dir_all(&export);
                    let _ = std::fs::create_dir_all(&anon);
                    export
                } else {
                    let export = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("export");
                    let anon = export.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")).join("anon");
                    let _ = std::fs::create_dir_all(&export);
                    let _ = std::fs::create_dir_all(&anon);
                    export
                }
            },
            rx: None,
            processing_step: None,
            processing_progress: 0.0,
            tx: None,
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
            base_url_mode: {
                // decide initial mode from saved config
                let cfg = upload::load_base_url();
                if let Some(s) = cfg {
                    if s == "https://www.penracourses.org.uk" { 0 } else if s == "http://localhost:8080" { 1 } else { 2 }
                } else {
                    0
                }
            },
            custom_base_url: upload::load_base_url().unwrap_or_default(),
            skip_ssl: upload::load_skip_ssl(),
            metadata_window_open: false,
            metadata_compare_open: false,
            metadata_single: None,
            metadata_compare: Vec::new(),
            selected_files_for_meta: HashSet::new(),
            metadata_select_mode: false,
            log_window_open: false,
            login_open: upload::token_username().is_none(),
            confirm_remove_all: false,
        }
    }
}

impl AppState {
    fn spawn_login(&mut self, user: String, pass: String) {
        let tx = match &self.tx {
            Some(t) => t.clone(),
            None => {
                // fall back to a local channel if tx not available
                let (t, _r) = mpsc::channel::<String>();
                t
            }
        };
        thread::spawn(move || {
            let base = upload::base_site_url();
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
    fn anon_dir(&self) -> PathBuf {
        self.export_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")).join("anon")
    }
}

impl eframe::App for AppState {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        CentralPanel::default().show(ctx, |ui| {
            ui.heading("Uploader (Rust)");

            


            egui::CollapsingHeader::new("Login").default_open(self.login_open).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Username:");
                    ui.text_edit_singleline(&mut self.username);
                });
                // password field: pressing Enter should submit
                ui.horizontal(|ui| {
                    ui.label("Password:");
                    let pw_resp = ui.add(egui::widgets::TextEdit::singleline(&mut self.password).password(true));
                    if pw_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let user = self.username.clone();
                        let pass = self.password.clone();
                        self.spawn_login(user, pass);
                    }
                });

                if ui.button("Login").clicked() {
                    let user = self.username.clone();
                    let pass = self.password.clone();
                    self.spawn_login(user, pass);
                }

                if ui.button("Logout").clicked() {
                    if upload::clear_api_token() {
                        self.logged_in_user = None;
                        self.login_open = true;
                        self.last_msg = "Logged out".to_string();
                    } else {
                        self.last_msg = "Failed to clear token".to_string();
                    }
                }

                ui.label(format!("Logged in: {}", self.logged_in_user.clone().unwrap_or_else(|| "no".to_string())));
            });

            ui.collapsing("Processing", |ui| {
                if let Some(step) = &self.processing_step {
                    ui.label(format!("Step: {}", step));
                    ui.add(egui::ProgressBar::new(self.processing_progress).show_percentage());
                }
                if ui.button("Process export (anonymize + notify)").clicked() {
                    let export = self.export_dir.clone();
                    let anon_dir = export
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join("anon");
                    let tx = match &self.tx { Some(t) => t.clone(), None => { let (t,_r)=mpsc::channel(); t } };
                    let notify_flag = self.notify_on_process;
                    let seed_clone = self.seed.clone();

                    thread::spawn(move || {
                        // iterate files in export and anonymize .dcm files
                        if let Ok(entries) = fs::read_dir(&export) {
                            for ent in entries.flatten() {
                                let p = ent.path();
                                if p.extension().map(|e| e == "dcm").unwrap_or(false) {
                                    match anonymize_file(&p, &anon_dir, true, false, false, seed_clone.as_deref()) {
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

                        if notify_flag {
                            if let Ok(s) = Socket::new(Protocol::Pair0) {
                                if s.dial("tcp://127.0.0.1:9976").is_ok() {
                                    let _ = tx.send("Sent NNG 'loaded'".to_string());
                                } else {
                                    let _ = tx.send("Failed to dial NNG socket".to_string());
                                }
                            } else {
                                let _ = tx.send("Failed to create NNG socket".to_string());
                            }
                        }

                        match scan_for_upload(&anon_dir) {
                            Ok(series) => {
                                if let Ok(json) = serde_json::to_string(&series) {
                                    let _ = std::fs::write(".last_scan.json", json);
                                    let _ = tx.send("scan_written".to_string());
                                }
                            }
                            Err(e) => { let _ = tx.send(format!("Post-process scan failed: {}", e)); }
                        }

                        let _ = tx.send("done".to_string());
                    });
                }

                if ui.button("Import from folder").clicked() {
                    // Pick a source folder and copy .dcm files into the export dir in background
                    if let Some(src) = FileDialog::new().pick_folder() {
                        let do_move = self.move_files;
                        let seed_clone = self.seed.clone();
                        let depth = self.recurse_depth;
                        let ext = self.ext_filter.clone();
                        let export = self.export_dir.clone();
                        let tx = match &self.tx { Some(t) => t.clone(), None => { let (t,_r)=mpsc::channel(); t } };

                        thread::spawn(move || {
                            // collect files (optionally recurse with depth)
                            let mut stack: Vec<(PathBuf, i32)> = vec![(src.clone(), depth)];
                            let mut found: Vec<PathBuf> = Vec::new();
                            let mut copied_files: Vec<PathBuf> = Vec::new();

                            while let Some((dir, dleft)) = stack.pop() {
                                if let Ok(entries) = fs::read_dir(&dir) {
                                    for e in entries.flatten() {
                                        let p = e.path();
                                        if p.is_dir() && (dleft != 0) {
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
                                        match fs::rename(&p, &dest) {
                                            Ok(_) => { let _ = tx.send(format!("Moved {} -> {}", p.display(), dest.display())); copied_files.push(dest.clone()); }
                                            Err(_) => match fs::copy(&p, &dest) {
                                                Ok(_) => {
                                                    let _ = fs::remove_file(&p);
                                                    let _ = tx.send(format!("Copied+removed {} -> {}", p.display(), dest.display()));
                                                    copied_files.push(dest.clone());
                                                }
                                                Err(e) => { let _ = tx.send(format!("Failed to move {}: {}", p.display(), e)); }
                                            }
                                        }
                                    } else {
                                        match fs::copy(&p, &dest) {
                                            Ok(_) => { let _ = tx.send(format!("Copied {} -> {}", p.display(), dest.display())); copied_files.push(dest.clone()); }
                                            Err(e) => { let _ = tx.send(format!("Failed to copy {}: {}", p.display(), e)); }
                                        }
                                    }
                                }

                                // After files have been copied/moved, automatically process them
                                if !copied_files.is_empty() {
                                    let anon_dir = export.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")).join("anon");
                                    for p in &copied_files {
                                        if p.extension().map(|e| e == "dcm").unwrap_or(false) {
                                            match anonymize_file(p, &anon_dir, true, false, false, seed_clone.as_deref()) {
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

                                    // after processing, refresh ready-to-upload by scanning anon dir
                                    match scan_for_upload(&anon_dir) {
                                        Ok(series) => {
                                            if let Ok(json) = serde_json::to_string(&series) {
                                                let _ = std::fs::write(".last_scan.json", json);
                                                let _ = tx.send("scan_written".to_string());
                                            }
                                        }
                                        Err(e) => { let _ = tx.send(format!("Post-import scan failed: {}", e)); }
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

                

                if ui.button("Refresh ready-to-upload").clicked() {
                    let anon_dir = self.export_dir.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")).join("anon");
                        let anon_dir = self.anon_dir();
                    let tx = match &self.tx { Some(t) => t.clone(), None => { let (t,_r)=mpsc::channel(); t } };
                    thread::spawn(move || {
                        match scan_for_upload(&anon_dir) {
                            Ok(series) => {
                                // write series summary to temp JSON for GUI to load
                                if let Ok(json) = serde_json::to_string(&series) {
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
            });
            
            ui.separator();
            if let Some(rx) = &self.rx {
                match rx.try_recv() {
                    Ok(m) => {
                        if m == "done" {
                            self.last_msg = "Processing complete".to_string();
                            self.processing_step = None;
                            self.processing_progress = 0.0;
                        } else if m == "PROC:DONE" {
                            self.last_msg = "Processing complete".to_string();
                            self.processing_step = None;
                            self.processing_progress = 0.0;
                        } else if m == "scan_written" {
                            if let Ok(txt) = std::fs::read_to_string(".last_scan.json") {
                                if let Ok(v) = serde_json::from_str::<Vec<SeriesInfo>>(&txt) {
                                    self.ready_series = v;
                                    self.selected_series = vec![true; self.ready_series.len()];
                                    self.last_msg = "Ready-to-upload refreshed".to_string();
                                }
                            }
                        } else if m.starts_with("duplicates_cleared:") {
                            if let Some(n) = m.strip_prefix("duplicates_cleared:") {
                                if let Ok(num) = n.parse::<usize>() {
                                    self.last_msg = format!("Cleared {} duplicate files", num);
                                }
                            }
                        } else if m.starts_with("LOGIN_USER:") {
                            if let Some(name) = m.strip_prefix("LOGIN_USER:") {
                                self.logged_in_user = Some(name.to_string());
                                self.login_open = false;
                                self.last_msg = format!("Logged in as {}", name);
                            }
                        } else if m.starts_with("PROC:STEP:") {
                            if let Some(step) = m.strip_prefix("PROC:STEP:") {
                                self.processing_step = Some(step.to_string());
                                self.last_msg = step.to_string();
                            }
                        } else if m.starts_with("PROC:PROG:") {
                            if let Some(p) = m.strip_prefix("PROC:PROG:") {
                                if let Ok(v) = p.parse::<f32>() {
                                    self.processing_progress = v.clamp(0.0, 1.0);
                                }
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
                        // Styled Upload button moved here to Ready-to-Upload (prominent)
                        if !self.metadata_select_mode {
                            if ui.add(egui::Button::new("Upload anonymized files").fill(egui::Color32::from_rgb(0,150,60))).clicked() {
                                let anon_dir = self.anon_dir();
                                let tx = match &self.tx { Some(t) => t.clone(), None => { let (t,_r)=mpsc::channel(); t } };
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
                            ui.add_space(8.0);
                            if ui.button("Compare selected metadata").clicked() {
                                self.metadata_select_mode = true;
                                self.selected_files_for_meta.clear();
                                self.last_msg = "Select files for metadata compare".to_string();
                            }
                            ui.add_space(8.0);
                            if ui.small_button("Clear duplicates").clicked() {
                                let anon_dir = self.anon_dir();
                                let tx = match &self.tx { Some(t) => t.clone(), None => { let (t,_r)=mpsc::channel(); t } };
                                thread::spawn(move || {
                                    match scan_for_upload(&anon_dir) {
                                        Ok(series) => {
                                            let mut deleted = 0usize;
                                            for s in &series {
                                                for f in &s.files {
                                                    if f.is_duplicate {
                                                        if std::fs::remove_file(&f.path).is_ok() {
                                                            upload::log_rpc(&format!("Deleted duplicate file: {}", f.path.display()));
                                                            deleted += 1;
                                                        } else {
                                                            upload::log_rpc(&format!("Failed to delete duplicate file: {}", f.path.display()));
                                                        }
                                                    }
                                                }
                                            }
                                            // after deletions, do a fresh scan so GUI reflects current anon dir
                                            match scan_for_upload(&anon_dir) {
                                                Ok(new_series) => {
                                                    if let Ok(json2) = serde_json::to_string(&new_series) {
                                                        let _ = std::fs::write(".last_scan.json", json2);
                                                        let _ = tx.send("scan_written".to_string());
                                                    }
                                                }
                                                Err(e) => { let _ = tx.send(format!("Post-clear scan failed: {}", e)); }
                                            }
                                            let _ = tx.send(format!("duplicates_cleared:{}", deleted));
                                        }
                                        Err(e) => { let _ = tx.send(format!("Clear duplicates failed: {}", e)); }
                                    }
                                    let _ = tx.send("done".to_string());
                                });
                            }
                            ui.add_space(6.0);
                            if ui.small_button("Remove all").clicked() {
                                // ask for confirmation before deleting everything
                                self.confirm_remove_all = true;
                                self.last_msg = "Confirm remove all files...".to_string();
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
                            let header = format!(
                                "Exam: {} — ID: {} — Study: {} — Modality: {} — Series {} — {} files — {}",
                                series.examination.as_deref().or(series.series_description.as_deref()).unwrap_or("-"),
                                series.patient_id.as_deref().unwrap_or("-"),
                                series.study_date.as_deref().unwrap_or("-"),
                                series.modality.as_deref().unwrap_or("-"),
                                series.series_number.as_deref().unwrap_or(&series.series_uid),
                                series.file_count,
                                human_size(series.total_bytes)
                            );
                            if ui.checkbox(&mut checked, header).changed() {
                                if si < self.selected_series.len() { self.selected_series[si] = checked; }
                            }
                            ui.add_space(8.0);
                            // single button per series: either open the duplicate series URL
                            // reported by the server (first entry), or redirect to the
                            // server's uploads page when the series is awaiting import.
                            if !series.duplicate_series_urls.is_empty() {
                                let url = series.duplicate_series_urls.get(0).cloned().unwrap_or_default();
                                if ui.small_button("View on server").clicked() {
                                    if !url.is_empty() {
                                        #[cfg(target_os = "windows")]
                                        let _ = std::process::Command::new("explorer").arg(url.clone()).spawn();
                                        #[cfg(target_os = "macos")]
                                        let _ = std::process::Command::new("open").arg(url.clone()).spawn();
                                        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                                        let _ = std::process::Command::new("xdg-open").arg(url.clone()).spawn();
                                    } else {
                                        self.last_msg = "No duplicate URL available".to_string();
                                    }
                                }
                            } else {
                                // fallback: series likely awaiting import — open the uploads page
                                let base = upload::base_site_url();
                                let uploads = format!("{}/atlas/uploads", base.trim_end_matches('/'));
                                if ui.small_button("Open uploads").clicked() {
                                    #[cfg(target_os = "windows")]
                                    let _ = std::process::Command::new("explorer").arg(uploads.clone()).spawn();
                                    #[cfg(target_os = "macos")]
                                    let _ = std::process::Command::new("open").arg(uploads.clone()).spawn();
                                    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
                                    let _ = std::process::Command::new("xdg-open").arg(uploads.clone()).spawn();
                                }
                            }
                        });
                        if !series.duplicate_series_urls.is_empty() {
                            ui.colored_label(egui::Color32::YELLOW, format!("{} duplicate(s) found on server", series.duplicate_series_urls.len()));
                            // show the first URL (full) as plain text for clarity
                            if let Some(u) = series.duplicate_series_urls.get(0) {
                                ui.label(u);
                            }
                        }
                        // files are hidden by default inside a collapsing header to reduce UI noise
                        egui::CollapsingHeader::new(format!("Files ({})", series.files.len()))
                            .default_open(false)
                            .id_source(format!("files-{}", si))
                            .show(ui, |ui| {
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
                        // Add a "View series" button to launch dicom-view with the series files
                        if ui.small_button("View series").clicked() {
                            // collect file paths for this series
                            let mut paths: Vec<String> = Vec::new();
                            for f in &series.files {
                                paths.push(f.path.to_string_lossy().to_string());
                            }
                            if paths.is_empty() {
                                self.last_msg = "No files in series to view".to_string();
                            } else {
                                // Try to launch `dicom-view` (in PATH) with all file args; fall back to workspace target path
                                let try_spawn = |cmd: &str, args: &[String]| -> Result<std::process::Child, std::io::Error> {
                                    Command::new(cmd).args(args).spawn()
                                };

                                // first try by name
                                match try_spawn("dicom-view", &paths) {
                                    Ok(_) => { self.last_msg = "Launched dicom-view".to_string(); }
                                    Err(_) => {
                                        // try common workspace build locations relative to current dir
                                        let fallback1 = std::env::current_dir()
                                            .ok()
                                            .and_then(|cwd| Some(cwd.join("dicom-view/target/debug/dicom-view")));
                                        let fallback2 = std::env::current_exe()
                                            .ok()
                                            .and_then(|exe| exe.parent().and_then(|p| p.parent()).map(|p| p.join("dicom-view/target/debug/dicom-view")));
                                        let mut launched = false;
                                        if let Some(bin) = fallback1 { if bin.exists() {
                                            if try_spawn(bin.to_string_lossy().as_ref(), &paths).is_ok() { launched = true; }
                                        }}
                                        if !launched {
                                            if let Some(bin) = fallback2 { if bin.exists() {
                                                if try_spawn(bin.to_string_lossy().as_ref(), &paths).is_ok() { launched = true; }
                                            }}
                                        }
                                        if launched {
                                            self.last_msg = "Launched dicom-view (fallback)".to_string();
                                        } else {
                                            self.last_msg = "Failed to launch dicom-view; ensure it is built or in PATH".to_string();
                                        }
                                    }
                                }
                            }
                        }
                        ui.separator();
                    }
                });
            });

            ui.collapsing("Settings", |ui| {
                ui.label("Server settings:");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.base_url_mode, 0, "Production (https://www.penracourses.org.uk)");
                    ui.radio_value(&mut self.base_url_mode, 1, "Development (http://localhost:8080)");
                });
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.base_url_mode, 2, "Custom");
                    ui.text_edit_singleline(&mut self.custom_base_url);
                });
                ui.checkbox(&mut self.skip_ssl, "Disable SSL verification (unsafe)");
                if ui.button("Save Settings").clicked() {
                    let url = match self.base_url_mode {
                        0 => "https://www.penracourses.org.uk".to_string(),
                        1 => "http://localhost:8080".to_string(),
                        _ => self.custom_base_url.clone(),
                    };
                    let ok1 = upload::save_base_url(&url);
                    let ok2 = upload::save_skip_ssl(self.skip_ssl);
                    if ok1 && ok2 {
                        self.last_msg = format!("Saved settings: {} (skip_ssl={})", url, self.skip_ssl);
                    } else {
                        self.last_msg = "Failed to save settings".to_string();
                    }
                }
                ui.label(format!("Current base: {}", upload::base_site_url()));
                ui.horizontal(|ui| {
                    ui.label("Seed:");
                    let mut s = self.seed.clone().unwrap_or_default();
                    if ui.text_edit_singleline(&mut s).changed() {
                        self.seed = if s.is_empty() { None } else { Some(s.clone()) };
                    }
                });
                ui.horizontal(|ui| {
                    if ui.small_button("Show Logs").clicked() { self.log_window_open = !self.log_window_open; }
                    if ui.small_button("Refresh Logs").clicked() { self.last_msg = "Logs refreshed".to_string(); }
                    if ui.small_button("Clear Logs").clicked() {
                        let _ = std::fs::write(upload::log_file_path(), "");
                        self.last_msg = "Cleared logs".to_string();
                    }
                });
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
            });

            ui.label(format!("Last: {}", self.last_msg));

            // Confirmation modal for Remove all
            if self.confirm_remove_all {
                egui::Window::new("Confirm remove all").collapsible(false).resizable(false).show(ctx, |ui| {
                    ui.label("This will permanently delete all anonymised files in the anon directory. This cannot be undone.");
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("Yes, remove all").fill(egui::Color32::from_rgb(180,20,20))).clicked() {
                            let anon_dir = self.anon_dir();
                            let tx = match &self.tx { Some(t) => t.clone(), None => { let (t,_r)=mpsc::channel(); t } };
                            thread::spawn(move || {
                                match scan_for_upload(&anon_dir) {
                                    Ok(series) => {
                                        let mut removed = 0usize;
                                        for s in &series {
                                            for f in &s.files {
                                                if std::fs::remove_file(&f.path).is_ok() {
                                                    upload::log_rpc(&format!("Removed file: {}", f.path.display()));
                                                    removed += 1;
                                                } else {
                                                    upload::log_rpc(&format!("Failed to remove file: {}", f.path.display()));
                                                }
                                            }
                                        }
                                        // after removals, refresh scan so GUI shows empty/updated state
                                        match scan_for_upload(&anon_dir) {
                                            Ok(new_series) => {
                                                if let Ok(json2) = serde_json::to_string(&new_series) {
                                                    let _ = std::fs::write(".last_scan.json", json2);
                                                    let _ = tx.send("scan_written".to_string());
                                                }
                                            }
                                            Err(e) => { let _ = tx.send(format!("Post-remove scan failed: {}", e)); }
                                        }
                                        let _ = tx.send(format!("removed_all:{}", removed));
                                    }
                                    Err(e) => { let _ = tx.send(format!("Remove all failed: {}", e)); }
                                }
                                let _ = tx.send("done".to_string());
                            });
                            self.confirm_remove_all = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm_remove_all = false;
                            self.last_msg = "Remove all cancelled".to_string();
                        }
                    });
                });
            }

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

            // Logs window
            if self.log_window_open {
                egui::Window::new("Request/Response Logs").open(&mut self.log_window_open).show(ctx, |ui| {
                    let p = upload::log_file_path();
                    let contents = std::fs::read_to_string(&p).unwrap_or_else(|_| "(no logs)".to_string());
                    // expand BODY_FILE entries to inline the saved body contents for easier copy
                    let mut display = String::new();
                    for line in contents.lines() {
                        display.push_str(line);
                        display.push('\n');
                        if let Some(idx) = line.find("BODY_FILE:") {
                            let path = line[idx+"BODY_FILE:".len()..].trim();
                            if !path.is_empty() {
                                if let Ok(body) = std::fs::read_to_string(path) {
                                    display.push_str("---- BODY START ----\n");
                                    display.push_str(&body);
                                    if !body.ends_with('\n') { display.push('\n'); }
                                    display.push_str("---- BODY END ----\n");
                                } else {
                                    display.push_str("(failed to read body file)\n");
                                }
                            }
                        }
                    }
                    let mut txt = display;
                    egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                        ui.add(egui::TextEdit::multiline(&mut txt).desired_rows(20).desired_width(ui.available_width()));
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Refresh").clicked() {
                            // force UI to re-open (content read each frame, so nothing else required)
                            self.last_msg = "Logs refreshed".to_string();
                        }
                        if ui.button("Clear").clicked() {
                            let _ = std::fs::write(p.clone(), "");
                            self.last_msg = "Logs cleared".to_string();
                        }
                    });
                });
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
        match anonymize_file(in_path, out_path.parent().unwrap_or_else(||std::path::Path::new(".")), false, false, false, None) {
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
        // ensure log file exists for debug output
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(upload::log_file_path());
        let (tx, rx) = mpsc::channel::<String>();
        app.rx = Some(rx);
        app.tx = Some(tx.clone());

        // Initial scan for existing anonymised files to show ready-to-upload series
        let anon_dir = app.anon_dir();
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
        let export_dir_clone = app.export_dir.clone();
        let anon_dir_clone = app.anon_dir();
        let seed_for_nng = app.seed.clone();
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
                                // Kick off processing: copy-export-then-anonymize in background
                                let tx2 = tx_clone.clone();
                                let export_dir2 = export_dir_clone.clone();
                                let anon_dir2 = anon_dir_clone.clone();
                                let seed2 = seed_for_nng.clone();
                                thread::spawn(move || {
                                    // create a processing dir so we don't race with exporter clearing export
                                    let proc_base = export_dir2.parent().map(|p| p.join("processing")).unwrap_or_else(|| PathBuf::from("processing"));
                                    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
                                    let proc_dir = proc_base.join(ts);
                                    let _ = std::fs::create_dir_all(&proc_dir);
                                    let _ = tx2.send("PROC:STEP:Copying export files".to_string());
                                    // collect files
                                    let mut files: Vec<PathBuf> = Vec::new();
                                    if let Ok(entries) = std::fs::read_dir(&export_dir2) {
                                        for e in entries.flatten() {
                                            let p = e.path();
                                            if p.is_file() {
                                                if p.extension().map(|s| s.to_string_lossy().eq_ignore_ascii_case("dcm")).unwrap_or(false) {
                                                    files.push(p);
                                                }
                                            }
                                        }
                                    }
                                    let total = files.len();
                                    if total == 0 {
                                        let _ = tx2.send("PROC:STEP:No files to process".to_string());
                                        let _ = tx2.send("PROC:DONE".to_string());
                                        return;
                                    }
                                    for (i, p) in files.iter().enumerate() {
                                        let fname = p.file_name().unwrap_or_default().to_os_string();
                                        let dest = proc_dir.join(&fname);
                                        // prefer moving (rename) to avoid copying large files; fallback to copy+remove
                                        match std::fs::rename(&p, &dest) {
                                            Ok(_) => {
                                                let _ = tx2.send(format!("Moved {} -> {}", p.display(), dest.display()));
                                            }
                                            Err(_) => match std::fs::copy(&p, &dest) {
                                                Ok(_) => {
                                                    let _ = std::fs::remove_file(&p);
                                                    let _ = tx2.send(format!("Copied+removed {} -> {}", p.display(), dest.display()));
                                                }
                                                Err(e) => {
                                                    let _ = tx2.send(format!("Failed to move {}: {}", p.display(), e));
                                                }
                                            },
                                        }
                                        let frac = (i as f32 + 1.0) / (total as f32);
                                        let _ = tx2.send(format!("PROC:PROG:{}", frac));
                                    }

                                    let _ = tx2.send("PROC:STEP:Anonymizing copied files".to_string());
                                    for (i, p) in std::fs::read_dir(&proc_dir).unwrap_or_else(|_| std::fs::read_dir(&export_dir2).unwrap()).flatten().enumerate() {
                                        let src = p.path();
                                        if src.extension().map(|s| s.to_string_lossy().eq_ignore_ascii_case("dcm")).unwrap_or(false) {
                                            match anonymize_file(&src, &anon_dir2, true, false, false, seed2.as_deref()) {
                                                Ok(out) => {
                                                    let _ = tx2.send(format!("Anonymized: {}", out.display()));
                                                    // remove the source file from processing dir when anonymization succeeded
                                                    if std::fs::remove_file(&src).is_ok() {
                                                        let _ = tx2.send(format!("Removed processed file: {}", src.display()));
                                                    }
                                                }
                                                Err(e) => {
                                                    let _ = tx2.send(format!("Anon failed {}: {}", src.display(), e));
                                                }
                                            }
                                        }
                                        let frac = (i as f32 + 1.0) / (total as f32);
                                        let _ = tx2.send(format!("PROC:PROG:{}", frac));
                                    }

                                    // after processing, refresh ready-to-upload by scanning anon dir
                                    let _ = tx2.send("PROC:STEP:Refreshing ready-to-upload".to_string());
                                    match scan_for_upload(&anon_dir2) {
                                        Ok(series) => {
                                            if let Ok(json) = serde_json::to_string(&series) {
                                                let _ = std::fs::write(".last_scan.json", json);
                                                let _ = tx2.send("scan_written".to_string());
                                            }
                                        }
                                        Err(e) => { let _ = tx2.send(format!("Post-process scan failed: {}", e)); }
                                    }

                                    // attempt to remove the processing directory if it's now empty
                                    match std::fs::read_dir(&proc_dir) {
                                        Ok(mut rd) => {
                                            if rd.next().is_none() {
                                                if std::fs::remove_dir(&proc_dir).is_ok() {
                                                    let _ = tx2.send(format!("Removed empty processing dir: {}", proc_dir.display()));
                                                }
                                            } else {
                                                let _ = tx2.send(format!("Processing dir not empty: {}", proc_dir.display()));
                                            }
                                        }
                                        Err(_) => {
                                            // if we can't read it, try remove_dir and ignore errors
                                            let _ = std::fs::remove_dir(&proc_dir);
                                        }
                                    }

                                    let _ = tx2.send("PROC:DONE".to_string());
                                });
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

        Ok(Box::new(app))
    }));
}
