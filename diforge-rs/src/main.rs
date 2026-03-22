use eframe::egui;
use std::fs;
use crossbeam_channel::{unbounded, Receiver};
use anyhow::Result;
use serde_json::Value;

mod speech;
mod templates;
mod dragon_ipc;
use speech::{create_vosk_engine, SpeechEngine};

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
        }
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
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("New").clicked() {
                    self.report.clear();
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
                    // Request attach; actual rect is computed after TextEdit is added below
                    self.attach_requested = true;
                }
                ui.separator();
                for (i, t) in self.templates.iter().enumerate() {
                    if ui.small_button(format!("T{}", i + 1)).clicked() {
                        if !self.report.is_empty() && !self.report.ends_with('\n') {
                            self.report.push('\n');
                        }
                        self.report.push_str(&t.body);
                    }
                }
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

            ui.separator();
            // Speech recognition temporarily hidden — re-enable later if needed.

            ui.label("Radiology report");
            let text_resp = ui.add(
                egui::TextEdit::multiline(&mut self.report)
                    .desired_rows(20)
                    .desired_width(f32::INFINITY)
                    .lock_focus(true),
            );

            // If an attach was requested before we rendered the TextEdit, compute its rect and send overlay command
            if self.attach_requested {
                if let Some(writers) = &self.ipc_writers {
                    let rect = text_resp.rect;
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

            ui.horizontal(|ui| {
                if ui.button("Preview").clicked() {
                    // For now, just trigger a repaint
                    ctx.request_repaint();
                }
                if ui.button("Insert Template").clicked() {
                    self.show_templates_window = true;
                }
            });

            // Templates side panel (right)
            if self.show_templates_window {
                egui::SidePanel::right("templates_panel").resizable(true).show(ctx, |ui| {
                    ui.heading("Templates");
                    ui.horizontal(|ui| {
                        ui.label("NICIP codes (comma-separated):");
                        ui.text_edit_singleline(&mut self.template_nicip);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Search:");
                        ui.text_edit_singleline(&mut self.template_search);
                        if ui.button("Close").clicked() {
                            self.show_templates_window = false;
                        }
                    });

                    // parse nicips
                    let nicips: Vec<String> = self
                        .template_nicip
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect();

                    // list matching templates
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        for (i, t) in self.templates.iter().enumerate() {
                            // basic matching by nicip codes
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
                                    // render with empty vars for now
                                    let vars: std::collections::HashMap<String, String> = std::collections::HashMap::new();
                                    let rendered = templates::render_template(&t.body, &vars);
                                    if !self.report.is_empty() && !self.report.ends_with('\n') {
                                        self.report.push('\n');
                                    }
                                    self.report.push_str(&rendered);
                                    self.show_templates_window = false;
                                }
                                ui.label(title);
                            });
                        }
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
