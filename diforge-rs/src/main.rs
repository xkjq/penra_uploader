use eframe::egui;
use std::fs;
use crossbeam_channel::{unbounded, Receiver};
use anyhow::Result;
use serde_json::Value;

mod speech;
use speech::{create_vosk_engine, SpeechEngine};

struct ReportApp {
    report: String,
    templates: Vec<String>,
    // speech
    engine: Box<dyn SpeechEngine>,
    dictating: bool,
    interim: String,
    rx: Option<Receiver<String>>,
}

impl Default for ReportApp {
    fn default() -> Self {
        Self {
            report: String::new(),
            templates: vec![
                "Clinical details: \n\nImpression: \n".to_string(),
                "History: \nTechnique: \nFindings: \nImpression: \n".to_string(),
            ],
            engine: create_vosk_engine("models/vosk-small").unwrap_or_else(|_| create_vosk_engine("").unwrap()),
            dictating: false,
            interim: String::new(),
            rx: None,
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
                        self.report.push_str(t);
                    }
                }
            });

            ui.separator();
            ui.horizontal(|ui| {
                if ui.button(if self.dictating { "Stop dictation" } else { "Start dictation" }).clicked() {
                    if !self.dictating {
                        // start dictation; create an engine-facing channel and a UI-facing channel.
                        // We forward engine messages into the UI receiver from a watcher thread
                        // so we can call `request_repaint()` when messages arrive.
                        let (eng_tx, eng_rx) = unbounded();
                        let (ui_tx, ui_rx) = unbounded();
                        if let Ok(()) = self.engine.start(eng_tx) {
                            self.dictating = true;
                            self.rx = Some(ui_rx);
                            let ctx = ctx.clone();
                            std::thread::spawn(move || {
                                while let Ok(msg) = eng_rx.recv() {
                                    let _ = ui_tx.send(msg);
                                    let _ = ctx.request_repaint();
                                }
                            });
                        }
                    } else {
                        self.engine.stop();
                        self.dictating = false;
                        self.rx = None;
                        self.interim.clear();
                    }
                }

                // Pull any pending transcript messages from the engine and apply them
                if let Some(ref rx) = self.rx {
                    while let Ok(msg) = rx.try_recv() {
                        // messages from the engine are often JSON blobs (partial/final) or error strings
                        if let Ok(v) = serde_json::from_str::<Value>(&msg) {
                            if let Some(partial) = v.get("partial").and_then(|p| p.as_str()) {
                                self.interim = partial.to_string();
                            } else if let Some(text) = v.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    if !self.report.is_empty() && !self.report.ends_with('\n') {
                                        self.report.push(' ');
                                    }
                                    self.report.push_str(text);
                                    self.report.push('\n');
                                }
                                self.interim.clear();
                            } else if let Some(results) = v.get("result") {
                                // aggregate result array into text
                                if let Some(arr) = results.as_array() {
                                    let mut acc = String::new();
                                    for item in arr.iter() {
                                        if let Some(t) = item.get("text").and_then(|x| x.as_str()) {
                                            if !acc.is_empty() { acc.push(' '); }
                                            acc.push_str(t);
                                        }
                                    }
                                    if !acc.is_empty() {
                                        if !self.report.is_empty() && !self.report.ends_with('\n') {
                                            self.report.push(' ');
                                        }
                                        self.report.push_str(&acc);
                                        self.report.push('\n');
                                    }
                                    self.interim.clear();
                                }
                            }
                        } else {
                            // not JSON — treat as plain interim text or an error line
                            if msg.starts_with("vosk") || msg.starts_with("failed") || msg.contains("error") {
                                // surface errors into interim for visibility
                                self.interim = format!("[err] {}", msg);
                            } else {
                                // plain transcripts — treat as partial
                                self.interim = msg;
                            }
                        }
                    }
                }

                ui.label(&self.interim);
            });

            ui.label("Radiology report");
            ui.add(egui::TextEdit::multiline(&mut self.report).desired_rows(20).lock_focus(true));

            ui.horizontal(|ui| {
                if ui.button("Preview").clicked() {
                    // For now, just trigger a repaint
                    ctx.request_repaint();
                }
                if ui.button("Insert Template").clicked() {
                    self.templates.push("New template\nImpression: \n".to_string());
                }
            });
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
