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

#[cfg(test)]
mod selection_tests;
#[derive(PartialEq, Eq, Serialize, Deserialize, Clone, Copy, Debug)]
enum VimMode {
    Normal,
    Insert,
    Visual,
    VisualLine,
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

// selection tests moved into src/selection_tests.rs

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
    // template editor state
    show_template_editor: bool,
    editing_template: Option<templates::Template>,
    editing_index: Option<usize>,
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
    // last text-object prefix `i` or `a` when operator-pending
    last_vim_object: Option<char>,
    // visual mode anchor (selection start) in char indices
    visual_anchor: Option<usize>,
    // numeric prefix for vim commands (e.g., `3dw`)
    // mouse drag tracking for click-and-drag selection when TextEdit is non-interactive
    mouse_dragging: bool,
    mouse_drag_anchor: Option<usize>,
    // numeric prefix for vim commands (e.g., `3dw`)
    vim_count: Option<usize>,
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
            show_template_editor: false,
            editing_template: None,
            editing_index: None,
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
            last_vim_object: None,
            visual_anchor: None,
            mouse_dragging: false,
            mouse_drag_anchor: None,
            vim_count: None,
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
                    let is_interactive = !self.vim_enabled || self.vim_mode == VimMode::Insert;
                    let mut text_edit = egui::TextEdit::multiline(&mut self.buffer.report)
                        .desired_rows(20)
                        .desired_width(left_w)
                        // only lock focus when in Insert mode so Normal mode can intercept keys
                        .lock_focus(self.vim_enabled && self.vim_mode == VimMode::Insert)
                        // make the widget non-interactive when Vim is active but not in Insert,
                        // so clicks don't hand keyboard focus to the TextEdit unexpectedly.
                        .interactive(is_interactive);

                    // Use monospace font when Vim emulation is active for consistent fixed-width behavior
                    if self.vim_enabled {
                        text_edit = text_edit.font(egui::TextStyle::Monospace);
                    }

                    let mut output = text_edit.show(ui);

                    // Update stored caret char range from the widget's reported cursor_range
                    // Only trust the widget when the TextEdit is interactive (Insert mode or Vim disabled).
                    if is_interactive {
                        if let Some(ccr) = output.cursor_range {
                            let sorted = ccr.as_sorted_char_range();
                            eprintln!("[dbg] widget.cursor_range -> {:?}", sorted);
                            self.buffer.caret_char_range = Some(sorted);
                        }
                    }

                    // If vim emulation is active but we're not in Insert mode, revert any
                    // direct text changes the TextEdit may have applied (so Normal mode
                    // keystrokes are handled by our modal logic instead).
                    if self.vim_enabled && self.vim_mode != VimMode::Insert && self.buffer.report != prev_report {
                        eprintln!("[dbg] reverting buffer.report due to non-Insert vim mode (widget tried to edit)");
                        self.buffer.report = prev_report;
                    }

                    use egui::Event;

                    // (No visible drag handles; mouse selection is handled via
                    // galley-based mapping and pointer drag logic below.)

                    // Capture events once and use them both for global handling and
                    // vim-specific handling below. Make undo/redo available even
                    // when Vim emulation is disabled by handling Ctrl-Z / Ctrl-Y
                    // here unconditionally.
                    let events = ctx.input(|i| i.events.clone());

                    for ev in events.iter() {
                        if let Event::Key { key, pressed: true, modifiers, .. } = ev {
                            if modifiers.ctrl {
                                match key {
                                    egui::Key::Z => { self.buffer.undo(); }
                                    egui::Key::Y | egui::Key::R => { self.buffer.redo(); }
                                    _ => {}
                                }
                            }
                        }
                    }

                    // Ensure any active undo group is ended on Escape regardless
                    // of whether Vim emulation is enabled. This prevents leaving
                    // grouped edits open when the user presses Escape or when
                    // other UI flows cause focus changes.
                    for ev in events.iter() {
                        if let Event::Key { key: egui::Key::Escape, pressed: true, .. } = ev {
                            self.buffer.end_undo_group();
                        }
                    }

                    // Handle mouse interactions inside the TextEdit widget so
                    // clicks and drag-selections update our vim buffer state
                    // regardless of mode. If the TextEdit is non-interactive
                    // (vim Normal mode) compute the clicked/selected char index
                    // from the laid-out galley so the caret still moves without
                    // giving the widget keyboard focus.
                    for ev in events.iter() {
                        if let Event::PointerButton { pos, pressed, button, .. } = ev {
                            if *button != egui::PointerButton::Primary { continue; }
                            // No special handle hit-testing; fall through to normal logic
                            if !output.response.rect.contains(*pos) { continue; }

                            // Prefer the widget-reported cursor_range when available
                            // but only when the TextEdit is interactive (Insert mode or Vim disabled).
                            if is_interactive {
                                if let Some(ccr) = output.cursor_range {
                                    let sorted = ccr.as_sorted_char_range();
                                    eprintln!("[dbg] pointer button (interactive): widget reported cursor_range -> {:?} pressed={}", sorted, pressed);
                                    self.buffer.caret_char_range = Some(sorted.clone());
                                    self.last_vim_key = None;

                                    if *pressed {
                                        // start drag using the reported cursor position
                                        self.mouse_dragging = true;
                                        self.mouse_drag_anchor = Some(sorted.start);
                                    }

                                    if !*pressed {
                                        if self.vim_enabled {
                                            if sorted.start != sorted.end {
                                                self.vim_mode = VimMode::Visual;
                                                self.visual_anchor = Some(sorted.start);
                                            } else if self.vim_mode == VimMode::Visual {
                                                self.vim_mode = VimMode::Normal;
                                                self.visual_anchor = None;
                                            }
                                        }
                                    } else if self.vim_enabled && self.vim_mode == VimMode::Visual {
                                        if self.visual_anchor.is_none() {
                                            self.visual_anchor = Some(sorted.start);
                                        }
                                    }
                                }
                            } else if !is_interactive {
                                // Widget didn't report a cursor_range (because it's non-interactive).
                                // Map the mouse `pos` into a character index using the galley.
                                let mut best = 0usize;
                                let mut best_dist = f32::INFINITY;
                                let total = self.buffer.char_len();
                                for idx in 0..=total {
                                    let cursor = CCursor::new(idx);
                                    let rect = output.galley.pos_from_cursor(cursor);
                                    let screen = output.response.rect.min + rect.min.to_vec2();
                                    let dx = screen.x - pos.x;
                                    let dy = screen.y - pos.y;
                                    let dist = dx * dx + dy * dy;
                                    if dist < best_dist {
                                        best_dist = dist;
                                        best = idx;
                                    }
                                }
                                eprintln!("[dbg] pointer button: galley mapped pos -> {} (pressed={})", best, pressed);
                                self.buffer.caret_char_range = Some(best..best);
                                self.last_vim_key = None;
                                if *pressed {
                                    // start drag
                                    self.mouse_dragging = true;
                                    self.mouse_drag_anchor = Some(best);
                                } else {
                                    // mouse released
                                    if self.mouse_dragging {
                                        // ended a drag
                                        self.mouse_dragging = false;
                                        if let Some(anchor) = self.mouse_drag_anchor.take() {
                                            let cur = best;
                                            if anchor != cur {
                                                eprintln!("[dbg] mouse drag end anchor={} cur={} -> selection {}..{}", anchor, cur, anchor.min(cur), anchor.max(cur));
                                                if self.vim_enabled {
                                                    self.vim_mode = VimMode::Visual;
                                                    // store the original anchor; caret is the cursor
                                                    self.visual_anchor = Some(anchor);
                                                }
                                                // Keep canonical caret at the cursor position
                                                self.buffer.caret_char_range = Some(cur..cur);
                                            }
                                        }
                                    } else {
                                        // single click without drag
                                        if self.vim_enabled {
                                            if self.vim_mode == VimMode::Visual {
                                                self.vim_mode = VimMode::Normal;
                                                self.visual_anchor = None;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Handle pointer movement to update selection during drag
                    for ev in events.iter() {
                        if let Event::PointerMoved(pos) = ev {
                            if !self.mouse_dragging { continue; }
                            // compute nearest char index to mouse pos
                            let pos = *pos;
                            let mut best = 0usize;
                            let mut best_dist = f32::INFINITY;
                            let total = self.buffer.char_len();
                            for idx in 0..=total {
                                let cursor = CCursor::new(idx);
                                let rect = output.galley.pos_from_cursor(cursor);
                                let screen = output.response.rect.min + rect.min.to_vec2();
                                let dx = screen.x - pos.x;
                                let dy = screen.y - pos.y;
                                let dist = dx * dx + dy * dy;
                                if dist < best_dist {
                                    best_dist = dist;
                                    best = idx;
                                }
                            }
                            if let Some(anchor) = self.mouse_drag_anchor {
                                let cur = best;
                                let s = anchor.min(cur);
                                let e = anchor.max(cur).min(self.buffer.char_len());
                                eprintln!("[dbg] pointer moved drag update anchor={} best={} -> selection {}..{}", anchor, best, s, e);
                                // Keep canonical caret at the current cursor index
                                self.buffer.caret_char_range = Some(cur..cur);
                                if self.vim_enabled {
                                    self.vim_mode = VimMode::Visual;
                                    // keep the anchor as the original anchor
                                    self.visual_anchor = Some(anchor);
                                }
                                self.last_vim_key = None;
                            }
                        }
                    }

                    // Vim-only handling: Escape and text input handling for modal editing
                    if self.vim_enabled {
                        for ev in events.iter() {
                            match ev {
                                Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                                    // Escape always returns to Normal mode; end any insert grouping
                                    self.vim_mode = VimMode::Normal;
                                    self.last_vim_key = None;
                                    self.buffer.end_undo_group();
                                }
                                Event::Text(text) => {
                                    if text.is_empty() {
                                        continue;
                                    }
                                    let ch = text.chars().next().unwrap();
                                    match self.vim_mode {
                                        VimMode::Normal | VimMode::Visual | VimMode::VisualLine => {
                                            let focus = vim::ReportBuffer::handle_normal_key(&mut self.buffer, &mut self.vim_mode, &mut self.last_vim_key, &mut self.last_vim_object, &mut self.vim_count, &mut self.visual_anchor, ch);
                                            if focus {
                                                output.response.request_focus();
                                                if let Some(range) = &self.buffer.caret_char_range {
                                                    let start = CCursor::new(range.start);
                                                    let end = CCursor::new(range.end);
                                                    output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                                                    output.state.clone().store(ctx, output.response.id);
                                                } else {
                                                    let pos = self.buffer.report.chars().count();
                                                    let start = CCursor::new(pos);
                                                    output.state.cursor.set_char_range(Some(CCursorRange::one(start)));
                                                    output.state.clone().store(ctx, output.response.id);
                                                }
                                            }
                                        }
                                        VimMode::Insert => {
                                            // in Insert mode, normal text events are handled by TextEdit; we only intercept Escape above
                                        }
                                        _ => {}
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    // If we already have a desired caret position (e.g. from an insert earlier this frame), push it into widget state
                    if self.vim_mode == VimMode::Visual || self.vim_mode == VimMode::VisualLine {
                        // When in Visual mode, prefer showing a selection between the visual anchor
                        // and the current caret. This drives the TextEdit's selection rendering.
                        if let Some(anchor) = self.visual_anchor {
                            let cur = self.buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                                let start_char;
                                let display_end;
                                if self.vim_mode == VimMode::VisualLine {
                                    // compute full-line bounds for anchor and caret
                                    let (a_s, a_e) = self.buffer.line_bounds_at(anchor);
                                    let (c_s, c_e) = self.buffer.line_bounds_at(cur);
                                    start_char = a_s.min(c_s);
                                    let e = a_e.max(c_e).min(self.buffer.char_len());
                                    display_end = e;
                                } else {
                                    let s = anchor.min(cur);
                                    let e = anchor.max(cur);
                                    // Extend the shown cursor range for any non-empty selection
                                    // so the final character is included (ranges are end-exclusive).
                                    let extend = if e > s { 1 } else { 0 };
                                    start_char = s;
                                    display_end = (e).min(self.buffer.char_len()).saturating_add(extend);
                                }
                                let s = start_char;
                                let cur_pos = cur;
                                // Ensure textual selection mirrors visual selection
                                eprintln!("[dbg] visual sync anchor={} cur={} -> display {}..{}", anchor, cur_pos, s, display_end);
                                // Keep canonical caret as the caret position (`cur..cur`).
                                // The visible selection is driven by `visual_anchor` + the caret.
                                self.buffer.caret_char_range = Some(cur_pos..cur_pos);
                                let start = CCursor::new(s);
                                let end = CCursor::new(display_end);
                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                            output.state.store(ui.ctx(), output.response.id);
                        } else if let Some(range) = &self.buffer.caret_char_range {
                            let start = CCursor::new(range.start);
                            let end = CCursor::new(range.end);
                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                            output.state.store(ui.ctx(), output.response.id);
                        }
                    } else {
                        if let Some(range) = &self.buffer.caret_char_range {
                            let start = CCursor::new(range.start);
                            let end = CCursor::new(range.end);
                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                            output.state.store(ui.ctx(), output.response.id);
                        }
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
                            // Prepare painter for selection/caret drawing
                            let painter = ui.painter();

                            // If Visual mode is active and we have an anchor, draw a selection background
                            if self.vim_mode == VimMode::Visual || self.vim_mode == VimMode::VisualLine {
                                if let Some(anchor) = self.visual_anchor {
                                    let cur = self.buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                                    let (s, e_display) = if self.vim_mode == VimMode::VisualLine {
                                        let (a_s, a_e) = self.buffer.line_bounds_at(anchor);
                                        let (c_s, c_e) = self.buffer.line_bounds_at(cur);
                                        let s = a_s.min(c_s);
                                        let e = a_e.max(c_e).min(self.buffer.char_len());
                                        (s, e)
                                    } else {
                                        let s = anchor.min(cur);
                                        let e = anchor.max(cur);
                                        let e_display = if e > s { e.saturating_add(1) } else { e };
                                        (s, e_display)
                                    };
                                    if s < e_display {
                                        // Determine exact per-line glyph bounds by splitting the selected
                                        // substring on newline boundaries and mapping each segment back
                                        // to absolute char indices to query `pos_from_cursor`.
                                        let report = &self.buffer.report;
                                        // Avoid drawing a trailing empty segment when the selection
                                        // ends exactly at a newline in VisualLine mode; that
                                        // would produce a caret-width highlight on the next line.
                                        let mut draw_end = e_display;
                                        if self.vim_mode == VimMode::VisualLine && e_display > s {
                                            if report.chars().nth(e_display.saturating_sub(1)) == Some('\n') {
                                                draw_end = e_display.saturating_sub(1);
                                            }
                                        }
                                        let sel = report.chars().skip(s).take(draw_end.saturating_sub(s)).collect::<String>();
                                        let mut offset = 0usize;
                                        let mut abs_index = s;
                                        for (i, line) in sel.split('\n').enumerate() {
                                            let line_len = line.chars().count();
                                            let line_start = abs_index;
                                            let line_end = abs_index + line_len;

                                            // compute screen coords for line_start and line_end (end is exclusive)
                                            let start_cursor = CCursor::new(line_start);
                                            let end_cursor = CCursor::new(line_end);
                                            let start_pos = output.galley.pos_from_cursor(start_cursor);
                                            let mut end_pos = output.galley.pos_from_cursor(end_cursor);
                                            // Also compute the previous glyph rect and use the maximum
                                            // right edge to ensure the last character is included
                                            let prev_pos_opt = if line_len > 0 {
                                                let prev_idx = line_end.saturating_sub(1);
                                                Some(output.galley.pos_from_cursor(CCursor::new(prev_idx)))
                                            } else { None };

                                            let start_screen = output.response.rect.min + start_pos.min.to_vec2();
                                            let mut end_screen_x = output.response.rect.min.x + end_pos.max.x;
                                            if let Some(prev_pos) = prev_pos_opt {
                                                let prev_x = output.response.rect.min.x + prev_pos.max.x;
                                                if prev_x > end_screen_x {
                                                    end_screen_x = prev_x;
                                                }
                                            }
                                            let end_screen = egui::pos2(end_screen_x, output.response.rect.min.y);

                                            // derive a per-line height from the glyph extents
                                            let line_h = (start_pos.max.y - start_pos.min.y).abs().max(14.0_f32).min(48.0_f32);

                                            // For empty lines (line_len == 0), draw a caret-width selection
                                            let x0 = if line_len == 0 { start_screen.x } else { start_screen.x };
                                            let x1 = if line_len == 0 { start_screen.x + 8.0 } else { end_screen.x };
                                            let y0 = start_screen.y.clamp(output.response.rect.min.y, output.response.rect.max.y);
                                            let y1 = (y0 + line_h).min(output.response.rect.max.y);
                                            let sel_rect = egui::Rect::from_min_max(egui::pos2(x0.clamp(output.response.rect.min.x, output.response.rect.max.x), y0), egui::pos2(x1.clamp(output.response.rect.min.x, output.response.rect.max.x), y1));
                                            painter.rect_filled(sel_rect, 0.0, egui::Color32::from_rgba_unmultiplied(120, 160, 255, 140));

                                            // advance absolute index past this line and the newline (if present)
                                            abs_index = line_end + 1; // skip the newline; safe even if at end because we'll not use abs_index further
                                            offset += line_len + 1;
                                            if abs_index > draw_end { break; }
                                        }
                                    }
                                }
                            }
                            // No visible drag handles (selection is shown in-text).
                            // Draw a visible caret as a filled rectangle the width of a character.
                            let caret_height = 18.0_f32;
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
                        let sel_text = if range.start != range.end {
                            format!("Selection: {}-{}", range.start, range.end)
                        } else {
                            "Selection: -".to_string()
                        };
                        ui.label(format!("Caret: {}    {}", range.end, sel_text));
                    } else {
                        ui.label("Caret: -    Selection: -");
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
                            VimMode::VisualLine => "Visual-Line",
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

                        // Template creation/edit/delete buttons removed — view-only in main UI.

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
                                    // Only apply NICIP filtering when the user supplied NICIP codes.
                                    if !nicips.is_empty() {
                                        // If template has no applicable_codes it's global -> show it.
                                        if !t.applicable_codes.is_empty() {
                                            // require intersection
                                            let mut matched = false;
                                            for sc in &nicips {
                                                if t.applicable_codes.iter().any(|ac| ac.eq_ignore_ascii_case(sc)) {
                                                    matched = true;
                                                    break;
                                                }
                                            }
                                            if !matched {
                                                continue;
                                            }
                                        }
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
                                        let rendered = templates::render_template(&t.body, &vars, &self.templates);
                                        let mut body = rendered.clone();
                                                    if !self.buffer.report.is_empty() && !self.buffer.report.ends_with('\n') && self.buffer.caret_char_range.is_none() {
                                                        body = format!("\n{}", body);
                                                    }
                                                    self.buffer.insert_at_caret(&body);
                                        // After inserting ensure widget state will be updated next frame by the TextEdit output handling
                                    }
                                    let selected = self.selected_template.map(|s| s == i).unwrap_or(false);
                                    if ui.selectable_label(selected, title).clicked() {
                                        if selected {
                                            self.selected_template = None;
                                        } else {
                                            self.selected_template = Some(i);
                                        }
                                    }
                                });
                            }
                        });
                    });
                });
            }

            // Template editor window
            if self.show_template_editor {
                let mut open = self.show_template_editor;
                egui::Window::new("Template Editor").open(&mut open).show(ctx, |ui| {
                    if let Some(mut t) = self.editing_template.clone() {
                        ui.horizontal(|ui| {
                            ui.label("ID:");
                            let mut idv = t.id.clone().unwrap_or_default();
                            ui.text_edit_singleline(&mut idv);
                            if idv.is_empty() { t.id = None; } else { t.id = Some(idv); }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Title:");
                            let mut tv = t.title.clone().unwrap_or_default();
                            ui.text_edit_singleline(&mut tv);
                            if tv.is_empty() { t.title = None; } else { t.title = Some(tv); }
                        });
                        ui.horizontal(|ui| {
                            ui.label("NICIP codes (comma):");
                            let mut codes = t.applicable_codes.join(",");
                            ui.text_edit_singleline(&mut codes);
                            t.applicable_codes = codes.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                        });
                        ui.horizontal(|ui| {
                            ui.label("Modalities (comma):");
                            let mut mods = t.modalities.join(",");
                            ui.text_edit_singleline(&mut mods);
                            t.modalities = mods.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                        });
                        ui.label("Body:");
                        ui.add(egui::TextEdit::multiline(&mut t.body).desired_rows(12));
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                // ensure user templates dir
                                let user_dir = std::path::Path::new("templates/user");
                                let _ = std::fs::create_dir_all(user_dir);
                                // pick filename from id or title
                                let fname_base = t.id.as_ref().or_else(|| t.title.as_ref()).map(|s| s.clone()).unwrap_or_else(|| format!("template_{}", chrono::Utc::now().timestamp()));
                                let mut safe = fname_base.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>();
                                if safe.is_empty() { safe = format!("template_{}", chrono::Utc::now().timestamp()); }
                                let path = user_dir.join(format!("{}.yml", safe));
                                if let Ok(yml) = serde_yaml::to_string(&t) {
                                    let _ = std::fs::write(&path, yml);
                                }
                                // refresh templates list
                                self.templates = templates::load_templates();
                                self.show_template_editor = false;
                                self.editing_template = None;
                                self.editing_index = None;
                                return;
                            }
                            if ui.button("Cancel").clicked() {
                                self.show_template_editor = false;
                                self.editing_template = None;
                                self.editing_index = None;
                                return;
                            }
                        });
                        // save changes back into editing_template state while open
                        self.editing_template = Some(t);
                    } else {
                        ui.label("No template loaded.");
                    }
                });
                self.show_template_editor = open;
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

