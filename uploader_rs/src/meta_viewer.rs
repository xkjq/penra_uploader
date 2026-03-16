use eframe::{egui, NativeOptions};
use std::collections::{HashMap, HashSet};
use crate::dicom_viewer::read_metadata_all;

pub fn run_meta_viewer(paths: Vec<String>) {
    // load metadata maps
    let mut comps: Vec<(String, HashMap<String, String>)> = Vec::new();
    for p in &paths {
        match read_metadata_all(std::path::Path::new(p)) {
            Ok(map) => comps.push((p.clone(), map)),
            Err(e) => comps.push((p.clone(), {
                let mut m = HashMap::new(); m.insert("error".to_string(), e); m
            })),
        }
    }

    let app = MetaApp { comps, filter: String::new(), full_open: false, full_text: String::new() };
    let native_options = NativeOptions::default();
    eframe::run_native("DICOM Metadata Viewer", native_options, Box::new(|_cc| Box::new(app))).ok();
}

struct MetaApp {
    comps: Vec<(String, HashMap<String, String>)>,
    filter: String,
    full_open: bool,
    full_text: String,
}

impl eframe::App for MetaApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("DICOM Metadata Compare");
            if self.comps.is_empty() {
                ui.label("No files loaded");
                return;
            }

            ui.horizontal(|ui| {
                ui.label("Filter (matches keys or values):");
                ui.text_edit_singleline(&mut self.filter);
                if ui.button("Clear").clicked() { self.filter.clear(); }
            });

            // build union of keys preserving order
            let mut keys: Vec<String> = Vec::new();
            let mut keyset: HashSet<String> = HashSet::new();
            for (_name, map) in &self.comps {
                for k in map.keys() {
                    if !keyset.contains(k) { keyset.insert(k.clone()); keys.push(k.clone()); }
                }
            }

            // apply filter to keys: keep keys where key or any value contains the filter
            let filter_lower = self.filter.to_lowercase();
            if !filter_lower.is_empty() {
                keys.retain(|k| {
                    if k.to_lowercase().contains(&filter_lower) { return true; }
                    for (_name, map) in &self.comps {
                        if let Some(v) = map.get(k) {
                            if v.to_lowercase().contains(&filter_lower) { return true; }
                        }
                    }
                    false
                });
            }

            // header row
            egui::Grid::new("meta_header").num_columns(1 + self.comps.len()).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Key");
                for (name, _map) in &self.comps {
                    ui.label(name);
                }
                ui.end_row();
            });
            ui.separator();

            egui::ScrollArea::vertical().max_height(900.0).show(ui, |ui| {
                egui::Grid::new("meta_rows").num_columns(1 + self.comps.len()).spacing([8.0, 4.0]).show(ui, |ui| {
                    for k in &keys {
                        ui.label(k);
                        // collect values for this key
                        let mut vals: Vec<Option<String>> = Vec::new();
                        for (_name, map) in &self.comps {
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
                            let full = v.unwrap_or_default();
                            let display = if full.len() > 120 { format!("{}...", &full[..120]) } else { full.clone() };
                            let mut btn = egui::Button::new(display.clone());
                            if !same {
                                btn = btn.fill(egui::Color32::from_rgb(255, 243, 205));
                            }
                            let resp = ui.add(btn).on_hover_text(full.clone());
                            if resp.clicked() {
                                // open full text window for selection/copy
                                self.full_text = full.clone();
                                self.full_open = true;
                            }
                        }
                        ui.end_row();
                    }
                });
            });

            // Full-text window for selecting/copying long values
            if self.full_open {
                egui::Window::new("Field value").open(&mut self.full_open).show(ctx, |ui| {
                    ui.vertical(|ui| {
                        ui.label("Value (select and copy):");
                        let mut tmp = self.full_text.clone();
                        ui.add(egui::TextEdit::multiline(&mut tmp).desired_rows(12));
                        // reflect edits back into stored string so copy will use edited content
                        self.full_text = tmp;
                        if ui.button("Copy to clipboard").clicked() {
                            ui.ctx().output_mut(|o| o.copied_text = self.full_text.clone());
                        }
                    });
                });
            }
        });
    }
}
