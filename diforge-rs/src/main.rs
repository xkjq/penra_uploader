use eframe::egui;
use std::fs;
use crossbeam_channel::unbounded;
use anyhow::Result;

mod speech;
use speech::{create_vosk_engine, SpeechEngine};

struct ReportApp {
    report: String,
    templates: Vec<String>,
    // speech
    engine: Box<dyn SpeechEngine>,
    dictating: bool,
    interim: String,
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
                        // start dictation; receive transcripts on a channel
                        let (tx, rx) = unbounded();
                        if let Ok(()) = self.engine.start(tx) {
                            self.dictating = true;
                            // spawn a UI-side watcher to pull messages
                            let ctx = ctx.clone();
                            let interim_ref = &mut self.interim;
                            // move receiver into a new thread that forwards into GUI via request_repaint
                            std::thread::spawn(move || {
                                while let Ok(msg) = rx.recv() {
                                    // In this simple design we just write to a file-backed channel.
                                    // The UI will poll `interim` periodically (we request repaint).
                                    // TODO: send via a better cross-thread mechanism to the app state
                                    eprintln!("transcript: {}", msg);
                                    let _ = ctx.request_repaint();
                                }
                            });
                        }
                    } else {
                        self.engine.stop();
                        self.dictating = false;
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
