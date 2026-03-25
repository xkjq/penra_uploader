use eframe::egui;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
    #[serde(default)]
    pub vars: HashMap<String, String>,
    #[serde(skip)]
    pub source: Option<PathBuf>,
    #[serde(default)]
    pub insert_inline: bool,
    #[serde(default = "default_true")]
    pub ensure_surrounding_newlines: bool,
    #[serde(default)]
    pub inline_finish: InlineFinish,
}

fn default_true() -> bool { true }

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
enum InlineFinish {
    None,
    Pre,
    Post,
    Both,
}

impl Default for InlineFinish { fn default() -> Self { InlineFinish::None } }

impl Template {
    fn display_title(&self) -> String {
        if let Some(t) = &self.title { return t.clone(); }
        if let Some(id) = &self.id { return id.clone(); }
        self.body.lines().next().unwrap_or("(template)").to_string()
    }
}

// (InlineFinish is defined locally above)

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
fn find_all_templates_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            let candidate = dir.join("templates");
            if candidate.exists() {
                roots.push(candidate);
            }
            if !dir.pop() { break; }
        }
    }
    roots
}

fn load_templates() -> Vec<Template> {
    let mut out = Vec::new();
    let roots = find_all_templates_roots();
    if roots.is_empty() {
        eprintln!("[template_editor] no templates/ directory found while walking parents; using ./templates");
        let root = PathBuf::from("templates");
        for dir in [root.join("project"), root.join("user")] {
            if dir.exists() {
                if let Ok(entries) = fs::read_dir(dir) {
                    for e in entries.flatten() {
                        let path = e.path();
                        if path.is_file() {
                            if let Ok(txt) = fs::read_to_string(&path) {
                                match serde_yaml::from_str::<Template>(&txt) {
                                    Ok(mut t) => { t.source = Some(path.clone()); out.push(t); },
                                    Err(_) => {
                                        let mut t = Template { id: path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()), title: None, applicable_codes: Vec::new(), modalities: Vec::new(), body: txt, vars: HashMap::new(), insert_inline: false, ensure_surrounding_newlines: true, inline_finish: InlineFinish::None, source: Some(path.clone()) };
                                        out.push(t);
                                    }
                                }
                                eprintln!("[template_editor] loaded: {}", path.display());
                            }
                        }
                    }
                }
            }
        }
    } else {
        for root in roots.iter() {
            eprintln!("[template_editor] scanning root: {}", root.display());
            for dir in [root.join("project"), root.join("user")] {
                eprintln!("[template_editor] scanning: {}", dir.display());
                if dir.exists() {
                    if let Ok(entries) = fs::read_dir(dir) {
                        for e in entries.flatten() {
                            let path = e.path();
                            if path.is_file() {
                                if let Ok(txt) = fs::read_to_string(&path) {
                                    match serde_yaml::from_str::<Template>(&txt) {
                                        Ok(mut t) => { t.source = Some(path.clone()); out.push(t); },
                                        Err(_) => {
                                            let mut t = Template { id: path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()), title: None, applicable_codes: Vec::new(), modalities: Vec::new(), body: txt, vars: HashMap::new(), insert_inline: false, ensure_surrounding_newlines: true, inline_finish: InlineFinish::None, source: Some(path.clone()) };
                                            out.push(t);
                                        }
                                    }
                                    eprintln!("[template_editor] loaded: {}", path.display());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!("[template_editor] total templates: {}", out.len());
    if out.is_empty() {
        out.push(Template { id: Some("default1".to_string()), title: Some("Default Clinical".to_string()), applicable_codes: Vec::new(), modalities: Vec::new(), body: "Clinical details:\n\nImpression:\n".to_string(), vars: HashMap::new(), source: None, insert_inline: false, ensure_surrounding_newlines: true, inline_finish: InlineFinish::None });
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
                    self.editing = Some(Template { id: None, title: None, applicable_codes: Vec::new(), modalities: Vec::new(), body: String::new(), vars: HashMap::new(), source: None, insert_inline: false, ensure_surrounding_newlines: true, inline_finish: InlineFinish::None });
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
                    ui.horizontal(|ui| {
                            if ui.selectable_label(selected, title.clone()).clicked() {
                            if selected { self.selected = None; } else { self.selected = Some(i); }
                        }
                            // insertion mode indicator
                            if t.insert_inline {
                                ui.label("(inline)");
                            } else if t.ensure_surrounding_newlines {
                                ui.label("(block)");
                            } else {
                                ui.label("(soft)");
                            }
                        // show metadata inline
                        let id = t.id.as_deref().unwrap_or("");
                        if !id.is_empty() {
                            ui.label(format!("id: {}", id));
                        }
                        if !t.modalities.is_empty() {
                            ui.label(format!("mods: {}", t.modalities.join(",")));
                        }
                        if !t.applicable_codes.is_empty() {
                            ui.label(format!("codes: {}", t.applicable_codes.join(",")));
                        }
                        // per-row Edit button to directly open editor for this template
                        if ui.small_button("Edit").clicked() {
                            self.editing = Some(self.templates[i].clone());
                            self.show_editor = true;
                            self.selected = Some(i);
                        }
                    });
                    // show a short preview (first non-empty line)
                    if let Some(first) = t.body.lines().find(|l| !l.trim().is_empty()) {
                        ui.label(format!("  ↳ {}", first.trim()));
                    }
                    ui.separator();
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
                        // Note: vars editing removed from standalone editor
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut t.insert_inline, "Insert inline");
                            ui.checkbox(&mut t.ensure_surrounding_newlines, "Ensure surrounding newlines");
                        });
                        ui.horizontal(|ui| {
                            ui.label("Inline finish:");
                            ui.selectable_value(&mut t.inline_finish, InlineFinish::None, "None");
                            ui.selectable_value(&mut t.inline_finish, InlineFinish::Pre, "Pre");
                            ui.selectable_value(&mut t.inline_finish, InlineFinish::Post, "Post");
                            ui.selectable_value(&mut t.inline_finish, InlineFinish::Both, "Both");
                        });
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                // preserve existing `t.vars`; do not allow editing here
                                let user_dir = Path::new("templates/user"); let _ = fs::create_dir_all(user_dir);

                                // Try to overwrite the original file if we have its source path recorded
                                let mut overwritten = false;
                                if let Some(sel) = self.selected {
                                    if sel < self.templates.len() {
                                        let orig = &self.templates[sel];
                                        if let Some(path) = &orig.source {
                                            if let Ok(yml) = serde_yaml::to_string(&t) {
                                                if let Ok(_) = fs::write(path, yml) {
                                                    overwritten = true;
                                                }
                                            }
                                        } else {
                                            // try to find by id/body in the templates dirs as a fallback
                                            for dir in [user_dir.to_path_buf(), PathBuf::from("templates/project")] {
                                                if dir.exists() {
                                                    if let Ok(entries) = fs::read_dir(&dir) {
                                                        'entry_loop: for e in entries.flatten() {
                                                            let path = e.path();
                                                            if path.is_file() {
                                                                if let Ok(txt) = fs::read_to_string(&path) {
                                                                    if let Ok(parsed) = serde_yaml::from_str::<Template>(&txt) {
                                                                        let matched = if orig.id.is_some() {
                                                                            parsed.id == orig.id
                                                                        } else {
                                                                            parsed.body == orig.body
                                                                        };
                                                                        if matched {
                                                                            if let Ok(yml) = serde_yaml::to_string(&t) {
                                                                                let _ = fs::write(&path, yml);
                                                                                overwritten = true;
                                                                                break 'entry_loop;
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                                if overwritten { break; }
                                            }
                                        }
                                    }
                                }

                                if !overwritten {
                                    // fallback: create new user template file
                                    let fname_base = t.id.as_ref().or_else(|| t.title.as_ref()).map(|s| s.clone()).unwrap_or_else(|| format!("template_{}", chrono::Utc::now().timestamp()));
                                    let mut safe = fname_base.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>(); if safe.is_empty() { safe = format!("template_{}", chrono::Utc::now().timestamp()); }
                                    let path = user_dir.join(format!("{}.yml", safe)); if let Ok(yml) = serde_yaml::to_string(&t) { let _ = fs::write(&path, yml); }
                                }

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
