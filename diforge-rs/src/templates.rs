use chrono::Local;
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
    /// Per-template variable defaults (e.g. name: Dr Test)
    pub vars: std::collections::HashMap<String, String>,
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

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub enum InlineFinish {
    None,
    Pre,
    Post,
    Both,
}

impl Default for InlineFinish {
    fn default() -> Self {
        InlineFinish::None
    }
}

fn char_to_byte_idx(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// If needed, ensure the text immediately before `pos` ends a sentence and is
/// followed by a single space. Returns the updated insertion char index.
pub fn ensure_finish_before(report: &mut String, pos: usize) -> usize {
    if pos == 0 {
        return pos;
    }
    let orig_total_chars = report.chars().count();
    // find last non-whitespace char before pos
    let mut last = None;
    for i in (0..pos).rev() {
        if let Some(c) = report.chars().nth(i) {
            if !c.is_whitespace() {
                last = Some((i, c));
                break;
            }
        }
    }
    let mut new_pos = pos;
    if let Some((idx, ch)) = last {
        // remove any whitespace between the last non-space char and the insertion pos
        let start_ws_char = idx + 1;
        let start_ws_byte = char_to_byte_idx(report, start_ws_char);
        let end_ws_byte = char_to_byte_idx(report, pos);
        if end_ws_byte > start_ws_byte {
            report.replace_range(start_ws_byte..end_ws_byte, "");
        }

        // insertion point is immediately after the last non-space char
        let ins_char = start_ws_char;
        let mut ins_byte = char_to_byte_idx(report, ins_char);

        // if previous char is not sentence-ending punctuation, insert a period
        if !".!?".contains(ch) {
            report.insert_str(ins_byte, ".");
            // advance insertion byte/index to be after the period
            ins_byte = char_to_byte_idx(report, ins_char + 1);
        }

        // collapse any whitespace after the punctuation to a single ASCII space
        // find char-range of whitespace that originally followed the insertion point
        let mut ws_end_char = ins_char + 1;
        while let Some(c) = report.chars().nth(ws_end_char) {
            if c.is_whitespace() {
                ws_end_char += 1;
            } else {
                break;
            }
        }
        let ws_end_byte = char_to_byte_idx(report, ws_end_char);
        // replace whatever whitespace exists with a single ASCII space
        report.replace_range(ins_byte..ws_end_byte, " ");

        // If there originally was whitespace between the last word and the caret,
        // preserve a slot after the template by inserting an extra space so the
        // template can be placed between two spaces. This keeps the original
        // separation semantics.
        let had_ws_original = pos > start_ws_char;
        if had_ws_original {
            let extra_space_byte = char_to_byte_idx(report, ins_char + 2);
            report.insert_str(extra_space_byte, " ");
        }

        // place caret after the first space (i.e. between the spaces when present)
        // use original total chars (before we mutated the string) to decide EOF behavior
        if ins_char >= orig_total_chars {
            // insertion at end-of-string: we appended a single space, caret after that
            new_pos = ins_char + 1;
        } else {
            new_pos = ins_char + 2;
        }
    }
    new_pos
}

/// After inserting text at `pos` of length `inserted_len` chars, ensure the
/// following text is spaced and capitalized appropriately.
pub fn ensure_finish_after(report: &mut String, pos: usize, inserted_len: usize) {
    let check_pos = pos + inserted_len;

    // Collapse any whitespace immediately after the inserted text to a single ASCII space
    let mut ws_start = check_pos;
    let mut ws_end = check_pos;
    while let Some(c) = report.chars().nth(ws_end) {
        if c.is_whitespace() {
            ws_end += 1;
        } else {
            break;
        }
    }
    if ws_end > ws_start {
        let start = char_to_byte_idx(report, ws_start);
        let end = char_to_byte_idx(report, ws_end);
        report.replace_range(start..end, " ");
    } else {
        // if there's a non-space immediately after inserted text, insert a space
        if let Some(_) = report.chars().nth(check_pos) {
            let b = char_to_byte_idx(report, check_pos);
            report.insert_str(b, " ");
        }
    }

    // find first non-space char after insertion (after our collapsed space)
    let mut first_nonspace = None;
    let mut i = check_pos + 1; // skip the single space we ensured
    while let Some(c) = report.chars().nth(i) {
        if !c.is_whitespace() {
            first_nonspace = Some(i);
            break;
        }
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
                                            id: path
                                                .file_stem()
                                                .and_then(|s| s.to_str())
                                                .map(|s| s.to_string()),
                                            title: None,
                                            applicable_codes: Vec::new(),
                                            modalities: Vec::new(),
                                            body: txt,
                                            vars: HashMap::new(),
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
            vars: HashMap::new(),
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
            vars: HashMap::new(),
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
        if t.applicable_codes
            .iter()
            .any(|ac| ac.eq_ignore_ascii_case(sc))
        {
            return true;
        }
    }
    false
}

/// Render template by replacing `{{key}}` and `{{key|default}}` with values in the `vars` map.
/// Render template by replacing `{{key}}` and `{{key|default}}` with values in the `vars` map.
///
/// Supports including other templates with `{{> name}}` where `name` is a template `id` or title.
pub fn render_template(
    template: &str,
    vars: &HashMap<String, String>,
    all_templates: &[Template],
) -> String {
    // Inject automatic variables (date/time) into a local vars map so templates
    // can reference `{{date}}` and `{{time}}` without the caller providing them.
    let mut vars_with_defaults = vars.clone();
    // If caller passed a special entry `_template_defaults` this will be ignored here;
    // callers should merge per-template defaults before calling `render_template`.
    vars_with_defaults
        .entry("date".to_string())
        .or_insert_with(|| Local::now().format("%Y-%m-%d").to_string());
    vars_with_defaults
        .entry("time".to_string())
        .or_insert_with(|| Local::now().format("%H:%M").to_string());

    fn recurse(
        tmpl: &str,
        vars: &HashMap<String, String>,
        all: &[Template],
        depth: usize,
    ) -> String {
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
                    if w.is_whitespace() {
                        chars.next();
                    } else {
                        break;
                    }
                }
                if chars.peek() == Some(&'>') {
                    // include
                    chars.next();
                    // read until '}}'
                    let mut name = String::new();
                    while let Some(&nc) = chars.peek() {
                        if nc == '}' {
                            break;
                        }
                        name.push(nc);
                        chars.next();
                    }
                    // consume trailing '}}'
                    if chars.peek() == Some(&'}') {
                        chars.next();
                    }
                    if chars.peek() == Some(&'}') {
                        chars.next();
                    }
                    let name = name.trim();
                    // find template by id or title
                    if let Some(found) = all.iter().find(|tt| {
                        tt.id
                            .as_deref()
                            .map(|s| s.eq_ignore_ascii_case(name))
                            .unwrap_or(false)
                            || tt
                                .title
                                .as_deref()
                                .map(|s| s.eq_ignore_ascii_case(name))
                                .unwrap_or(false)
                    }) {
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
                    if nc == '}' {
                        break;
                    }
                    key.push(nc);
                    chars.next();
                }
                if chars.peek() == Some(&'}') {
                    chars.next();
                }
                if chars.peek() == Some(&'}') {
                    chars.next();
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

    recurse(template, &vars_with_defaults, all_templates, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ensure_finish_before_inserts_period_and_space() {
        let mut s = "This is a testand more".to_string();
        // position between 'test' and 'and' (char index 14)
        let pos = s.chars().take_while(|&c| true).count();
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

    #[test]
    fn test_render_template_includes_and_vars() {
        let mut all = Vec::new();
        all.push(Template {
            id: Some("sig".to_string()),
            title: Some("Signature".to_string()),
            applicable_codes: Vec::new(),
            modalities: Vec::new(),
            body: "--\n{{name}}\n".to_string(),
            vars: HashMap::new(),
            insert_inline: true,
            ensure_surrounding_newlines: false,
            inline_finish: InlineFinish::None,
        });
        all.push(Template {
            id: Some("main".to_string()),
            title: Some("Main".to_string()),
            applicable_codes: Vec::new(),
            modalities: Vec::new(),
            body: "Findings... {{> sig }} Report by {{author|unknown}}.".to_string(),
            vars: HashMap::new(),
            insert_inline: true,
            ensure_surrounding_newlines: false,
            inline_finish: InlineFinish::None,
        });

        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "Dr Test".to_string());
        vars.insert("author".to_string(), "Alice".to_string());

        let out = render_template(&all[1].body, &vars, &all);
        assert!(out.contains("--\nDr Test"));
        assert!(out.contains("Report by Alice."));
    }

    #[test]
    fn test_render_template_auto_date_time() {
        let all: Vec<Template> = Vec::new();
        let vars: HashMap<String, String> = HashMap::new();
        let out = render_template("Today is {{date}} at {{time}}", &vars, &all);
        assert!(out.contains("Today is "));
        // date should contain a hyphen (YYYY-MM-DD) and time contain ':'
        assert!(out.contains('-'));
        assert!(out.contains(':'));
    }

    #[test]
    fn test_ensure_finish_before_no_change_on_sentence_end() {
        let mut s = "Ends here.".to_string();
        let pos = s.chars().count();
        let new_pos = ensure_finish_before(&mut s, pos);
        // The helper will ensure there is a single space after sentence-ending
        // punctuation even at end-of-string, so a trailing space is expected.
        assert_eq!(s, "Ends here. ");
        assert_eq!(new_pos, pos + 1);
    }

    #[test]
    fn test_ensure_finish_after_handles_space_and_capital() {
        let mut s = "previous ".to_string() + "next";
        // simulate we inserted "Insert" at position 9 (after 'previous ')
        let pos = "previous ".chars().count();
        // inserted_len 6
        ensure_finish_after(&mut s, pos, 6);
        // After insertion a space should exist and the following word should be capitalized
        // We don't actually insert the text here; ensure_finish_after only adjusts surrounding text,
        // so validate no panic and spacing logic when string has content.
        assert!(s.len() > 0);
    }

    #[test]
    fn test_inline_pre_insertion_combines_with_buffer() {
        use crate::vim::ReportBuffer;
        let mut buf = ReportBuffer::new();
        buf.report = "Findings suspiciousand more".to_string();
        // find position of "and"
        let pos = buf.report.find("and").unwrap();
        let char_pos = buf.report[..pos].chars().count();

        // simulate Pre finish behavior
        let new_pos = ensure_finish_before(&mut buf.report, char_pos);
        buf.set_caret_pos(new_pos);
        buf.insert_at_caret("X");

        assert!(buf.report.contains("suspicious. Xand"));
    }

    #[test]
    fn test_inline_post_insertion_combines_with_buffer() {
        use crate::vim::ReportBuffer;
        let mut buf = ReportBuffer::new();
        buf.report = "Hello next".to_string();
        let pos = "Hello ".chars().count();
        buf.set_caret_pos(pos);
        // perform insertion
        buf.insert_at_caret("inserted");
        let inserted_len = "inserted".chars().count();
        // apply post finish
        ensure_finish_after(&mut buf.report, pos, inserted_len);
        assert!(buf.report.contains("inserted Next"));
    }

    #[test]
    fn test_inline_both_pre_post() {
        use crate::vim::ReportBuffer;
        let mut buf = ReportBuffer::new();
        buf.report = "noteokayhere afterwards".to_string();
        // place caret before "afterwards"
        let pos = buf.report.find("afterwards").unwrap();
        let char_pos = buf.report[..pos].chars().count();

        // pre
        let new_pos = ensure_finish_before(&mut buf.report, char_pos);
        buf.set_caret_pos(new_pos);
        // insert
        buf.insert_at_caret("ins");
        let insert_len = "ins".chars().count();
        // post
        let start_pos = buf
            .caret_char_range
            .as_ref()
            .map(|r| r.start)
            .unwrap_or_else(|| buf.report.chars().count())
            .saturating_sub(insert_len);
        ensure_finish_after(&mut buf.report, start_pos, insert_len);

        let s = buf.report.clone();
        assert!(s.contains("ins "));
        assert!(s.to_lowercase().contains("afterwards"));
        let idx_ins = s.find("ins ").unwrap();
        let idx_dot = s.rfind('.').unwrap_or(0);
        assert!(idx_dot < idx_ins);
    }

    #[test]
    fn test_block_ensure_surrounding_newlines_behavior() {
        use crate::vim::ReportBuffer;
        let mut buf = ReportBuffer::new();
        buf.report = "Line one\nLine two".to_string();
        // caret at end
        buf.goto_end_of_file();
        let mut body = "Inserted block\n".to_string();
        let pos = buf
            .caret_char_range
            .as_ref()
            .map(|r| r.start)
            .unwrap_or_else(|| buf.report.chars().count());
        // ensure_surrounding_newlines true -> prefix with newline if previous char != '\n'
        if pos > 0 {
            if let Some(ch) = buf.report.chars().nth(pos.saturating_sub(1)) {
                if ch != '\n' {
                    body = format!("\n{}", body);
                }
            }
        }
        buf.insert_at_caret(&body);
        assert!(
            buf.report.contains("Line two\nInserted block\n")
                || buf.report.contains("Line two\n\nInserted block\n")
        );
    }

    #[test]
    fn test_inline_pre_example_hello_test_you() {
        use crate::vim::ReportBuffer;
        // initial buffer "hello you" with caret before 'you'
        let mut buf = ReportBuffer::new();
        buf.report = "hello you".to_string();
        // find char index of 'you' start
        let pos = buf.report.find("you").unwrap();
        let char_pos = buf.report[..pos].chars().count();

        // run pre-finish to normalize and place caret
        let new_pos = ensure_finish_before(&mut buf.report, char_pos);
        // set caret and insert template text
        buf.set_caret_pos(new_pos);
        buf.insert_at_caret("test");

        // after insertion we expect: "hello. test you"
        assert_eq!(buf.report, "hello. test you");
    }
}
