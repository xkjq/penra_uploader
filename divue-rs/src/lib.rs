use eframe::egui;
use std::collections::{HashMap, HashSet};
use dicom_viewer::read_metadata_all;

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
    let native_options = eframe::NativeOptions::default();
    eframe::run_native("DICOM Metadata Viewer", native_options, Box::new(|_cc| Ok(Box::new(app)))).ok();
}

/// Build a union of all keys from the provided metadata maps, preserving insertion order.
pub fn build_key_union(comps: &[(String, HashMap<String, String>)]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    let mut keyset: HashSet<String> = HashSet::new();
    for (_name, map) in comps {
        for k in map.keys() {
            if !keyset.contains(k) {
                keyset.insert(k.clone());
                keys.push(k.clone());
            }
        }
    }
    keys
}

/// Filter keys based on a search term. Returns keys where the key itself or any value contains the filter term.
pub fn filter_keys(
    keys: &[String],
    comps: &[(String, HashMap<String, String>)],
    filter: &str,
) -> Vec<String> {
    if filter.is_empty() {
        return keys.to_vec();
    }

    let filter_lower = filter.to_lowercase();
    keys.iter()
        .filter(|k| {
            if k.to_lowercase().contains(&filter_lower) {
                return true;
            }
            for (_name, map) in comps {
                if let Some(v) = map.get(*k) {
                    if v.to_lowercase().contains(&filter_lower) {
                        return true;
                    }
                }
            }
            false
        })
        .cloned()
        .collect()
}

/// Detect if all values for a given key are the same across all metadata maps.
pub fn values_are_same(key: &str, comps: &[(String, HashMap<String, String>)]) -> bool {
    if comps.is_empty() {
        return true;
    }

    // Get the value from the first map as reference
    let first_val = comps[0].1.get(key).cloned();
    
    // Compare all other maps to the first
    for (_name, map) in comps.iter().skip(1) {
        let val = map.get(key).cloned();
        if val != first_val {
            return false;
        }
    }
    true
}

/// Truncate a string to a maximum length and add ellipsis if needed.
pub fn truncate_string(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[..max_len.min(s.len())])
    } else {
        s.to_string()
    }
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
            let mut keys = build_key_union(&self.comps);
            keys = filter_keys(&keys, &self.comps, &self.filter);

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
                        let same = values_are_same(k, &self.comps);
                        for v in vals {
                            let full = v.unwrap_or_default();
                            let display = truncate_string(&full, 120);
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
                            // Clipboard API changed in newer egui; keep value visible for manual copy.
                        }
                    });
                });
            }
        });
    }
}

