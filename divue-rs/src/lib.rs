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

    let app = MetaApp {
        comps,
        expanded_keys: HashSet::new(),
        filter: String::new(),
        identifiable_only: false,
        full_open: false,
        full_text: String::new(),
    };
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

fn key_segments(key: &str) -> Vec<&str> {
    key.split('/').collect()
}

fn segment_alias(segment: &str) -> Option<String> {
    if segment.starts_with('[') {
        return None;
    }

    let dict = StandardDataDictionary;
    let tag_text = segment.split_once('[').map(|(tag_text, _)| tag_text).unwrap_or(segment);
    let (group_str, elem_str) = tag_text.split_once(',')?;
    let g = u16::from_str_radix(group_str, 16).ok()?;
    let e = u16::from_str_radix(elem_str, 16).ok()?;
    let entry = dict.by_tag(Tag(g, e))?;
    Some(entry.alias().to_string())
}

fn row_may_contain_identifiable_data(key: &str, comps: &[(String, HashMap<String, String>)]) -> bool {
    const KEYWORDS: &[&str] = &[
        "patient",
        "person",
        "name",
        "birth",
        "address",
        "institution",
        "physician",
        "operator",
        "performing",
        "referring",
        "requesting",
        "telephone",
        "phone",
        "email",
        "mail",
        "accession",
        "medicalrecord",
        "medical record",
        "studyid",
        "admission",
        "insurance",
        "occupation",
        "religion",
        "ethnic",
        "uid",
        "identifier",
        "id",
    ];

    let mut haystacks = Vec::new();
    haystacks.push(key.to_lowercase());
    haystacks.extend(
        key_segments(key)
            .into_iter()
            .filter_map(segment_alias)
            .map(|alias| alias.to_lowercase()),
    );

    if haystacks
        .iter()
        .any(|text| KEYWORDS.iter().any(|keyword| text.contains(keyword)))
    {
        return true;
    }

    for (_name, map) in comps {
        if let Some(value) = map.get(key) {
            let value_lower = value.to_lowercase();
            if value_lower.contains('@') {
                return true;
            }
            if value.chars().filter(|ch| ch.is_ascii_digit()).count() >= 8 {
                return true;
            }
        }
    }

    false
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

#[derive(Clone)]
struct TreeRow {
    row_id: String,
    value_key: Option<String>,
}

fn append_path(base: &str, segment: &str) -> String {
    if base.is_empty() {
        segment.to_string()
    } else {
        format!("{}/{}", base, segment)
    }
}

fn row_ancestors(row_id: &str) -> Vec<String> {
    let segments: Vec<&str> = row_id.split('/').collect();
    let mut ancestors = Vec::new();
    let mut path = String::new();

    for segment in segments.iter().take(segments.len().saturating_sub(1)) {
        path = append_path(&path, segment);
        ancestors.push(path.clone());
    }

    ancestors
}

fn build_tree_rows(all_keys: &[String], keys_to_show: &[String]) -> Vec<TreeRow> {
    let all_key_set: HashSet<&str> = all_keys.iter().map(String::as_str).collect();
    let mut rows = Vec::new();
    let mut seen = HashSet::new();

    for key in keys_to_show {
        let mut row_prefix = String::new();
        let mut raw_prefix = String::new();

        for raw_segment in key.split('/') {
            if let Some((tag_text, rest)) = raw_segment.split_once('[') {
                let tag_row_id = append_path(&row_prefix, tag_text);
                let tag_raw_key = append_path(&raw_prefix, tag_text);
                if seen.insert(tag_row_id.clone()) {
                    rows.push(TreeRow {
                        row_id: tag_row_id.clone(),
                        value_key: all_key_set.contains(tag_raw_key.as_str()).then_some(tag_raw_key.clone()),
                    });
                }
                row_prefix = tag_row_id;

                let item_segment = format!("[{}", rest);
                let item_row_id = append_path(&row_prefix, &item_segment);
                if seen.insert(item_row_id.clone()) {
                    rows.push(TreeRow {
                        row_id: item_row_id.clone(),
                        value_key: None,
                    });
                }
                row_prefix = item_row_id;
                raw_prefix = append_path(&raw_prefix, raw_segment);
            } else {
                let row_id = append_path(&row_prefix, raw_segment);
                let raw_key = append_path(&raw_prefix, raw_segment);
                if seen.insert(row_id.clone()) {
                    rows.push(TreeRow {
                        row_id: row_id.clone(),
                        value_key: all_key_set.contains(raw_key.as_str()).then_some(raw_key.clone()),
                    });
                }
                row_prefix = row_id;
                raw_prefix = raw_key;
            }
        }
    }

    rows
}

fn effective_expanded_keys(
    rows: &[TreeRow],
    expanded_keys: &HashSet<String>,
    filtered: bool,
) -> HashSet<String> {
    let mut effective = expanded_keys.clone();

    if filtered {
        for row in rows {
            if row.value_key.is_some() {
                for ancestor in row_ancestors(&row.row_id) {
                    effective.insert(ancestor);
                }
            }
        }
    }

    effective
}

fn visible_rows<'a>(rows: &'a [TreeRow], expanded_keys: &HashSet<String>) -> Vec<&'a TreeRow> {
    rows.iter()
        .filter(|row| {
            row_ancestors(&row.row_id)
                .iter()
                .all(|ancestor| expanded_keys.contains(ancestor))
        })
        .collect()
}

fn row_has_children(row_id: &str, rows: &[TreeRow]) -> bool {
    rows.iter()
        .any(|row| row.row_id != row_id && row_ancestors(&row.row_id).iter().any(|ancestor| ancestor == row_id))
}

/// Format a single tag segment with a human-readable name if possible.
fn format_tag_segment(segment: &str) -> String {
    if segment.starts_with('[') {
        return segment.to_string();
    }

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
                    return format!("{} {}{}", tag_text, alias, suffix);
                }
            }
        }
    }

    segment.to_string()
}

/// Prepare a key label for the table.
/// Nested tags are shown as an indented leaf node, with the full path retained for hover text.
fn get_key_display(row_id: &str) -> (usize, String, String) {
    let segments: Vec<&str> = row_id.split('/').collect();
    let depth = segments.len().saturating_sub(1);
    let leaf = segments.last().copied().unwrap_or(row_id);
    let leaf_display = format_tag_segment(leaf);
    let full_display = segments
        .iter()
        .map(|segment| format_tag_segment(segment))
        .collect::<Vec<_>>()
        .join(" / ");

    (depth, leaf_display, full_display)
}

fn render_key_cell(
    ui: &mut egui::Ui,
    row_id: &str,
    rows: &[TreeRow],
    effective_expanded_keys: &HashSet<String>,
    expanded_keys: &mut HashSet<String>,
) {
    let (depth, key_display, key_hover) = get_key_display(row_id);
    let has_children = row_has_children(row_id, rows);

    ui.horizontal(|ui| {
        ui.add_space((depth as f32) * 18.0);

        if has_children {
            let is_expanded = effective_expanded_keys.contains(row_id);
            let icon = if is_expanded { "▼" } else { "▶" };
            if ui.small_button(icon).clicked() {
                if is_expanded {
                    expanded_keys.remove(row_id);
                } else {
                    expanded_keys.insert(row_id.to_string());
                }
            }
        } else {
            ui.add_space(24.0);
        }

        ui.label(key_display).on_hover_text(key_hover);
    });
}

/// App state that manages both file selection and comparison views
struct DivueApp {
    // File selection state
    selected_files: Vec<PathBuf>,
    
    // Comparison state
    read_mode: MetadataReadMode,
    show_comparison: bool,
    comps: Vec<(String, HashMap<String, String>)>,
    expanded_keys: HashSet<String>,
    filter: String,
    identifiable_only: bool,
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
            expanded_keys: HashSet::new(),
            filter: String::new(),
            identifiable_only: false,
            full_open: false,
            full_text: String::new(),
        }
    }

    fn load_files(&mut self) {
        self.comps.clear();
        self.expanded_keys.clear();
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
        self.expanded_keys.clear();
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
        ui.checkbox(&mut self.identifiable_only, "Only show rows likely to contain identifiable data")
            .on_hover_text("Heuristic filter based on DICOM tag names and suspicious values such as IDs, emails, or long digit strings.");

        // build union of keys preserving order
    let mut all_keys = build_key_union(&self.comps);
    all_keys.sort();
        let filtered = !self.filter.is_empty();
        let mut keys = filter_keys(&all_keys, &self.comps, &self.filter);
        if self.identifiable_only {
            keys.retain(|key| row_may_contain_identifiable_data(key, &self.comps));
        }
    let rows = build_tree_rows(&all_keys, &keys);
    let effective_expanded = effective_expanded_keys(&rows, &self.expanded_keys, filtered);
    let visible = visible_rows(&rows, &effective_expanded);

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
                        for row in &visible {
                            render_key_cell(ui, &row.row_id, &rows, &effective_expanded, &mut self.expanded_keys);
                            if let Some(value_key) = &row.value_key {
                                let mut vals: Vec<Option<String>> = Vec::new();
                                for (_name, map) in &self.comps {
                                    vals.push(map.get(value_key).cloned());
                                }
                                let same = values_are_same(value_key, &self.comps);
                                for v in vals {
                                    let full = v.unwrap_or_default();
                                    let display = truncate_string(&full, 120);
                                    let mut btn = egui::Button::new(display.clone());
                                    if !same {
                                        btn = btn.fill(egui::Color32::from_rgb(255, 243, 205));
                                    }
                                    let resp = ui.add(btn).on_hover_text(full.clone());
                                    if resp.clicked() {
                                        self.full_text = full.clone();
                                        self.full_open = true;
                                    }
                                }
                            } else {
                                for _ in &self.comps {
                                    ui.label("");
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
    expanded_keys: HashSet<String>,
    filter: String,
    identifiable_only: bool,
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
            ui.checkbox(&mut self.identifiable_only, "Only show rows likely to contain identifiable data")
                .on_hover_text("Heuristic filter based on DICOM tag names and suspicious values such as IDs, emails, or long digit strings.");

            // build union of keys preserving order
            let mut all_keys = build_key_union(&self.comps);
            all_keys.sort();
            let filtered = !self.filter.is_empty();
            let mut keys = filter_keys(&all_keys, &self.comps, &self.filter);
            if self.identifiable_only {
                keys.retain(|key| row_may_contain_identifiable_data(key, &self.comps));
            }
            let rows = build_tree_rows(&all_keys, &keys);
            let effective_expanded = effective_expanded_keys(&rows, &self.expanded_keys, filtered);
            let visible = visible_rows(&rows, &effective_expanded);

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
                    for row in &visible {
                        render_key_cell(ui, &row.row_id, &rows, &effective_expanded, &mut self.expanded_keys);
                        if let Some(value_key) = &row.value_key {
                            let mut vals: Vec<Option<String>> = Vec::new();
                            for (_name, map) in &self.comps {
                                vals.push(map.get(value_key).cloned());
                            }
                            let same = values_are_same(value_key, &self.comps);
                            for v in vals {
                                let full = v.unwrap_or_default();
                                let display = truncate_string(&full, 120);
                                let mut btn = egui::Button::new(display.clone());
                                if !same {
                                    btn = btn.fill(egui::Color32::from_rgb(255, 243, 205));
                                }
                                let resp = ui.add(btn).on_hover_text(full.clone());
                                if resp.clicked() {
                                    self.full_text = full.clone();
                                    self.full_open = true;
                                }
                            }
                        } else {
                            for _ in &self.comps {
                                ui.label("");
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

