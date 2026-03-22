use eframe::egui;
use std::fs;
use crossbeam_channel::{unbounded, Receiver};
use anyhow::Result;
use serde_json::Value;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

mod speech;
mod templates;
mod dragon_ipc;
mod vim;
use speech::{create_vosk_engine, SpeechEngine};

use std::ops::Range;
use egui::text::{CCursor, CCursorRange};

#[derive(PartialEq, Eq, Serialize, Deserialize, Clone, Copy, Debug)]
enum VimMode {
    Normal,
    Insert,
    Visual,
}

impl Default for VimMode {
    fn default() -> Self {
        VimMode::Normal
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct Settings {
    vim_enabled: bool,
    vim_mode: VimMode,
    caret_x_offset: f32,
    show_caret_debug: bool,
    overlay_x: i32,
    overlay_y: i32,
    overlay_w: i32,
    overlay_h: i32,
}

fn settings_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("diforge-rs");
        p.push("settings.json");
        p
    } else if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config/diforge-rs/settings.json");
        p
    } else {
        PathBuf::from("./diforge-settings.json")
    }
}

fn load_settings() -> Settings {
    let p = settings_path();
    if let Ok(txt) = fs::read_to_string(&p) {
        if let Ok(s) = serde_json::from_str::<Settings>(&txt) {
            return s;
        }
    }
    Settings::default()
}

fn save_settings(s: &Settings) -> Result<()> {
    let p = settings_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let txt = serde_json::to_string_pretty(s)?;
    fs::write(p, txt)?;
    Ok(())
}

struct ReportApp {
    buffer: vim::ReportBuffer,
    templates: Vec<templates::Template>,
    // speech
    engine: Box<dyn SpeechEngine>,
    dictating: bool,
    interim: String,
    rx: Option<Receiver<String>>,
    ipc_writers: Option<dragon_ipc::SharedWriters>,
    // templates UI state
    show_templates_window: bool,
    template_search: String,
    template_nicip: String,
    selected_template: Option<usize>,
    attach_requested: bool,
    // overlay position (for helper)
    overlay_x: i32,
    overlay_y: i32,
    overlay_w: i32,
    overlay_h: i32,
    // (now in `buffer`) last known caret/selection as char indices
    // debug flag to show caret diagnostics
    show_caret_debug: bool,
    // manual pixel offset to correct caret X position when needed (debug tweak)
    caret_x_offset: f32,
    // enable modal vim-like keybindings
    vim_enabled: bool,
    // current vim mode
    vim_mode: VimMode,
    // last key pressed (for multi-key commands like dd)
    last_vim_key: Option<char>,
}

impl Default for ReportApp {
    fn default() -> Self {
        // start IPC listener for Dragon helper
        let (tx, rx_local) = crossbeam_channel::unbounded();
        let ipc_writers = dragon_ipc::start_listener(tx);

        let mut app = Self {
            templates: templates::load_templates(),
            engine: create_vosk_engine("models/vosk-small").unwrap_or_else(|_| create_vosk_engine("").unwrap()),
            dictating: false,
            interim: String::new(),
            rx: Some(rx_local),
            // store writers handle so we can send commands
            ipc_writers: Some(ipc_writers),
            show_templates_window: true,
            template_search: String::new(),
            template_nicip: String::new(),
            selected_template: None,
            attach_requested: false,
            overlay_x: 100,
            overlay_y: 100,
            overlay_w: 600,
            overlay_h: 200,
            buffer: vim::ReportBuffer::new(),
            show_caret_debug: false,
            caret_x_offset: 3.0,
            vim_enabled: false,
            vim_mode: VimMode::Normal,
            last_vim_key: None,
        };

        // Load persisted settings (if any) and apply
        let settings = load_settings();
        app.apply_settings(settings);

        // Add default multiline text for testing if the report is empty
        if app.buffer.report.is_empty() {
            let sample = "Patient: John Doe\nDOB: 1970-01-01\nStudy: CT Head\n\nFindings:\n- No acute intracranial hemorrhage.\n- Mild chronic microvascular ischemic change.\n\nImpression:\n1. No acute intracranial hemorrhage.\n2. Chronic microvascular ischemic change.\n";
            app.buffer.report = sample.to_string();
            let pos = app.buffer.report.chars().count();
            app.buffer.caret_char_range = Some(pos..pos);
        }

        app
    }
}

impl ReportApp {
    fn to_settings(&self) -> Settings {
        Settings {
            vim_enabled: self.vim_enabled,
            vim_mode: self.vim_mode,
            caret_x_offset: self.caret_x_offset,
            show_caret_debug: self.show_caret_debug,
            overlay_x: self.overlay_x,
            overlay_y: self.overlay_y,
            overlay_w: self.overlay_w,
            overlay_h: self.overlay_h,
        }
    }

    fn apply_settings(&mut self, s: Settings) {
        self.vim_enabled = s.vim_enabled;
        self.vim_mode = s.vim_mode;
        self.caret_x_offset = s.caret_x_offset;
        self.show_caret_debug = s.show_caret_debug;
        self.overlay_x = s.overlay_x;
        self.overlay_y = s.overlay_y;
        self.overlay_w = s.overlay_w;
        self.overlay_h = s.overlay_h;
    }
}

impl eframe::App for ReportApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // drain any IPC messages (e.g., from Dragon helper) and insert into report buffer
        if let Some(rx) = &self.rx {
            while let Ok(msg) = rx.try_recv() {
                if !self.buffer.report.is_empty() && !self.buffer.report.ends_with('\n') {
                    self.buffer.report.push('\n');
                }
                self.buffer.report.push_str(&msg);
            }
        }
        // (removed SidePanel overlay; templates render inline when requested)

        egui::CentralPanel::default().show(ctx, |ui| {
            // Build a two-column layout: left = main report area, right = templates (fixed width)
            let avail = ui.available_rect_before_wrap();
            let avail_w = avail.width();
            let avail_h = avail.height();
            let right_w = 320.0_f32.min(avail_w.max(0.0));
            let left_w = (avail_w - right_w).max(180.0);

            let left_rect = egui::Rect::from_min_max(avail.min, egui::pos2(avail.min.x + left_w, avail.max.y));
            let right_rect = egui::Rect::from_min_max(egui::pos2(avail.min.x + left_w, avail.min.y), avail.max);

            ui.allocate_ui_at_rect(left_rect, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        if ui.button("New").clicked() {
                            self.buffer.report.clear();
                            self.buffer.caret_char_range = Some(0..0);
                        }
                        if ui.button("Open...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_file() {
                                if let Ok(txt) = fs::read_to_string(path) {
                                    self.buffer.report = txt;
                                }
                            }
                        }
                        if ui.button("Save...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().save_file() {
                                let _ = fs::write(path, &self.buffer.report);
                            }
                        }
                        if ui.button("Attach Dragon Overlay").clicked() {
                            self.attach_requested = true;
                        }
                        ui.separator();
                        // Clone templates to avoid borrowing `self` across UI calls so we can mutably borrow later
                        let templates_top = self.templates.clone();
                        for (i, t) in templates_top.iter().enumerate() {
                            if ui.small_button(format!("T{}", i + 1)).clicked() {
                                    // Insert at caret (or append)
                                    let mut body = t.body.clone();
                                    if !self.buffer.report.is_empty() && !self.buffer.report.ends_with('\n') && self.buffer.caret_char_range.is_none() {
                                        // if no caret known, preserve previous append behavior and add newline
                                        body = format!("\n{}", body);
                                    }
                                    self.buffer.insert_at_caret(&body);
                                }
                        }
                    });

                    ui.separator();

                    ui.label("Radiology report");
                    // Show the editable report and capture TextEdit output so we can track/store the cursor.
                    // If Vim emulation is enabled and we are NOT in Insert mode, prevent the TextEdit
                    // from applying direct edits by restoring any accidental changes.
                    let prev_report = self.buffer.report.clone();
                    let mut text_edit = egui::TextEdit::multiline(&mut self.buffer.report)
                        .desired_rows(20)
                        .desired_width(left_w)
                        // only lock focus when in Insert mode so Normal mode can intercept keys
                        .lock_focus(self.vim_enabled && self.vim_mode == VimMode::Insert);

                    // Use monospace font when Vim emulation is active for consistent fixed-width behavior
                    if self.vim_enabled {
                        text_edit = text_edit.font(egui::TextStyle::Monospace);
                    }

                    let mut output = text_edit.show(ui);

                    // Update stored caret char range from the widget's reported cursor_range
                    if let Some(ccr) = output.cursor_range {
                        let sorted = ccr.as_sorted_char_range();
                        self.buffer.caret_char_range = Some(sorted);
                    }

                    // If vim emulation is active but we're not in Insert mode, revert any
                    // direct text changes the TextEdit may have applied (so Normal mode
                    // keystrokes are handled by our modal logic instead).
                    if self.vim_enabled && self.vim_mode != VimMode::Insert && self.buffer.report != prev_report {
                        self.buffer.report = prev_report;
                    }

                    // Vim emulation: intercept global events when enabled
                    if self.vim_enabled {
                        use egui::Event;

                        let events = ctx.input(|i| i.events.clone());
                        for ev in events.iter() {
                            match ev {
                                Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                                    // Escape always returns to Normal mode
                                    self.vim_mode = VimMode::Normal;
                                    self.last_vim_key = None;
                                }
                                Event::Text(text) => {
                                    if text.is_empty() {
                                        continue;
                                    }
                                    let ch = text.chars().next().unwrap();
                                    match self.vim_mode {
                                        VimMode::Normal => {
                                            match ch {
                                                    'i' => {
                                                        self.vim_mode = VimMode::Insert;
                                                        // request focus immediately and restore cursor state
                                                        output.response.request_focus();
                                                        if let Some(range) = &self.buffer.caret_char_range {
                                                            let start = CCursor::new(range.start);
                                                            let end = CCursor::new(range.end);
                                                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                                                            output.state.clone().store(ctx, output.response.id);
                                                        } else {
                                                            // place caret at end
                                                            let pos = self.buffer.report.chars().count();
                                                            let start = CCursor::new(pos);
                                                            output.state.cursor.set_char_range(Some(CCursorRange::one(start)));
                                                            output.state.clone().store(ctx, output.response.id);
                                                        }
                                                    }
                                                    'a' => {
                                                        // append: move right one then enter insert
                                                        self.buffer.move_caret_by(1);
                                                        self.vim_mode = VimMode::Insert;
                                                        output.response.request_focus();
                                                        if let Some(range) = &self.buffer.caret_char_range {
                                                            let start = CCursor::new(range.start);
                                                            let end = CCursor::new(range.end);
                                                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                                                            output.state.clone().store(ctx, output.response.id);
                                                        }
                                                    }
                                                    'A' => {
                                                        // append at end of line: use shared helper
                                                        self.buffer.append_at_end_of_line();
                                                        self.vim_mode = VimMode::Insert;
                                                        output.response.request_focus();
                                                        if let Some(range) = &self.buffer.caret_char_range {
                                                            let start = CCursor::new(range.start);
                                                            let end = CCursor::new(range.end);
                                                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                                                            output.state.clone().store(ctx, output.response.id);
                                                        }
                                                    }
                                                    'I' => {
                                                        // insert at start of line
                                                        let (s, _e, _c) = self.buffer.move_to_line_bounds();
                                                        self.buffer.set_caret_pos(s);
                                                        self.vim_mode = VimMode::Insert;
                                                        output.response.request_focus();
                                                        if let Some(range) = &self.buffer.caret_char_range {
                                                            let start = CCursor::new(range.start);
                                                            let end = CCursor::new(range.end);
                                                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                                                            output.state.clone().store(ctx, output.response.id);
                                                        }
                                                    }
                                                    'o' => {
                                                        // open new line below: use shared helper
                                                        self.buffer.open_line_below();
                                                        self.vim_mode = VimMode::Insert;
                                                        output.response.request_focus();
                                                        if let Some(range) = &self.buffer.caret_char_range {
                                                            let start = CCursor::new(range.start);
                                                            let end = CCursor::new(range.end);
                                                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                                                            output.state.clone().store(ctx, output.response.id);
                                                        }
                                                    }
                                                    'O' => {
                                                        // open new line above: use shared helper
                                                        self.buffer.open_line_above();
                                                        self.vim_mode = VimMode::Insert;
                                                        output.response.request_focus();
                                                        if let Some(range) = &self.buffer.caret_char_range {
                                                            let start = CCursor::new(range.start);
                                                            let end = CCursor::new(range.end);
                                                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                                                            output.state.clone().store(ctx, output.response.id);
                                                        }
                                                    }
                                                    'h' => { self.buffer.move_caret_by(-1); }
                                                    'l' => { self.buffer.move_caret_by(1); }
                                                    'j' => { self.buffer.move_line_down(); }
                                                    'k' => { self.buffer.move_line_up(); }
                                                    'w' => { self.buffer.move_word_forward(); }
                                                    'b' => { self.buffer.move_word_backward(); }
                                                    'e' => { self.buffer.move_word_end(); }
                                                    '0' => { let (s, _e, _c) = self.buffer.move_to_line_bounds(); self.buffer.set_caret_pos(s); }
                                                    '$' => { let (_s, e, _c) = self.buffer.move_to_line_bounds(); if e>0 { self.buffer.set_caret_pos(e.saturating_sub(1)); } }
                                                'x' => { self.buffer.delete_char_at_cursor(); }
                                                'd' => {
                                                    if self.last_vim_key == Some('d') {
                                                        self.buffer.delete_current_line();
                                                        self.last_vim_key = None;
                                                    } else {
                                                        self.last_vim_key = Some('d');
                                                    }
                                                }
                                                _ => {
                                                    // unhandled normal-mode key
                                                    self.last_vim_key = None;
                                                }
                                            }
                                        }
                                        VimMode::Insert => {
                                            // in Insert mode, normal text events are handled by TextEdit; we only intercept Escape above
                                        }
                                        VimMode::Visual => {
                                            // not implemented yet
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    // If we already have a desired caret position (e.g. from an insert earlier this frame), push it into widget state
                    if let Some(range) = &self.buffer.caret_char_range {
                        let start = CCursor::new(range.start);
                        let end = CCursor::new(range.end);
                        output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                        output.state.store(ui.ctx(), output.response.id);
                    }

                    // Draw an unfocused caret indicator so the user can see the caret when the editor is not focused.
                    if !output.response.has_focus() {
                        // Try to use the widget-reported cursor_range, otherwise fall back to our stored `caret_char_range`.
                        let maybe_ccr = output.cursor_range.or_else(|| {
                            self.buffer.caret_char_range.as_ref().map(|r| CCursorRange::two(CCursor::new(r.start), CCursor::new(r.end)))
                        });

                        if let Some(ccr) = maybe_ccr {
                            // Use the primary cursor position
                            let ccursor = ccr.primary;
                            // Position inside the laid-out galley (pos_from_cursor returns a Rect)
                            let galley_pos = output.galley.pos_from_cursor(ccursor);
                            // Convert to screen coords: widget rect min + galley_pos offset
                            // Note: `galley_pos` is already positioned relative to the widget; avoid double-adding `output.galley_pos`.
                            let screen_pos = output.response.rect.min + galley_pos.min.to_vec2();
                            // Draw a visible caret as a filled rectangle the width of a character.
                            let caret_height = 18.0_f32;
                            let painter = ui.painter();
                            // Use the galley cursor rect width as a best-effort char width; fallback to 8.0
                            let mut char_w = (galley_pos.max.x - galley_pos.min.x).abs();
                            if char_w <= 0.1 {
                                char_w = 8.0;
                            }
                            // Nudge the caret slightly right for visual alignment
                            let nudge = 2.0_f32;
                            let x = (screen_pos.x + self.caret_x_offset + nudge).clamp(output.response.rect.min.x, output.response.rect.max.x - 1.0);
                            let y0 = screen_pos.y.clamp(output.response.rect.min.y, output.response.rect.max.y - caret_height);
                            let y1 = y0 + caret_height;
                            let x2 = (x + char_w).min(output.response.rect.max.x - 1.0);
                            let rect = egui::Rect::from_min_max(egui::pos2(x, y0), egui::pos2(x2, y1));
                            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(255, 80, 80));
                            if self.show_caret_debug {
                                painter.circle_filled(egui::pos2(x, y0), 4.0, egui::Color32::from_rgb(80, 200, 255));
                                let info = format!("screen: {:.1},{:.1}  char_range: {:?}", screen_pos.x + self.caret_x_offset, screen_pos.y, self.buffer.caret_char_range);
                                painter.text(
                                    egui::pos2(output.response.rect.min.x + 6.0, output.response.rect.min.y + 6.0),
                                    egui::Align2::LEFT_TOP,
                                    info,
                                    egui::FontId::proportional(12.0),
                                    egui::Color32::WHITE,
                                );
                            }
                        }
                    }

                    if self.attach_requested {
                        if let Some(writers) = &self.ipc_writers {
                            let rect = output.response.rect;
                            let x = rect.min.x.round() as i32;
                            let y = rect.min.y.round() as i32;
                            let w = rect.width().round() as i32;
                            let h = rect.height().round() as i32;
                            let cmd = serde_json::json!({
                                "cmd": "show_overlay",
                                "text": self.buffer.report,
                                "x": x,
                                "y": y,
                                "w": w,
                                "h": h,
                            });
                            dragon_ipc::send_to_helpers(writers, &cmd);
                        }
                        self.attach_requested = false;
                    }

                    // Show caret position below the text edit so user always sees it
                    if let Some(range) = &self.buffer.caret_char_range {
                        ui.label(format!("Caret: {}{}",
                            range.end,
                            if range.start != range.end { format!(" (sel {}..{})", range.start, range.end) } else { String::new() }
                        ));
                    } else {
                        ui.label("Caret: -");
                    }

                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.show_caret_debug, "Debug caret pos");
                        let vim_resp = ui.checkbox(&mut self.vim_enabled, "Vim emulation");
                        if vim_resp.changed() {
                            if self.vim_enabled {
                                self.vim_mode = VimMode::Normal;
                                self.last_vim_key = None;
                            }
                            // persist settings when user toggles Vim emulation
                            let _ = save_settings(&self.to_settings());
                        }
                        let mode_label = match self.vim_mode {
                            VimMode::Normal => "Normal",
                            VimMode::Insert => "Insert",
                            VimMode::Visual => "Visual",
                        };
                        ui.label(format!("Mode: {}", mode_label));
                    });
                    if self.show_caret_debug {
                        ui.add(egui::Slider::new(&mut self.caret_x_offset, -40.0..=40.0).text("Caret X offset"));
                    }

                    ui.horizontal(|ui| {
                        if ui.button("Preview").clicked() {
                            ctx.request_repaint();
                        }
                        if ui.button("Insert Template").clicked() {
                            self.show_templates_window = true;
                        }
                    });

                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label("Overlay X:");
                        ui.add(egui::DragValue::new(&mut self.overlay_x));
                        ui.label("Y:");
                        ui.add(egui::DragValue::new(&mut self.overlay_y));
                        ui.label("W:");
                        ui.add(egui::DragValue::new(&mut self.overlay_w));
                        ui.label("H:");
                        ui.add(egui::DragValue::new(&mut self.overlay_h));
                        if ui.button("Send Overlay Position").clicked() {
                            if let Some(writers) = &self.ipc_writers {
                                let cmd = serde_json::json!({
                                    "cmd": "set_overlay_position",
                                    "x": self.overlay_x,
                                    "y": self.overlay_y,
                                    "w": self.overlay_w,
                                    "h": self.overlay_h
                                });
                                dragon_ipc::send_to_helpers(writers, &cmd);
                            }
                        }
                    });
                });
            });

            // Templates area: render in the right column when requested
            if self.show_templates_window {
                ui.allocate_ui_at_rect(right_rect, |ui| {
                    ui.vertical(|ui| {
                        ui.heading("Templates");
                        ui.separator();

                        ui.horizontal(|ui| {
                            ui.label("NICIP codes (comma-separated):");
                            ui.text_edit_singleline(&mut self.template_nicip);
                        });

                        ui.horizontal(|ui| {
                            ui.label("Search:");
                            ui.text_edit_singleline(&mut self.template_search);
                            if ui.button("Hide").clicked() {
                                self.show_templates_window = false;
                            }
                        });

                        let nicips: Vec<String> = self
                            .template_nicip
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();

                        // Clone templates to avoid immutable borrow across UI closures.
                        let templates_list = self.templates.clone();

                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for (i, t) in templates_list.iter().enumerate() {
                                if !templates::matches_template(t, &nicips, None) {
                                    continue;
                                }
                                let title = t.display_title();
                                if !self.template_search.is_empty()
                                    && !title.to_lowercase().contains(&self.template_search.to_lowercase())
                                    && !t.body.to_lowercase().contains(&self.template_search.to_lowercase())
                                {
                                    continue;
                                }
                                ui.horizontal(|ui| {
                                    if ui.small_button("Insert").clicked() {
                                        let vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
                                        let rendered = templates::render_template(&t.body, &vars);
                                        let mut body = rendered.clone();
                                                    if !self.buffer.report.is_empty() && !self.buffer.report.ends_with('\n') && self.buffer.caret_char_range.is_none() {
                                                        body = format!("\n{}", body);
                                                    }
                                                    self.buffer.insert_at_caret(&body);
                                        // After inserting ensure widget state will be updated next frame by the TextEdit output handling
                                    }
                                    ui.label(title);
                                });
                            }
                        });
                    });
                });
            }
        });
    }
}

fn main() {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Diforge — Radiology Report Editor",
        native_options,
        Box::new(|_cc| Ok(Box::new(ReportApp::default()))),
    );
}
