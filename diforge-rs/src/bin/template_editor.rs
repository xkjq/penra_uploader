use eframe::egui;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Template {
    pub id: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub applicable_codes: Vec<String>,
    #[serde(default)]
    pub modalities: Vec<String>,
    #[serde(default)]
    pub body: String,
}

impl Template {
    fn display_title(&self) -> String {
        if let Some(t) = &self.title { return t.clone(); }
        if let Some(id) = &self.id { return id.clone(); }
        self.body.lines().next().unwrap_or("(template)").to_string()
    }
}

fn find_templates_root() -> Option<PathBuf> {
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            let candidate = dir.join("templates");
            if candidate.exists() {
                return Some(candidate);
            }
            if !dir.pop() {
                break;
            }
        }
    }
    None
}

fn load_templates() -> Vec<Template> {
    let mut out = Vec::new();
    let root = find_templates_root().unwrap_or_else(|| PathBuf::from("templates"));
    let project_dir = root.join("project");
    let user_dir = root.join("user");
    for dir in [project_dir, user_dir] {
        if dir.exists() {
            if let Ok(entries) = fs::read_dir(dir) {
                for e in entries.flatten() {
                    let path = e.path();
                    if path.is_file() {
                        if let Ok(txt) = fs::read_to_string(&path) {
                            match serde_yaml::from_str::<Template>(&txt) {
                                Ok(t) => out.push(t),
                                Err(_) => out.push(Template { id: path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()), title: None, applicable_codes: Vec::new(), modalities: Vec::new(), body: txt }),
                            }
                        }
                    }
                }
            }
        }
    }
    if out.is_empty() {
        out.push(Template { id: Some("default1".to_string()), title: Some("Default Clinical".to_string()), applicable_codes: Vec::new(), modalities: Vec::new(), body: "Clinical details:\n\nImpression:\n".to_string() });
    }
    out
}

struct AppState {
    templates: Vec<Template>,
    selected: Option<usize>,
    show_editor: bool,
    editing: Option<Template>,
    // search / filter state
    search: String,
    filter_nicip: String,
    filter_modality: String,
}

impl Default for AppState {
    fn default() -> Self {
        Self { templates: load_templates(), selected: None, show_editor: false, editing: None, search: String::new(), filter_nicip: String::new(), filter_modality: String::new() }
    }
}

impl eframe::App for AppState {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("Template Editor");
            ui.horizontal(|ui| {
                if ui.button("New").clicked() {
                    self.editing = Some(Template { id: None, title: None, applicable_codes: Vec::new(), modalities: Vec::new(), body: String::new() });
                    self.show_editor = true;
                }
                if ui.button("Edit").clicked() {
                    if let Some(i) = self.selected { if i < self.templates.len() { self.editing = Some(self.templates[i].clone()); self.show_editor = true; } }
                }
                if ui.button("Delete").clicked() {
                    if let Some(i) = self.selected {
                        if i < self.templates.len() {
                            let t = &self.templates[i];
                            let user_dir = Path::new("templates/user");
                            if user_dir.exists() {
                                if let Ok(entries) = fs::read_dir(user_dir) {
                                    for e in entries.flatten() {
                                        let path = e.path();
                                        if path.is_file() {
                                            if let Ok(txt) = fs::read_to_string(&path) {
                                                let matched = if let Ok(parsed) = serde_yaml::from_str::<Template>(&txt) { (t.id.is_some() && parsed.id == t.id) || parsed.body == t.body } else { txt == t.body };
                                                if matched { let _ = fs::remove_file(&path); break; }
                                            }
                                        }
                                    }
                                }
                            }
                            self.templates = load_templates();
                            self.selected = None;
                        }
                    }
                }
            });

            ui.separator();

            ui.horizontal(|ui| {
                ui.label("Search:");
                ui.text_edit_singleline(&mut self.search);
            });
            ui.horizontal(|ui| {
                ui.label("NICIP filter (comma):");
                ui.text_edit_singleline(&mut self.filter_nicip);
            });
            ui.horizontal(|ui| {
                ui.label("Modality filter:");
                ui.text_edit_singleline(&mut self.filter_modality);
            });

            // compute displayed indices after applying filters
            let nicips: Vec<String> = self.filter_nicip.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            let modality_opt = if self.filter_modality.trim().is_empty() { None } else { Some(self.filter_modality.trim().to_lowercase()) };
            let mut displayed: Vec<usize> = Vec::new();
            for (i, t) in self.templates.iter().enumerate() {
                // NICIP filtering
                if !nicips.is_empty() {
                    let mut any = false;
                    for n in &nicips {
                        if t.applicable_codes.iter().any(|ac| ac.eq_ignore_ascii_case(n)) { any = true; break; }
                    }
                    if !any { continue; }
                }
                // modality filtering
                if let Some(m) = &modality_opt {
                    if !t.modalities.iter().any(|mm| mm.eq_ignore_ascii_case(m)) { continue; }
                }
                // search string filtering
                if !self.search.trim().is_empty() {
                    let s = self.search.to_lowercase();
                    let title = t.display_title().to_lowercase();
                    if !title.contains(&s) && !t.body.to_lowercase().contains(&s) && t.id.as_ref().map(|x| x.to_lowercase()).unwrap_or_default().contains(&s) == false {
                        continue;
                    }
                }
                displayed.push(i);
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                for &i in displayed.iter() {
                    let t = &self.templates[i];
                    let title = t.display_title();
                    let selected = self.selected.map(|s| s == i).unwrap_or(false);
                    if ui.selectable_label(selected, title).clicked() {
                        if selected { self.selected = None; } else { self.selected = Some(i); }
                    }
                }
            });

            if self.show_editor {
                egui::Window::new("Edit Template").show(ctx, |ui| {
                    if let Some(mut t) = self.editing.clone() {
                        ui.horizontal(|ui| { ui.label("ID:"); let mut idv = t.id.clone().unwrap_or_default(); ui.text_edit_singleline(&mut idv); if idv.is_empty() { t.id = None } else { t.id = Some(idv) } });
                        ui.horizontal(|ui| { ui.label("Title:"); let mut tv = t.title.clone().unwrap_or_default(); ui.text_edit_singleline(&mut tv); if tv.is_empty() { t.title = None } else { t.title = Some(tv) } });
                        ui.horizontal(|ui| { ui.label("NICIP codes (comma):"); let mut codes = t.applicable_codes.join(","); ui.text_edit_singleline(&mut codes); t.applicable_codes = codes.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(); });
                        ui.horizontal(|ui| { ui.label("Modalities (comma):"); let mut mods = t.modalities.join(","); ui.text_edit_singleline(&mut mods); t.modalities = mods.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(); });
                        ui.label("Body:"); ui.add(egui::TextEdit::multiline(&mut t.body).desired_rows(12));
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                let user_dir = Path::new("templates/user"); let _ = fs::create_dir_all(user_dir);
                                let fname_base = t.id.as_ref().or_else(|| t.title.as_ref()).map(|s| s.clone()).unwrap_or_else(|| format!("template_{}", chrono::Utc::now().timestamp()));
                                let mut safe = fname_base.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>(); if safe.is_empty() { safe = format!("template_{}", chrono::Utc::now().timestamp()); }
                                let path = user_dir.join(format!("{}.yml", safe)); if let Ok(yml) = serde_yaml::to_string(&t) { let _ = fs::write(&path, yml); }
                                self.templates = load_templates(); self.editing = None; self.show_editor = false; return;
                            }
                            if ui.button("Cancel").clicked() { self.editing = None; self.show_editor = false; return; }
                        });
                        self.editing = Some(t);
                    } else { ui.label("No template loaded."); }
                });
            }
        });
    }
}

fn main() {
    let opts = eframe::NativeOptions::default();
    eframe::run_native("Template Editor", opts, Box::new(|_cc| Ok(Box::new(AppState::default()))));
}
