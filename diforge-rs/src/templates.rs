use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct Template {
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
    let project_dir = Path::new("templates/project");
    let user_dir = Path::new("templates/user");
    for dir in [project_dir, user_dir] {
        if dir.exists() {
            if let Ok(entries) = fs::read_dir(dir) {
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
                                    });
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
        });
        out.push(Template {
            id: Some("default2".to_string()),
            title: Some("Default History".to_string()),
            applicable_codes: Vec::new(),
            modalities: Vec::new(),
            body: "History: \nTechnique: \nFindings: \nImpression: \n".to_string(),
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
pub fn render_template(template: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' && chars.peek() == Some(&'{') {
            chars.next();
            let mut key = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == '}' {
                    break;
                }
                key.push(nc);
                chars.next();
            }
            if chars.peek() == Some(&'}') {
                chars.next();
                if chars.peek() == Some(&'}') {
                    chars.next();
                }
            }
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

