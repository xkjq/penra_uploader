use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize)]
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

/// Load templates from `templates/project/` and `templates/user/`.
/// Returns a list of template bodies (strings) for quick UI use.
pub fn load_templates() -> Vec<String> {
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
                            // Try to parse YAML; if that fails, use full file as body
                            match serde_yaml::from_str::<Template>(&txt) {
                                Ok(t) => {
                                    if !t.body.is_empty() {
                                        out.push(t.body);
                                    }
                                }
                                Err(_) => {
                                    out.push(txt);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if out.is_empty() {
        out.push("Clinical details: \n\nImpression: \n".to_string());
        out.push("History: \nTechnique: \nFindings: \nImpression: \n".to_string());
    }
    out
}

/// Very small template renderer: replace `{{key}}` and `{{key|default}}` with values from the map.
pub fn render_template(template: &str, vars: &std::collections::HashMap<&str, &str>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' && chars.peek() == Some(&'{') {
            // consume second '{'
            chars.next();
            // collect until '}}'
            let mut key = String::new();
            while let Some(&nc) = chars.peek() {
                if nc == '}' {
                    // peek ahead to see if next is also '}'
                    break;
                }
                key.push(nc);
                chars.next();
            }
            // consume '}}' if present
            if chars.peek() == Some(&'}') {
                chars.next();
                if chars.peek() == Some(&'}') {
                    chars.next();
                }
            }
            let key = key.trim();
            // handle default syntax key|default
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
