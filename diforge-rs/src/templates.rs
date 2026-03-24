use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Template {
    pub id: Option<String>,
    pub title: Option<String>,
    #[serde(default)]
    pub applicable_codes: Vec<String>,
    #[serde(default)]
    pub modalities: Vec<String>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    /// When true, allow inserting this template in the middle of a line without
    /// forcing surrounding newlines.
    pub insert_inline: bool,
    #[serde(default = "default_true")]
    /// When true, ensure there is a blank line before and after the inserted
    /// template body when not inserting inline.
    pub ensure_surrounding_newlines: bool,
}

fn default_true() -> bool { true }

impl Template {
    pub fn display_title(&self) -> String {
        if let Some(t) = &self.title {
            return t.clone();
        }
        if let Some(id) = &self.id {
            return id.clone();
        }
        self.body.lines().next().unwrap_or("(template)").to_string()
    }
}

/// Load templates from `templates/project/` and `templates/user/`.
pub fn load_templates() -> Vec<Template> {
    let mut out = Vec::new();
    // Walk up from current dir and collect any `templates/` directories we find.
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            let candidate = dir.join("templates");
            if candidate.exists() {
                roots.push(candidate);
            }
            if !dir.pop() {
                break;
            }
        }
    }
    // If none found, fall back to local `templates/` path.
    if roots.is_empty() {
        roots.push(PathBuf::from("templates"));
    }

    for root in roots.iter() {
        for dir in [root.join("project"), root.join("user")] {
            if dir.exists() {
                if let Ok(entries) = fs::read_dir(&dir) {
                    for e in entries.flatten() {
                        let path = e.path();
                        if path.is_file() {
                            if let Ok(txt) = fs::read_to_string(&path) {
                                // Try to parse YAML; if that fails, create a basic template
                                match serde_yaml::from_str::<Template>(&txt) {
                                    Ok(t) => {
                                        out.push(t);
                                    }
                                    Err(_) => {
                                        out.push(Template {
                                                id: path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string()),
                                                title: None,
                                                applicable_codes: Vec::new(),
                                                modalities: Vec::new(),
                                                body: txt,
                                                insert_inline: false,
                                                ensure_surrounding_newlines: true,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if out.is_empty() {
        out.push(Template {
            id: Some("default1".to_string()),
            title: Some("Default Clinical".to_string()),
            applicable_codes: Vec::new(),
            modalities: Vec::new(),
            body: "Clinical details: \n\nImpression: \n".to_string(),
            insert_inline: false,
            ensure_surrounding_newlines: true,
        });
        out.push(Template {
            id: Some("default2".to_string()),
            title: Some("Default History".to_string()),
            applicable_codes: Vec::new(),
            modalities: Vec::new(),
            body: "History: \nTechnique: \nFindings: \nImpression: \n".to_string(),
            insert_inline: false,
            ensure_surrounding_newlines: true,
        });
    }
    out
}

/// Basic matching: returns true when the template should be shown for the given NICIP codes and optional modality.
pub fn matches_template(t: &Template, study_codes: &[String], modality: Option<&str>) -> bool {
    // If template has modalities specified and a modality is provided, require it
    if !t.modalities.is_empty() {
        if let Some(m) = modality {
            if !t.modalities.iter().any(|x| x.eq_ignore_ascii_case(m)) {
                return false;
            }
        }
    }
    // If template has no applicable_codes, it's global -> match
    if t.applicable_codes.is_empty() {
        return true;
    }
    // Otherwise require intersection
    for sc in study_codes {
        if t.applicable_codes.iter().any(|ac| ac.eq_ignore_ascii_case(sc)) {
            return true;
        }
    }
    false
}

/// Render template by replacing `{{key}}` and `{{key|default}}` with values in the `vars` map.
/// Render template by replacing `{{key}}` and `{{key|default}}` with values in the `vars` map.
///
/// Supports including other templates with `{{> name}}` where `name` is a template `id` or title.
pub fn render_template(template: &str, vars: &HashMap<String, String>, all_templates: &[Template]) -> String {
    fn recurse(tmpl: &str, vars: &HashMap<String, String>, all: &[Template], depth: usize) -> String {
        if depth > 8 {
            return "".to_string();
        }
        let mut out = String::with_capacity(tmpl.len());
        let mut chars = tmpl.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '{' && chars.peek() == Some(&'{') {
                // consume the second '{'
                chars.next();
                // peek to see if this is an include (>)
                // consume any whitespace after the '{{'
                while let Some(&w) = chars.peek() {
                    if w.is_whitespace() { chars.next(); } else { break; }
                }
                if chars.peek() == Some(&'>') {
                    // include
                    chars.next();
                    // read until '}}'
                    let mut name = String::new();
                    while let Some(&nc) = chars.peek() {
                        if nc == '}' { break; }
                        name.push(nc);
                        chars.next();
                    }
                    // consume trailing '}}'
                    if chars.peek() == Some(&'}') { chars.next(); }
                    if chars.peek() == Some(&'}') { chars.next(); }
                    let name = name.trim();
                    // find template by id or title
                    if let Some(found) = all.iter().find(|tt| tt.id.as_deref().map(|s| s.eq_ignore_ascii_case(name)).unwrap_or(false) || tt.title.as_deref().map(|s| s.eq_ignore_ascii_case(name)).unwrap_or(false)) {
                        let included = recurse(&found.body, vars, all, depth + 1);
                        out.push_str(&included);
                    } else {
                        // not found: leave empty (or could keep a marker)
                    }
                    continue;
                }

                // otherwise parse a variable expression up to the first '}}'
                let mut key = String::new();
                while let Some(&nc) = chars.peek() {
                    if nc == '}' { break; }
                    key.push(nc);
                    chars.next();
                }
                if chars.peek() == Some(&'}') { chars.next(); }
                if chars.peek() == Some(&'}') { chars.next(); }
                let key = key.trim();
                let mut parts = key.splitn(2, '|');
                let k = parts.next().unwrap_or("").trim();
                let default = parts.next().unwrap_or("").trim();
                if let Some(v) = vars.get(k) {
                    out.push_str(v);
                } else if !default.is_empty() {
                    out.push_str(default);
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    recurse(template, vars, all_templates, 0)
}

