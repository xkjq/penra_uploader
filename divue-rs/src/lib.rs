use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use dicom_viewer::{read_metadata_all, read_metadata_in_depth, MetadataReadMode};
use dicom_core::Tag;
use dicom_core::dictionary::{DataDictionary, DataDictionaryEntry};
use dicom_dictionary_std::StandardDataDictionary;

pub fn run_meta_viewer(paths: Vec<String>) {
    run_meta_viewer_with_mode(paths, MetadataReadMode::InDepth);
}

pub fn run_meta_viewer_with_mode(paths: Vec<String>, mode: MetadataReadMode) {
    // load metadata maps
    let mut comps: Vec<(String, HashMap<String, String>)> = Vec::new();
    for p in &paths {
        let result = match mode {
            MetadataReadMode::Simple => read_metadata_all(std::path::Path::new(p)),
            MetadataReadMode::InDepth => read_metadata_in_depth(std::path::Path::new(p)),
        };
        match result {
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

/// Launch interactive mode with file picker and drag-drop support
pub fn run_interactive() {
    let app = DivueApp::new();
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

/// Format a single tag segment with a human-readable name if possible.
fn format_tag_segment(segment: &str) -> String {
    let dict = StandardDataDictionary;
    let (tag_text, suffix) = match segment.split_once('[') {
        Some((tag_text, rest)) => (tag_text, format!("[{}", rest)),
        None => (segment, String::new()),
    };

    if let Some((group_str, elem_str)) = tag_text.split_once(',') {
        if let (Ok(g), Ok(e)) = (
            u16::from_str_radix(group_str, 16),
            u16::from_str_radix(elem_str, 16),
        ) {
            let tag = Tag(g, e);
            if let Some(entry) = dict.by_tag(tag) {
                let alias = entry.alias();
                if !alias.is_empty() {
                    return format!("{} ({}){}", alias, tag_text, suffix);
                }
            }
        }
    }

    segment.to_string()
}

/// Prepare a key label for the table.
/// Nested tags are shown as an indented leaf node, with the full path retained for hover text.
fn get_key_display(key: &str) -> (String, String) {
    let segments: Vec<&str> = key.split('/').collect();
    let depth = segments.len().saturating_sub(1);
    let leaf = segments.last().copied().unwrap_or(key);
    let leaf_display = format_tag_segment(leaf);
    let full_display = segments
        .iter()
        .map(|segment| format_tag_segment(segment))
        .collect::<Vec<_>>()
        .join(" / ");

    (format!("{}{}", "  ".repeat(depth), leaf_display), full_display)
}

/// App state that manages both file selection and comparison views
struct DivueApp {
    // File selection state
    selected_files: Vec<PathBuf>,
    
    // Comparison state
    read_mode: MetadataReadMode,
    show_comparison: bool,
    comps: Vec<(String, HashMap<String, String>)>,
    filter: String,
    full_open: bool,
    full_text: String,
}

impl DivueApp {
    fn new() -> Self {
        Self {
            selected_files: Vec::new(),
            read_mode: MetadataReadMode::InDepth,
            show_comparison: false,
            comps: Vec::new(),
            filter: String::new(),
            full_open: false,
            full_text: String::new(),
        }
    }

    fn load_files(&mut self) {
        self.comps.clear();
        for selected_file in &self.selected_files {
            if let Some(path_str) = selected_file.to_str() {
                let result = match self.read_mode {
                    MetadataReadMode::Simple => read_metadata_all(selected_file),
                    MetadataReadMode::InDepth => read_metadata_in_depth(selected_file),
                };
                match result {
                    Ok(map) => self.comps.push((path_str.to_string(), map)),
                    Err(e) => {
                        let mut m = HashMap::new();
                        m.insert("error".to_string(), e);
                        self.comps.push((path_str.to_string(), m));
                    }
                }
            }
        }
    }

    fn go_back_to_selection(&mut self) {
        self.show_comparison = false;
        self.comps.clear();
        self.filter.clear();
        self.full_open = false;
        self.full_text.clear();
    }
}

impl eframe::App for DivueApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.show_comparison {
                self.render_comparison_view(ui, ctx);
            } else {
                self.render_file_selection_view(ctx, ui);
            }
        });
    }
}

impl DivueApp {
    fn render_file_selection_view(&mut self, ctx: &egui::Context, ui: &mut egui::Ui) {
        ui.heading("DICOM Metadata Viewer");
        ui.separator();

        ui.group(|ui| {
            ui.label("Select DICOM files to compare:");
            ui.horizontal(|ui| {
                ui.label("Compare mode:");
                ui.selectable_value(&mut self.read_mode, MetadataReadMode::Simple, "Simple");
                ui.selectable_value(&mut self.read_mode, MetadataReadMode::InDepth, "In-depth (default)");
            });
            match self.read_mode {
                MetadataReadMode::Simple => {
                    ui.label("Simple mode: fast text-focused view of common metadata.");
                }
                MetadataReadMode::InDepth => {
                    ui.label("In-depth mode: iterates all available tags; non-text values shown as VR-aware placeholders.");
                }
            }
            ui.separator();

            // Add files button
            if ui.button("📁 Add Files...").clicked() {
                if let Some(files) = rfd::FileDialog::new()
                    .add_filter("DICOM files", &["dcm"])
                    .add_filter("All files", &["*"])
                    .pick_files()
                {
                    self.selected_files.extend(files);
                }
            }

            // Add folder button
            if ui.button("📂 Add Folder...").clicked() {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    if let Ok(entries) = std::fs::read_dir(&folder) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path.is_file()
                                && path
                                    .extension()
                                    .map(|e| e.eq_ignore_ascii_case("dcm"))
                                    .unwrap_or(false)
                            {
                                self.selected_files.push(path);
                            }
                        }
                    }
                }
            }

            ui.separator();

            // Drag and drop area
            ui.label(
                egui::RichText::new(
                    "💎 Drag and drop DICOM files or folders here\n(or click buttons above)",
                )
                .color(egui::Color32::GRAY),
            );

            // Handle drag and drop
            if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
                for file in ctx.input(|i| i.raw.hovered_files.clone()) {
                    if let Some(path) = file.path {
                        if path.is_file() {
                            if path
                                .extension()
                                .map(|e| e.eq_ignore_ascii_case("dcm"))
                                .unwrap_or(false)
                                && !self.selected_files.contains(&path)
                            {
                                self.selected_files.push(path);
                            }
                        } else if path.is_dir() {
                            // Add all .dcm files from dropped folder
                            if let Ok(entries) = std::fs::read_dir(&path) {
                                for entry in entries.flatten() {
                                    let entry_path = entry.path();
                                    if entry_path.is_file()
                                        && entry_path
                                            .extension()
                                            .map(|e| e.eq_ignore_ascii_case("dcm"))
                                            .unwrap_or(false)
                                        && !self.selected_files.contains(&entry_path)
                                    {
                                        self.selected_files.push(entry_path);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });

        ui.separator();

        // Display selected files
        if !self.selected_files.is_empty() {
            ui.group(|ui| {
                ui.label(format!("Selected files ({}):", self.selected_files.len()));
                ui.separator();

                let mut to_remove = None;
                for (idx, path) in self.selected_files.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(
                            path.file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown"),
                        );
                        if ui.button("✕").clicked() {
                            to_remove = Some(idx);
                        }
                    });
                }

                if let Some(idx) = to_remove {
                    self.selected_files.remove(idx);
                }
            });

            ui.separator();

            // Compare button
            if ui.button("🔍 Compare Metadata").clicked() {
                self.load_files();
                self.show_comparison = true;
            }

            // Clear button
            if ui.button("🗑️ Clear All").clicked() {
                self.selected_files.clear();
            }
        } else {
            ui.heading("No files selected");
            ui.label("Add files above to get started");
        }
    }

    fn render_comparison_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if ui.button("← Back to File Selection").clicked() {
            self.go_back_to_selection();
            return;
        }

        ui.separator();
        ui.heading("DICOM Metadata Compare");
        let mode_label = match self.read_mode {
            MetadataReadMode::Simple => "Simple",
            MetadataReadMode::InDepth => "In-depth",
        };
        ui.label(format!("Mode: {}", mode_label));

        if self.comps.is_empty() {
            ui.label("No files loaded");
            return;
        }

        ui.horizontal(|ui| {
            ui.label("Filter (matches keys or values):");
            ui.text_edit_singleline(&mut self.filter);
            if ui.button("Clear").clicked() {
                self.filter.clear();
            }
        });

        // build union of keys preserving order
        let mut keys = build_key_union(&self.comps);
        keys = filter_keys(&keys, &self.comps, &self.filter);

        // header row
        egui::Grid::new("meta_header")
            .num_columns(1 + self.comps.len())
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Key");
                for (name, _map) in &self.comps {
                    ui.label(name);
                }
                ui.end_row();
            });
        ui.separator();

        egui::ScrollArea::vertical()
            .max_height(900.0)
            .show(ui, |ui| {
                egui::Grid::new("meta_rows")
                    .num_columns(1 + self.comps.len())
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        for k in &keys {
                            let (key_display, key_hover) = get_key_display(k);
                            ui.label(key_display).on_hover_text(key_hover);
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
            egui::Window::new("Field value")
                .open(&mut self.full_open)
                .show(ctx, |ui| {
                    ui.vertical(|ui| {
                        ui.label("Value (select and copy):");
                        let mut tmp = self.full_text.clone();
                        ui.add(egui::TextEdit::multiline(&mut tmp).desired_rows(12));
                        self.full_text = tmp;
                        if ui.button("Copy to clipboard").clicked() {
                            // Clipboard API changed in newer egui; keep value visible for manual copy.
                        }
                    });
                });
        }
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
                        let (key_display, key_hover) = get_key_display(k);
                        ui.label(key_display).on_hover_text(key_hover);
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

