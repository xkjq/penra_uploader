use eframe::egui;
use std::fs;
use crossbeam_channel::{unbounded, Receiver};
use anyhow::Result;
use serde_json::Value;

mod speech;
mod templates;
mod dragon_ipc;
use speech::{create_vosk_engine, SpeechEngine};

use std::ops::Range;
use egui::text::{CCursor, CCursorRange};

struct ReportApp {
    report: String,
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
    // last known caret/selection as char indices
    caret_char_range: Option<Range<usize>>,
    // debug flag to show caret diagnostics
    show_caret_debug: bool,
    // manual pixel offset to correct caret X position when needed (debug tweak)
    caret_x_offset: f32,
}

impl Default for ReportApp {
    fn default() -> Self {
        // start IPC listener for Dragon helper
        let (tx, rx_local) = crossbeam_channel::unbounded();
        let ipc_writers = dragon_ipc::start_listener(tx);

        Self {
            report: String::new(),
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
            caret_char_range: None,
            show_caret_debug: false,
            caret_x_offset: 3.0,
        }
    }
}

impl ReportApp {
    fn insert_at_caret(&mut self, insert: &str) {
        // Insert `insert` into `self.report` at the current caret/selection (char indices).
        // If there's a selection, replace it. Otherwise insert at cursor or append.
        let (start_char, end_char) = if let Some(r) = &self.caret_char_range {
            (r.start, r.end)
        } else {
            (self.report.chars().count(), self.report.chars().count())
        };

        // Convert char indices to byte indices
        let mut cur = 0usize;
        let mut start_byte = self.report.len();
        let mut end_byte = self.report.len();
        for (b, _) in self.report.char_indices() {
            if cur == start_char {
                start_byte = b;
            }
            if cur == end_char {
                end_byte = b;
                break;
            }
            cur += 1;
        }
        // If start/end point to end of string
        if start_char >= self.report.chars().count() {
            start_byte = self.report.len();
        }
        if end_char >= self.report.chars().count() {
            end_byte = self.report.len();
        }

        self.report.replace_range(start_byte..end_byte, insert);

        // Update caret to be after inserted text (no selection)
        let new_char_pos = start_char + insert.chars().count();
        self.caret_char_range = Some(new_char_pos..new_char_pos);
    }
}

impl eframe::App for ReportApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // drain any IPC messages (e.g., from Dragon helper) and insert into report
        if let Some(rx) = &self.rx {
            while let Ok(msg) = rx.try_recv() {
                if !self.report.is_empty() && !self.report.ends_with('\n') {
                    self.report.push('\n');
                }
                self.report.push_str(&msg);
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
                            self.report.clear();
                            self.caret_char_range = Some(0..0);
                        }
                        if ui.button("Open...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_file() {
                                if let Ok(txt) = fs::read_to_string(path) {
                                    self.report = txt;
                                }
                            }
                        }
                        if ui.button("Save...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().save_file() {
                                let _ = fs::write(path, &self.report);
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
                                if !self.report.is_empty() && !self.report.ends_with('\n') && self.caret_char_range.is_none() {
                                    // if no caret known, preserve previous append behavior and add newline
                                    body = format!("\n{}", body);
                                }
                                self.insert_at_caret(&body);
                            }
                        }
                    });

                    ui.separator();

                    ui.label("Radiology report");
                    // Show the editable report and capture TextEdit output so we can track/store the cursor.
                    let text_edit = egui::TextEdit::multiline(&mut self.report)
                        .desired_rows(20)
                        .desired_width(left_w)
                        .lock_focus(true);

                    let mut output = text_edit.show(ui);

                    // Update stored caret char range from the widget's reported cursor_range
                    if let Some(ccr) = output.cursor_range {
                        let sorted = ccr.as_sorted_char_range();
                        self.caret_char_range = Some(sorted);
                    }

                    // If we already have a desired caret position (e.g. from an insert earlier this frame), push it into widget state
                    if let Some(range) = &self.caret_char_range {
                        let start = CCursor::new(range.start);
                        let end = CCursor::new(range.end);
                        output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                        output.state.store(ui.ctx(), output.response.id);
                    }

                    // Draw an unfocused caret indicator so the user can see the caret when the editor is not focused.
                    if !output.response.has_focus() {
                        // Try to use the widget-reported cursor_range, otherwise fall back to our stored `caret_char_range`.
                        let maybe_ccr = output.cursor_range.or_else(|| {
                            self.caret_char_range.as_ref().map(|r| CCursorRange::two(CCursor::new(r.start), CCursor::new(r.end)))
                        });

                        if let Some(ccr) = maybe_ccr {
                            // Use the primary cursor position
                            let ccursor = ccr.primary;
                            // Position inside the laid-out galley (pos_from_cursor returns a Rect)
                            let galley_pos = output.galley.pos_from_cursor(ccursor);
                            // Convert to screen coords: widget rect min + galley_pos offset
                            // Note: `galley_pos` is already positioned relative to the widget; avoid double-adding `output.galley_pos`.
                            let screen_pos = output.response.rect.min + galley_pos.min.to_vec2();
                            // Draw a visible caret line, clamped to the TextEdit rect
                            let caret_height = 18.0_f32;
                            let painter = ui.painter();
                            let x = (screen_pos.x + self.caret_x_offset).clamp(output.response.rect.min.x, output.response.rect.max.x - 1.0);
                            let y0 = screen_pos.y.clamp(output.response.rect.min.y, output.response.rect.max.y - caret_height);
                            let y1 = y0 + caret_height;
                            painter.line_segment(
                                [egui::pos2(x, y0), egui::pos2(x, y1)],
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(255, 80, 80)),
                            );
                            if self.show_caret_debug {
                                painter.circle_filled(egui::pos2(x, y0), 4.0, egui::Color32::from_rgb(80, 200, 255));
                                let info = format!("screen: {:.1},{:.1}  char_range: {:?}", screen_pos.x + self.caret_x_offset, screen_pos.y, self.caret_char_range);
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
                                "text": self.report,
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
                    if let Some(range) = &self.caret_char_range {
                        ui.label(format!("Caret: {}{}",
                            range.end,
                            if range.start != range.end { format!(" (sel {}..{})", range.start, range.end) } else { String::new() }
                        ));
                    } else {
                        ui.label("Caret: -");
                    }

                    ui.checkbox(&mut self.show_caret_debug, "Debug caret pos");
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
                                        if !self.report.is_empty() && !self.report.ends_with('\n') && self.caret_char_range.is_none() {
                                            body = format!("\n{}", body);
                                        }
                                        self.insert_at_caret(&body);
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
