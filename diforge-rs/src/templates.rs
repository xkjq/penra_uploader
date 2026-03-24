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
    #[serde(default)]
    /// Controls finishing behavior when inserting inline: None, Pre, Post or Both
    pub inline_finish: InlineFinish,
}

fn default_true() -> bool { true }

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub enum InlineFinish {
    None,
    Pre,
    Post,
    Both,
}

impl Default for InlineFinish {
    fn default() -> Self { InlineFinish::None }
}

fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
}

/// If needed, ensure the text immediately before `pos` ends a sentence and is
/// followed by a single space. Returns the updated insertion char index.
pub fn ensure_finish_before(report: &mut String, pos: usize) -> usize {
    if pos == 0 { return pos; }
    // find last non-whitespace char before pos
    let mut last = None;
    for i in (0..pos).rev() {
        if let Some(c) = report.chars().nth(i) {
            if !c.is_whitespace() { last = Some((i, c)); break; }
        }
    }
    let mut new_pos = pos;
    if let Some((idx, ch)) = last {
        if !".!?".contains(ch) {
            // insert a period at `pos`
            let byte_pos = char_to_byte_idx(report, pos);
            report.insert_str(byte_pos, ".");
            new_pos += 1;
        }
        // ensure a single space after the sentence-ending punctuation
        let byte_pos = char_to_byte_idx(report, new_pos);
        let next_char = report.chars().nth(new_pos);
        if next_char != Some(' ') {
            report.insert_str(byte_pos, " ");
            new_pos += 1;
        }
    }
    new_pos
}

/// After inserting text at `pos` of length `inserted_len` chars, ensure the
/// following text is spaced and capitalized appropriately.
pub fn ensure_finish_after(report: &mut String, pos: usize, inserted_len: usize) {
    let check_pos = pos + inserted_len;
    // ensure a single space between inserted text and following text
    let byte_check = char_to_byte_idx(report, check_pos);
    let next_char = report.chars().nth(check_pos);
    if next_char != Some(' ') && next_char.is_some() {
        report.insert_str(byte_check, " ");
    }
    // find first non-space char after insertion
    let mut first_nonspace = None;
    let mut i = check_pos;
    while let Some(c) = report.chars().nth(i) {
        if !c.is_whitespace() { first_nonspace = Some(i); break; }
        i += 1;
    }
    if let Some(fi) = first_nonspace {
        if let Some(c) = report.chars().nth(fi) {
            if c.is_ascii_lowercase() {
                // replace this character with uppercase
                let start = char_to_byte_idx(report, fi);
                let end = char_to_byte_idx(report, fi + 1);
                let up = c.to_ascii_uppercase().to_string();
                report.replace_range(start..end, &up);
            }
        }
    }
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
                                                inline_finish: InlineFinish::None,
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
            inline_finish: InlineFinish::None,
        });
        out.push(Template {
            id: Some("default2".to_string()),
            title: Some("Default History".to_string()),
            applicable_codes: Vec::new(),
            modalities: Vec::new(),
            body: "History: \nTechnique: \nFindings: \nImpression: \n".to_string(),
            insert_inline: false,
            ensure_surrounding_newlines: true,
            inline_finish: InlineFinish::None,
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_finish_before_inserts_period_and_space() {
        let mut s = "This is a testand more".to_string();
        // position between 'test' and 'and' (char index 14)
        let pos = s.chars().take_while(|&c| { true }).count();
        // compute pos for substring: find index of "and"
        let pos = s.find("and").unwrap();
        // convert byte index to char index
        let char_pos = s[..pos].chars().count();
        let new_pos = ensure_finish_before(&mut s, char_pos);
        assert!(s.contains("test. and"));
        assert_eq!(new_pos, char_pos + 2);
    }

    #[test]
    fn test_ensure_finish_after_adds_space_and_capitalizes() {
        let mut s = "Hello INSERThere".to_string();
        // insert at position after "Hello " (6 chars)
        let pos = "Hello ".chars().count();
        // simulate insertion length 6 (INSERT)
        ensure_finish_after(&mut s, pos, 6);
        // after call, the 'h' of 'here' should become 'H' and a space present
        assert!(s.contains("INSERT Here") || s.contains("INSERT Here"));
    }
}

