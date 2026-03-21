use eframe::egui;
use std::fs;

struct ReportApp {
    report: String,
    templates: Vec<String>,
}

impl Default for ReportApp {
    fn default() -> Self {
        Self {
            report: String::new(),
            templates: vec![
                "Clinical details: \n\nImpression: \n".to_string(),
                "History: \nTechnique: \nFindings: \nImpression: \n".to_string(),
            ],
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
