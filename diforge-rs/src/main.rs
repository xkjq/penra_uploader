use eframe::egui;
use std::fs;
use crossbeam_channel::{unbounded, Receiver};
use anyhow::Result;
use serde_json::Value;

mod speech;
mod templates;
use speech::{create_vosk_engine, SpeechEngine};

struct ReportApp {
    report: String,
    templates: Vec<templates::Template>,
    // speech
    engine: Box<dyn SpeechEngine>,
    dictating: bool,
    interim: String,
    rx: Option<Receiver<String>>,
    // templates UI state
    show_templates_window: bool,
    template_search: String,
    template_nicip: String,
    selected_template: Option<usize>,
}

impl Default for ReportApp {
    fn default() -> Self {
        Self {
            report: String::new(),
            templates: templates::load_templates(),
            engine: create_vosk_engine("models/vosk-small").unwrap_or_else(|_| create_vosk_engine("").unwrap()),
            dictating: false,
            interim: String::new(),
            rx: None,
            show_templates_window: false,
            template_search: String::new(),
            template_nicip: String::new(),
            selected_template: None,
        }
    }
}

impl eframe::App for ReportApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
                ui.separator();
                for (i, t) in self.templates.iter().enumerate() {
                    if ui.small_button(format!("T{}", i + 1)).clicked() {
                        if !self.report.is_empty() && !self.report.ends_with('\n') {
                            self.report.push('\n');
                        }
                        self.report.push_str(&t.body);
                    }
                }
            });

            ui.separator();
            // Speech recognition temporarily hidden — re-enable later if needed.

            ui.label("Radiology report");
            ui.add(
                egui::TextEdit::multiline(&mut self.report)
                    .desired_rows(20)
                    .desired_width(f32::INFINITY)
                    .lock_focus(true),
            );

            ui.horizontal(|ui| {
                if ui.button("Preview").clicked() {
                    // For now, just trigger a repaint
                    ctx.request_repaint();
                }
                if ui.button("Insert Template").clicked() {
                    self.show_templates_window = true;
                }
            });

            // Templates window
            if self.show_templates_window {
                egui::Window::new("Templates")
                    .collapsible(true)
                    .resizable(true)
                    .show(ctx, |ui| {
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
