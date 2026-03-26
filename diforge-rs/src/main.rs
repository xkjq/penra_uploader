use eframe::egui;
use std::fs;
use crossbeam_channel::{unbounded, Receiver};
use anyhow::Result;
use serde_json::Value;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

mod speech;
mod templates;
mod dragon_ipc;
mod vim;
use speech::{create_vosk_engine, SpeechEngine};

use std::ops::Range;
use egui::text::{CCursor, CCursorRange};

// Simple Levenshtein distance implementation for fallback suggestions
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    if m == 0 { return n; }
    if n == 0 { return m; }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut cur: Vec<usize> = vec![0; n+1];
    for i in 0..m {
        cur[0] = i + 1;
        for j in 0..n {
            let cost = if a_chars[i] == b_chars[j] { 0 } else { 1 };
            cur[j+1] = std::cmp::min(
                std::cmp::min(prev[j+1] + 1, cur[j] + 1),
                prev[j] + cost,
            );
        }
        prev.clone_from(&cur);
    }
    cur[n]
}

#[cfg(test)]
mod selection_tests;
#[derive(PartialEq, Eq, Serialize, Deserialize, Clone, Copy, Debug)]
enum VimMode {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl ReportApp {
    fn add_word_to_user_dict(&mut self, word: &str) {
        // ensure settings dir exists
        let p = settings_path();
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // default user dict path next to settings file
        let user_dict = if let Some(mut sp) = self.spell_dict_path.clone() {
            sp
        } else {
            let mut d = p.clone();
            d.set_file_name("user_dict.txt");
            let s = d.to_string_lossy().to_string();
            self.spell_dict_path = Some(s.clone());
            s
        };

        // load existing words into set (if file exists)
        let mut set = std::collections::HashSet::new();
        if let Ok(txt) = std::fs::read_to_string(&user_dict) {
            for ln in txt.lines() {
                set.insert(ln.trim().to_lowercase());
            }
        }
        let w = word.trim().to_lowercase();
        if !set.contains(&w) {
            // append to file
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&user_dict) {
                use std::io::Write;
                let _ = writeln!(f, "{}", w);
            }
            set.insert(w.clone());
        }
        // replace in-memory dict and persist
        self.spell_dict = set;
        let _ = save_settings(&self.to_settings());
    }
}

impl Default for VimMode {
    fn default() -> Self {
        VimMode::Normal
    }
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct Settings {
    vim_enabled: bool,
    vim_mode: VimMode,
    caret_x_offset: f32,
    show_caret_debug: bool,
    overlay_x: i32,
    overlay_y: i32,
    overlay_w: i32,
    overlay_h: i32,
    // per-user overrides for template variables: template_key -> (var -> value)
    user_template_vars: HashMap<String, HashMap<String, String>>,
    // centralized global variables that apply to all templates unless overridden
    global_vars: HashMap<String, String>,
    // persist spellcheck preference
    spell_enabled: bool,
    // optional path to a custom wordlist used as fallback dictionary
    spell_dict_path: Option<String>,
}

fn settings_path() -> PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        let mut p = PathBuf::from(xdg);
        p.push("diforge-rs");
        p.push("settings.json");
        p
    } else if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config/diforge-rs/settings.json");
        p
    } else {
        PathBuf::from("./diforge-settings.json")
    }
}

// selection tests moved into src/selection_tests.rs

fn load_settings() -> Settings {
    let p = settings_path();
    if let Ok(txt) = fs::read_to_string(&p) {
        if let Ok(s) = serde_json::from_str::<Settings>(&txt) {
            return s;
        }
    }
    Settings::default()
}

fn save_settings(s: &Settings) -> Result<()> {
    let p = settings_path();
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let txt = serde_json::to_string_pretty(s)?;
    fs::write(p, txt)?;
    Ok(())
}


struct ReportApp {
    buffer: vim::ReportBuffer,
    templates: Vec<templates::Template>,
    // speech
    engine: Box<dyn SpeechEngine>,
    dictating: bool,
    interim: String,
    rx: Option<Receiver<String>>,
    ipc_writers: Option<dragon_ipc::SharedWriters>,
    // templates UI state
    show_templates_window: bool,
    template_search: String,
    template_nicip: String,
    selected_template: Option<usize>,
    // template editor state
    show_template_editor: bool,
    editing_template: Option<templates::Template>,
    editing_index: Option<usize>,
    // per-user template variable overrides (runtime copy of Settings.user_template_vars)
    user_template_vars: HashMap<String, HashMap<String, String>>,
    // showing edit-vars dialog: template_key (text stored in edit_vars_text)
    show_edit_vars_dialog: Option<String>,
    edit_vars_text: String,
    // centralized global vars state
    global_vars: HashMap<String, String>,
    show_global_vars_dialog: bool,
    global_vars_text: String,
    global_vars_dirty: bool,
    user_template_vars_dirty: bool,
    // preview option: whether to show variables replaced in template preview
    preview_replace_vars: bool,
    attach_requested: bool,
    // overlay position (for helper)
    overlay_x: i32,
    overlay_y: i32,
    overlay_w: i32,
    overlay_h: i32,
    // (now in `buffer`) last known caret/selection as char indices
    // debug flag to show caret diagnostics
    show_caret_debug: bool,
    // manual pixel offset to correct caret X position when needed (debug tweak)
    caret_x_offset: f32,
    // enable modal vim-like keybindings
    vim_enabled: bool,
    // current vim mode
    vim_mode: VimMode,
    // last key pressed (for multi-key commands like dd)
    last_vim_key: Option<char>,
    // last text-object prefix `i` or `a` when operator-pending
    last_vim_object: Option<char>,
    // visual mode anchor (selection start) in char indices
    visual_anchor: Option<usize>,
    // numeric prefix for vim commands (e.g., `3dw`)
    // mouse drag tracking for click-and-drag selection when TextEdit is non-interactive
    mouse_dragging: bool,
    mouse_drag_anchor: Option<usize>,
    // remember last right-click position so context menus can remain anchored
    last_right_click_pos: Option<egui::Pos2>,
    // numeric prefix for vim commands (e.g., `3dw`)
    vim_count: Option<usize>,
    // Spellchecking
    spell_enabled: bool,
    spell_dict: std::collections::HashSet<String>,
    spell_dict_path: Option<String>,
    #[cfg(feature = "hunspell")]
    hunspell: Option<hunspell::Hunspell>,
    // transient context for spell suggestion popup
    spell_context: Option<SpellContext>,
    // when true, skip reverting widget-applied edits (used when we intentionally
    // update `buffer.report` from UI actions like choosing a suggestion)
    skip_revert_on_widget_edit: bool,
}

#[derive(Clone)]
struct SpellContext {
    word: String,
    start_byte: usize,
    end_byte: usize,
    screen_pos: egui::Pos2,
    suggestions: Vec<String>,
}

impl Default for ReportApp {
    fn default() -> Self {
        // start IPC listener for Dragon helper
        let (tx, rx_local) = crossbeam_channel::unbounded();
        let ipc_writers = dragon_ipc::start_listener(tx);

        let mut app = Self {
            templates: templates::load_templates(),
            engine: create_vosk_engine("models/vosk-small").unwrap_or_else(|_| create_vosk_engine("").unwrap()),
            dictating: false,
            interim: String::new(),
            rx: Some(rx_local),
            // store writers handle so we can send commands
            ipc_writers: Some(ipc_writers),
            show_templates_window: true,
            template_search: String::new(),
            template_nicip: String::new(),
            selected_template: None,
            show_template_editor: false,
            editing_template: None,
            editing_index: None,
            attach_requested: false,
            overlay_x: 100,
            overlay_y: 100,
            overlay_w: 600,
            overlay_h: 200,
            buffer: vim::ReportBuffer::new(),
            show_caret_debug: false,
            caret_x_offset: 3.0,
            vim_enabled: false,
            vim_mode: VimMode::Normal,
            last_vim_key: None,
            last_vim_object: None,
            visual_anchor: None,
            mouse_dragging: false,
            mouse_drag_anchor: None,
            last_right_click_pos: None,
            vim_count: None,
            spell_enabled: false,
            spell_dict: {
                // try loading a system wordlist if available
                let mut set = std::collections::HashSet::new();
                let candidates = ["/usr/share/dict/words", "/usr/dict/words", "/usr/share/dict/web2" ];
                for p in candidates.iter() {
                    if let Ok(txt) = std::fs::read_to_string(p) {
                        for ln in txt.lines() {
                            set.insert(ln.trim().to_lowercase());
                        }
                        break;
                    }
                }
                set
            },
            spell_dict_path: None,
            #[cfg(feature = "hunspell")]
            hunspell: {
                // Try to initialize Hunspell from env vars, common system locations,
                // or a project-local vendor directory (e.g. third_party/hunspell/en_GB/)
                let mut h = None;
                #[cfg(feature = "hunspell")]
                {
                    use std::path::PathBuf;
                    // Helper to attempt initialization and log failures
                    let try_init = |aff: &str, dic: &str| -> Option<hunspell::Hunspell> {
                        eprintln!("[dbg] trying hunspell aff='{}' dic='{}'", aff, dic);
                        match std::panic::catch_unwind(|| hunspell::Hunspell::new(aff, dic)) {
                            Ok(hs) => Some(hs),
                            Err(_) => {
                                eprintln!("[dbg] hunspell::Hunspell::new panicked for {} {}", aff, dic);
                                None
                            }
                        }
                    };

                    // 1) Env vars
                    if let (Ok(aff), Ok(dic)) = (std::env::var("HUNSPELL_AFF"), std::env::var("HUNSPELL_DIC")) {
                        if PathBuf::from(&aff).exists() && PathBuf::from(&dic).exists() {
                            h = try_init(&aff, &dic);
                        } else {
                            eprintln!("[dbg] HUNSPELL_AFF/DIC env set but files missing: {} {}", aff, dic);
                        }
                    }

                    // 2) Common system locations
                    if h.is_none() {
                        let system_dir = PathBuf::from("/usr/share/hunspell");
                        if system_dir.exists() {
                            // pick en_GB then en_US as fallback
                            let candidates = ["en_GB", "en_GB-oxford", "en_US", "en_US-large"];
                            for cand in candidates.iter() {
                                let aff = system_dir.join(format!("{}.aff", cand));
                                let dic = system_dir.join(format!("{}.dic", cand));
                                if aff.exists() && dic.exists() {
                                    if let Some(hs) = try_init(&aff.to_string_lossy(), &dic.to_string_lossy()) {
                                        h = Some(hs);
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    // 3) Project-local vendor directory (where you can place en_GB.aff/.dic)
                    if h.is_none() {
                        if let Ok(exe) = std::env::current_exe() {
                            if let Some(root) = exe.parent() {
                                let vend_dirs = [
                                    root.join("../third_party/hunspell/en_GB"),
                                    root.join("../../third_party/hunspell/en_GB"),
                                    root.join("third_party/hunspell/en_GB"),
                                ];
                                for vd in vend_dirs.iter() {
                                    let aff = vd.join("en_GB.aff");
                                    let dic = vd.join("en_GB.dic");
                                    if aff.exists() && dic.exists() {
                                        if let Some(hs) = try_init(&aff.to_string_lossy(), &dic.to_string_lossy()) {
                                            h = Some(hs);
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if h.is_none() {
                        eprintln!("[dbg] Hunspell not initialized (no usable aff/dic found or initialization failed)");
                    } else {
                        eprintln!("[dbg] Hunspell initialized successfully");
                    }
                }
                h
            },
            spell_context: None,
            skip_revert_on_widget_edit: false,
            user_template_vars: HashMap::new(),
            show_edit_vars_dialog: None,
            edit_vars_text: String::new(),
            user_template_vars_dirty: false,
            global_vars: HashMap::new(),
            show_global_vars_dialog: false,
            global_vars_text: String::new(),
            global_vars_dirty: false,
            preview_replace_vars: true,
        };

        // Debug: print hunspell env and initialization status before applying settings
        let aff_env = std::env::var("HUNSPELL_AFF").ok();
        let dic_env = std::env::var("HUNSPELL_DIC").ok();
        eprintln!("[dbg] HUNSPELL_AFF={:?}", aff_env.as_ref().map(|s| s.as_str()));
        eprintln!("[dbg] HUNSPELL_DIC={:?}", dic_env.as_ref().map(|s| s.as_str()));
        eprintln!("[dbg] HUNSPELL_AFF exists={}", aff_env.as_ref().map(|p| std::path::Path::new(p).exists()).unwrap_or(false));
        eprintln!("[dbg] HUNSPELL_DIC exists={}", dic_env.as_ref().map(|p| std::path::Path::new(p).exists()).unwrap_or(false));
        eprintln!("[dbg] hunspell_present={}", app.hunspell.is_some());

        // Load persisted settings (if any) and apply
        let settings = load_settings();
        app.apply_settings(settings);

        // Add default multiline text for testing if the report is empty
        if app.buffer.report.is_empty() {
            let sample = "Patient: John Doe\nDOB: 1970-01-01\nStudy: CT Head\n\nFindings:\n- No acute intracranial hemorrhage.\n- Mild chronic microvascular ischemic change.\n\nImpression:\n1. No acute intracranial hemorrhage.\n2. Chronic microvascular ischemic change.\n";
            app.buffer.report = sample.to_string();
            let pos = app.buffer.report.chars().count();
            app.buffer.caret_char_range = Some(pos..pos);
        }

        app
    }
}

impl ReportApp {
    fn show_spell_window(&mut self, ctx: &egui::Context) {
        if let Some(c) = self.spell_context.clone() {
            let mut open = true;
            egui::Window::new("Spelling").open(&mut open).fixed_pos(c.screen_pos).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label(format!("Suggestions for: {}", c.word));
                ui.separator();
                if !c.suggestions.is_empty() {
                    for s in c.suggestions.iter().take(8) {
                        if ui.button(s).clicked() {
                            // apply suggestion
                            let mut rep = self.buffer.report.clone();
                            rep.replace_range(c.start_byte..c.end_byte, s);
                            self.buffer.report = rep;
                            // attempt to set caret near replacement start
                            let pos = self.buffer.report[..].chars().take(c.start_byte).count();
                            self.buffer.caret_char_range = Some(pos..pos);
                            self.spell_context = None;
                        }
                    }
                } else {
                    ui.label("No suggestions");
                }
                ui.separator();
                if ui.button("Add to dictionary").clicked() {
                    let w = c.word.clone();
                    self.add_word_to_user_dict(&w);
                    self.spell_context = None;
                }
                if ui.button("Ignore").clicked() {
                    self.spell_dict.insert(c.word.clone());
                    let _ = save_settings(&self.to_settings());
                    self.spell_context = None;
                }
            });
            if !open { self.spell_context = None; }
        }
    }
}

impl ReportApp {
    fn collect_template_var_names(&self) -> Vec<String> {
        use std::collections::HashSet;
        let mut seen: HashSet<String> = HashSet::new();
        for t in self.templates.iter() {
            // include explicit template.vars keys
            for k in t.vars.keys() {
                seen.insert(k.clone());
            }
            // scan body for occurrences like {{key}} or {{key|default}} or {{> partial}}
            let s = &t.body;
            let mut i = 0usize;
            while let Some(start) = s[i..].find("{{") {
                i += start + 2;
                if let Some(end_rel) = s[i..].find("}}") {
                    let chunk = &s[i..i+end_rel];
                    let mut name = chunk.trim();
                    // skip partial includes
                    if name.starts_with('>') {
                        // skip
                    } else {
                        // take up to '|' if present
                        if let Some(pipe) = name.find('|') {
                            name = &name[..pipe];
                        }
                        // trim whitespace and quotes
                        let nm = name.trim().trim_matches('"').trim_matches('\'');
                        if !nm.is_empty() {
                            // only accept reasonable var name chars
                            let filtered = nm.chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect::<String>();
                            if !filtered.is_empty() {
                                seen.insert(filtered);
                            }
                        }
                    }
                    i = i + end_rel + 2;
                } else {
                    break;
                }
            }
        }
        let mut out: Vec<String> = seen.into_iter().collect();
        out.sort();
        out
    }
    // check a single word for correctness using hunspell if available,
    // otherwise fallback to in-memory dictionary lookup
    fn check_word_correct(&self, word: &str) -> bool {
        let key = word.to_lowercase();
        #[cfg(feature = "hunspell")]
        {
            if let Some(hs) = &self.hunspell {
                // hunspell::Hunspell API: spell(word) -> bool
                if hs.check(word) {
                    return true;
                } else {
                    return false;
                }
            }
        }
        // fallback to simple dictionary
        self.spell_dict.contains(&key)
    }
    fn to_settings(&self) -> Settings {
        Settings {
            vim_enabled: self.vim_enabled,
            vim_mode: self.vim_mode,
            caret_x_offset: self.caret_x_offset,
            show_caret_debug: self.show_caret_debug,
            overlay_x: self.overlay_x,
            overlay_y: self.overlay_y,
            overlay_w: self.overlay_w,
            overlay_h: self.overlay_h,
            user_template_vars: self.user_template_vars.clone(),
            global_vars: self.global_vars.clone(),
            spell_enabled: self.spell_enabled,
            spell_dict_path: self.spell_dict_path.clone(),
        }
    }

    // Insert a template at the current caret position, applying inline/block
    // insertion rules and optional pre/post finishing (sentence completion).
    fn insert_template_at_caret(&mut self, t: &templates::Template) {
        // Group the entire insertion (pre-finish, insertion, post-finish or
        // block insertion) into a single undo step so Undo/Redo treats it
        // atomically.
        self.buffer.start_undo_group();
        // start with per-template defaults
        let mut vars: std::collections::HashMap<String, String> = t.vars.clone();
        // overlay user overrides from settings (per-user saved values)
        let key = t.id.clone().unwrap_or_else(|| t.title.clone().unwrap_or_else(|| t.display_title()));
        if let Some(user_map) = self.user_template_vars.get(&key) {
            for (k, v) in user_map.iter() {
                vars.insert(k.clone(), v.clone());
            }
        }
        // finally, fill missing keys from centralized global vars so users
        // only need to set common values once
        for (k, v) in self.global_vars.iter() {
            vars.entry(k.clone()).or_insert_with(|| v.clone());
        }
        // (future) overlay per-insertion overrides here
        let rendered = templates::render_template(&t.body, &vars, &self.templates);

        if t.insert_inline {
            let mut pos = self.buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or_else(|| self.buffer.report.chars().count());
            // apply pre-finish if requested
            if t.inline_finish == templates::InlineFinish::Pre || t.inline_finish == templates::InlineFinish::Both {
                pos = templates::ensure_finish_before(&mut self.buffer.report, pos);
                self.buffer.set_caret_pos(pos);
            }

            // perform insertion
            self.buffer.insert_at_caret(&rendered);

            // apply post-finish if requested (pos is insertion start)
            if t.inline_finish == templates::InlineFinish::Post || t.inline_finish == templates::InlineFinish::Both {
                let insert_len = rendered.chars().count();
                let start_pos = self.buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or_else(|| self.buffer.report.chars().count()).saturating_sub(insert_len);
                templates::ensure_finish_after(&mut self.buffer.report, start_pos, insert_len);
            }
            // End the grouped undo step for inline insertions as well
            self.buffer.end_undo_group();
        } else {
            // block-mode insertion: ensure surrounding blank lines if requested
            let mut body = rendered.clone();
            let pos = self.buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or_else(|| self.buffer.report.chars().count());
            if t.ensure_surrounding_newlines {
                // prefix
                if pos > 0 {
                    if let Some(ch) = self.buffer.report.chars().nth(pos.saturating_sub(1)) {
                        if ch != '\n' {
                            body = format!("\n{}", body);
                        }
                    }
                }
                // suffix
                if let Some(ch) = self.buffer.report.chars().nth(pos) {
                    if ch != '\n' && !body.ends_with('\n') {
                        body.push('\n');
                    }
                }
            }
            self.buffer.insert_at_caret(&body);
            // End the grouped undo step
            self.buffer.end_undo_group();
        }
    }

    fn apply_settings(&mut self, s: Settings) {
        self.vim_enabled = s.vim_enabled;
        self.vim_mode = s.vim_mode;
        self.caret_x_offset = s.caret_x_offset;
        self.show_caret_debug = s.show_caret_debug;
        self.overlay_x = s.overlay_x;
        self.overlay_y = s.overlay_y;
        self.overlay_w = s.overlay_w;
        self.overlay_h = s.overlay_h;
        self.user_template_vars = s.user_template_vars;
        self.global_vars = s.global_vars;
        // apply persisted spell settings
        self.spell_enabled = s.spell_enabled;
        self.spell_dict_path = s.spell_dict_path.clone();
        // if a custom wordlist path is provided, try to load it as the spell_dict
        if let Some(p) = &self.spell_dict_path {
            if let Ok(txt) = std::fs::read_to_string(p) {
                let mut set = std::collections::HashSet::new();
                for ln in txt.lines() {
                    set.insert(ln.trim().to_lowercase());
                }
                self.spell_dict = set;
            }
        }
    }

    // Ensure all vim-related runtime state is cleared so emulation is fully disabled.
    pub fn ensure_vim_disabled_state(&mut self) {
        self.vim_mode = VimMode::Normal;
        self.last_vim_key = None;
        self.last_vim_object = None;
        self.visual_anchor = None;
        self.mouse_dragging = false;
        self.mouse_drag_anchor = None;
        // collapse any selection into a canonical caret
        let cur = self.buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
        self.buffer.caret_char_range = Some(cur..cur);
    }

    // Replicate the Alt+Number quick-insert behavior as performed in the
    // UI event handler. This helper intentionally does NOT attempt to
    // remove any numeric character that the TextEdit may have inserted; it
    // reproduces the raw sequence so tests can assert the original buggy
    // behaviour.
    pub fn alt_number_quick_insert(&mut self, key: egui::Key) {
        // Build visible template index list using current filters
        let nicips: Vec<String> = self
            .template_nicip
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut visible_indices: Vec<usize> = Vec::new();
        for (i, t) in self.templates.iter().enumerate() {
            if !nicips.is_empty() {
                if !t.applicable_codes.is_empty() {
                    let mut matched = false;
                    for sc in &nicips {
                        if t.applicable_codes.iter().any(|ac| ac.eq_ignore_ascii_case(sc)) {
                            matched = true;
                            break;
                        }
                    }
                    if !matched { continue; }
                }
            }
            let title = t.display_title();
            if !self.template_search.is_empty()
                && !title.to_lowercase().contains(&self.template_search.to_lowercase())
                && !t.body.to_lowercase().contains(&self.template_search.to_lowercase())
            {
                continue;
            }
            visible_indices.push(i);
        }

        let target_opt = match key {
            egui::Key::Num1 => Some(0usize),
            egui::Key::Num2 => Some(1usize),
            egui::Key::Num3 => Some(2usize),
            egui::Key::Num4 => Some(3usize),
            egui::Key::Num5 => Some(4usize),
            egui::Key::Num6 => Some(5usize),
            egui::Key::Num7 => Some(6usize),
            egui::Key::Num8 => Some(7usize),
            egui::Key::Num9 => Some(8usize),
            egui::Key::Num0 => Some(9usize),
            _ => None,
        };

        if let Some(pos) = target_opt {
            if let Some(&tmpl_i) = visible_indices.get(pos) {
                let t = self.templates[tmpl_i].clone();
                self.insert_template_at_caret(&t);
            }
        }
    }
}

impl eframe::App for ReportApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // drain any IPC messages (e.g., from Dragon helper) and insert into report buffer
        if let Some(rx) = &self.rx {
            while let Ok(msg) = rx.try_recv() {
                if !self.buffer.report.is_empty() && !self.buffer.report.ends_with('\n') {
                    self.buffer.report.push('\n');
                }
                self.buffer.report.push_str(&msg);
            }
        }
        // (removed SidePanel overlay; templates render inline when requested)

        // Intercept Alt+Number key events before widgets (e.g. TextEdit)
        // handle them here and remove the event so the TextEdit doesn't
        // receive and insert the numeric character.
        // Collect and remove Alt+Number events, recording which keys were seen
        // so we can handle insertion while ensuring widgets (TextEdit) don't
        // receive the numeric character.
        let mut removed_alt_keys: Vec<egui::Key> = Vec::new();
        ctx.input_mut(|i| {
            use egui::Event;
            let mut remove_idxs: Vec<usize> = Vec::new();
            for (idx, ev) in i.events.iter().enumerate() {
                if let Event::Key { key, pressed: true, modifiers, .. } = ev {
                    if modifiers.alt {
                        match key {
                            egui::Key::Num1 | egui::Key::Num2 | egui::Key::Num3 |
                            egui::Key::Num4 | egui::Key::Num5 | egui::Key::Num6 |
                            egui::Key::Num7 | egui::Key::Num8 | egui::Key::Num9 |
                            egui::Key::Num0 => {
                                // record key for handling after we leave input_mut
                                removed_alt_keys.push(*key);
                                remove_idxs.push(idx);
                            }
                            _ => {}
                        }
                    }
                }
            }
            // Remove matched events in reverse so indices remain valid
            for &r in remove_idxs.iter().rev() {
                i.events.remove(r);
            }
        });

        // Now perform quick-insert for each removed Alt+number key
        for key in removed_alt_keys.into_iter() {
            self.alt_number_quick_insert(key);
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // Build a two-column layout: left = main report area, right = templates (fixed width)
            let avail = ui.available_rect_before_wrap();
            let avail_w = avail.width();
            let avail_h = avail.height();
            let right_w = 320.0_f32.min(avail_w.max(0.0));
            let left_w = (avail_w - right_w).max(180.0);

            let left_rect = egui::Rect::from_min_max(avail.min, egui::pos2(avail.min.x + left_w, avail.max.y));
            let right_rect = egui::Rect::from_min_max(egui::pos2(avail.min.x + left_w, avail.min.y), avail.max);

            ui.allocate_ui_at_rect(left_rect, |ui| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        if ui.button("New").clicked() {
                            self.buffer.report.clear();
                            self.buffer.caret_char_range = Some(0..0);
                        }
                        if ui.button("Open...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().pick_file() {
                                if let Ok(txt) = fs::read_to_string(path) {
                                    self.buffer.report = txt;
                                }
                            }
                        }
                        if ui.button("Save...").clicked() {
                            if let Some(path) = rfd::FileDialog::new().save_file() {
                                let _ = fs::write(path, &self.buffer.report);
                            }
                        }
                        if ui.button("Attach Dragon Overlay").clicked() {
                            self.attach_requested = true;
                        }
                        ui.separator();
                        let spell_resp = ui.checkbox(&mut self.spell_enabled, "Spellcheck");
                        if spell_resp.changed() {
                            let _ = save_settings(&self.to_settings());
                        }
                        ui.separator();
                        // Clone templates to avoid borrowing `self` across UI calls so we can mutably borrow later
                        let templates_top = self.templates.clone();
                        for (i, t) in templates_top.iter().enumerate() {
                            if ui.small_button(format!("T{}", i + 1)).clicked() {
                                    // Insert at caret (or append)
                                    let mut body = t.body.clone();
                                    if !self.buffer.report.is_empty() && !self.buffer.report.ends_with('\n') && self.buffer.caret_char_range.is_none() {
                                        // if no caret known, preserve previous append behavior and add newline
                                        body = format!("\n{}", body);
                                    }
                                    self.buffer.insert_at_caret(&body);
                                }
                        }
                    });

                    ui.separator();

                    ui.label("Radiology report");
                    // Show the editable report and capture TextEdit output so we can track/store the cursor.
                    // If Vim emulation is enabled and we are NOT in Insert mode, prevent the TextEdit
                    // from applying direct edits by restoring any accidental changes.
                    let prev_report = self.buffer.report.clone();
                    // If vim emulation is disabled, ensure no vim runtime state persists.
                    if !self.vim_enabled {
                        self.ensure_vim_disabled_state();
                    }
                    // If Alt is currently pressed, make the TextEdit non-interactive
                    // so it doesn't receive/insert the numeric character while we
                    // intercept Alt+Number events.
                    let alt_pressed = ctx.input(|i| i.modifiers.alt);
                    let is_interactive = (!self.vim_enabled || self.vim_mode == VimMode::Insert) && !alt_pressed;
                    let mut text_edit = egui::TextEdit::multiline(&mut self.buffer.report)
                        .desired_rows(20)
                        .desired_width(left_w)
                        // only lock focus when in Insert mode so Normal mode can intercept keys
                        .lock_focus(self.vim_enabled && self.vim_mode == VimMode::Insert)
                        // make the widget non-interactive when Vim is active but not in Insert,
                        // so clicks don't hand keyboard focus to the TextEdit unexpectedly.
                        .interactive(is_interactive);

                    // Use monospace font when Vim emulation is active for consistent fixed-width behavior
                    if self.vim_enabled {
                        text_edit = text_edit.font(egui::TextStyle::Monospace);
                    }

                    let mut output = text_edit.show(ui);

                    // Update stored caret char range from the widget's reported cursor_range
                    // Only trust the widget when the TextEdit is interactive (Insert mode or Vim disabled).
                    if is_interactive {
                        if let Some(ccr) = output.cursor_range {
                            let sorted = ccr.as_sorted_char_range();
                            self.buffer.caret_char_range = Some(sorted);
                        }
                    }

                    // If vim emulation is active but we're not in Insert mode, revert any
                    // direct text changes the TextEdit may have applied (so Normal mode
                    // keystrokes are handled by our modal logic instead).
                    if self.vim_enabled && self.vim_mode != VimMode::Insert && self.buffer.report != prev_report {
                        if self.skip_revert_on_widget_edit {
                            eprintln!("[dbg] accepting intentional widget edit (skip revert)");
                            self.skip_revert_on_widget_edit = false;
                        } else {
                            eprintln!("[dbg] reverting buffer.report due to non-Insert vim mode (widget tried to edit)");
                            self.buffer.report = prev_report;
                        }
                    }

                    // Spellchecking: find words not in dictionary and draw squiggly underlines
                            if self.spell_enabled {
                        // simple word regex: letters and apostrophes, length >= 2
                        let re = regex::Regex::new(r"[A-Za-z']{2,}").unwrap();
                        let text = &self.buffer.report;
                        let painter = ui.painter();
                        for m in re.find_iter(text) {
                            let word = m.as_str();
                            if self.check_word_correct(word) { continue; }
                            // map byte offsets to char indices
                            let start_byte = m.start();
                            let end_byte = m.end();
                            let start_char = text[..start_byte].chars().count();
                            let word_len_chars = text[start_byte..end_byte].chars().count();
                            let end_char = start_char + word_len_chars;

                            // compute screen coords for start and end using galley
                            let start_cursor = CCursor::new(start_char);
                            let end_cursor = CCursor::new(end_char.saturating_sub(1));
                            let start_rect = output.galley.pos_from_cursor(start_cursor);
                            let end_rect = output.galley.pos_from_cursor(end_cursor);
                            let start_x = output.response.rect.min.x + start_rect.min.x;
                            let end_x = output.response.rect.min.x + end_rect.max.x;
                            // baseline y just below glyph area
                            let baseline_y = output.response.rect.min.y + start_rect.max.y + 2.0;

                            // draw a simple zig-zag squiggly underline
                            let mut pts: Vec<egui::Pos2> = Vec::new();
                            let step = 6.0_f32;
                            if end_x > start_x + 2.0 {
                                let mut x = start_x;
                                let mut up = false;
                                while x < end_x {
                                    let y = if up { baseline_y - 2.0 } else { baseline_y + 2.0 };
                                    pts.push(egui::pos2(x, y));
                                    x += step;
                                    up = !up;
                                }
                                // ensure last point at end_x
                                pts.push(egui::pos2(end_x, baseline_y));
                                painter.line(pts, egui::Stroke::new(1.5, egui::Color32::from_rgb(220, 40, 40)));
                            }
                        }
                    }

                    // Context menu for spellcheck: right-click on a misspelled word
                    if self.spell_enabled {
                        // Use the response's context menu so it only opens when right-clicking the TextEdit
                        let _ = output.response.context_menu(|ui| {
                            // force a minimum width so the menu doesn't shrink on reopen
                            let menu_width = 300.0f32;
                            ui.set_min_width(menu_width);
                            // Prefer the original press origin (frame of click), then any stored
                            // right-click pos. Avoid using the live interact_pos alone because
                            // that causes the menu contents to change as the mouse moves.
                            if let Some(pointer_pos) = ui.input(|i| i.pointer.press_origin())
                                .or(self.last_right_click_pos)
                                .or_else(|| ui.input(|i| i.pointer.interact_pos()))
                            {
                                // Find which word (if any) under the pointer is misspelled
                                let re = regex::Regex::new(r"[A-Za-z']{2,}").unwrap();
                                let text = &self.buffer.report;
                                for m in re.find_iter(text) {
                                    let start_byte = m.start();
                                    let end_byte = m.end();
                                    let start_char = text[..start_byte].chars().count();
                                    let word_len_chars = text[start_byte..end_byte].chars().count();
                                    let end_char = start_char + word_len_chars;

                                    let start_cursor = CCursor::new(start_char);
                                    let end_cursor = CCursor::new(end_char.saturating_sub(1));
                                    let start_rect = output.galley.pos_from_cursor(start_cursor);
                                    let end_rect = output.galley.pos_from_cursor(end_cursor);
                                    let start_x = output.response.rect.min.x + start_rect.min.x;
                                    let end_x = output.response.rect.min.x + end_rect.max.x;
                                    let baseline_y = output.response.rect.min.y + start_rect.max.y + 2.0;

                                    // crude hit test: pointer inside horizontal bounds and near baseline
                                    if pointer_pos.x >= start_x && pointer_pos.x <= end_x
                                        && pointer_pos.y >= baseline_y - 10.0 && pointer_pos.y <= baseline_y + 10.0
                                    {
                                        let word = m.as_str().to_string();
                                        if self.check_word_correct(&word) { break; }

                                        // compute suggestions once
                                        let mut suggestions: Vec<String> = Vec::new();
                                        #[cfg(feature = "hunspell")]
                                        if let Some(hs) = &self.hunspell {
                                            suggestions = hs.suggest(&word).clone();
                                        }
                                        // Fallback: if hunspell not present or returned no suggestions,
                                        // use the in-memory `spell_dict` and simple Levenshtein distance.
                                        if suggestions.is_empty() {
                                            let target = word.to_lowercase();
                                            // collect candidates with small length difference to limit work
                                            let mut cand: Vec<(usize, String)> = self.spell_dict.iter()
                                                .filter(|w| {
                                                    let lw = w.len();
                                                    let lt = target.len();
                                                    (lw as isize - lt as isize).abs() <= 3
                                                })
                                                .map(|w| (levenshtein(&target, w), w.clone()))
                                                .collect();
                                            // sort by distance then alphabetically
                                            cand.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
                                            for (d, s) in cand.into_iter().take(8) {
                                                // only include reasonably close matches
                                                if d <= 4 {
                                                    suggestions.push(s);
                                                }
                                            }
                                        }

                                        // Log for debugging: which word and suggestions were found
                                        eprintln!("[dbg] context menu word='{}' suggestions={:?}", word, suggestions);
                                        if suggestions.is_empty() {
                                            eprintln!("[dbg] no suggestions for '{}' (hunspell_present={})", word, self.hunspell.is_some());
                                        }

                                        for s in suggestions.iter().take(6) {
                                            if ui.add_sized(egui::vec2(menu_width, 0.0), egui::Button::new(s)).clicked() {
                                                // Use ReportBuffer API to replace by character indices (safer for UTF-8)
                                                let start_ch = start_char;
                                                let end_ch = end_char;
                                                // set selection and insert at caret which will replace selection
                                                // mark this as an intentional widget edit so it isn't reverted
                                                self.skip_revert_on_widget_edit = true;
                                                self.buffer.caret_char_range = Some(start_ch..end_ch);
                                                self.buffer.insert_at_caret(s);
                                                let _ = save_settings(&self.to_settings());
                                                ui.close_menu();
                                                // clear stored right-click position so menu content won't persist
                                                self.last_right_click_pos = None;
                                            }
                                        }

                                        if ui.button("Add to dictionary").clicked() {
                                            self.spell_context = Some(SpellContext { word: word.clone(), start_byte, end_byte, screen_pos: egui::pos2(pointer_pos.x, pointer_pos.y + 6.0), suggestions: suggestions.clone() });
                                            ui.close_menu();
                                        }
                                        if ui.button("Ignore").clicked() {
                                            // ignore immediately and persist
                                            self.spell_dict.insert(word.clone());
                                            let _ = save_settings(&self.to_settings());
                                            ui.close_menu();
                                        }

                                        // stop after first matching word
                                        break;
                                    }
                                }
                            }
                        });
                    }

                    use egui::Event;

                    // (No visible drag handles; mouse selection is handled via
                    // galley-based mapping and pointer drag logic below.)

                    // Capture events once and use them both for global handling and
                    // vim-specific handling below. Make undo/redo available even
                    // when Vim emulation is disabled by handling Ctrl-Z / Ctrl-Y
                    // here unconditionally.
                    let events = ctx.input(|i| i.events.clone());
                    // remember canonical caret before events so we can clean up any
                    // accidental character insertions caused by modifier shortcuts
                    let caret_before_events = self.buffer.caret_char_range.as_ref().map(|r| r.start);

                    for ev in events.iter() {
                        if let Event::Key { key, pressed: true, modifiers, .. } = ev {
                            if modifiers.ctrl {
                                match key {
                                    egui::Key::Z => { self.buffer.undo(); }
                                    egui::Key::Y | egui::Key::R => { self.buffer.redo(); }
                                    _ => {}
                                }
                            }
                            
                        }
                    }

                    // Ensure any active undo group is ended on Escape regardless
                    // of whether Vim emulation is enabled. This prevents leaving
                    // grouped edits open when the user presses Escape or when
                    // other UI flows cause focus changes.
                    for ev in events.iter() {
                        if let Event::Key { key: egui::Key::Escape, pressed: true, .. } = ev {
                            self.buffer.end_undo_group();
                        }
                    }

                    // Handle mouse interactions inside the TextEdit widget so
                    // clicks and drag-selections update our vim buffer state
                    // regardless of mode. If the TextEdit is non-interactive
                    // (vim Normal mode) compute the clicked/selected char index
                    // from the laid-out galley so the caret still moves without
                    // giving the widget keyboard focus.
                    for ev in events.iter() {
                        // Capture right-click position so context menus can be anchored
                        if let Event::PointerButton { pos, pressed, button, .. } = ev {
                            if *button == egui::PointerButton::Secondary && *pressed {
                                if output.response.rect.contains(*pos) {
                                    self.last_right_click_pos = Some(*pos);
                                } else {
                                    self.last_right_click_pos = None;
                                }
                            }
                        }

                        if let Event::PointerButton { pos, pressed, button, .. } = ev {
                            if *button != egui::PointerButton::Primary { continue; }
                            // No special handle hit-testing; fall through to normal logic
                            if !output.response.rect.contains(*pos) { continue; }

                            // Prefer the widget-reported cursor_range when available
                            // but only when the TextEdit is interactive (Insert mode or Vim disabled).
                            if is_interactive {
                                if let Some(ccr) = output.cursor_range {
                                    let sorted = ccr.as_sorted_char_range();
                                    eprintln!("[dbg] pointer button (interactive): widget reported cursor_range -> {:?} pressed={}", sorted, pressed);
                                    self.buffer.caret_char_range = Some(sorted.clone());
                                    self.last_vim_key = None;

                                    if *pressed {
                                        // start drag using the reported cursor position
                                        self.mouse_dragging = true;
                                        self.mouse_drag_anchor = Some(sorted.start);
                                    }

                                    if !*pressed {
                                        if self.vim_enabled {
                                            if sorted.start != sorted.end {
                                                self.vim_mode = VimMode::Visual;
                                                self.visual_anchor = Some(sorted.start);
                                            } else if self.vim_mode == VimMode::Visual {
                                                self.vim_mode = VimMode::Normal;
                                                self.visual_anchor = None;
                                            }
                                        }
                                    } else if self.vim_enabled && self.vim_mode == VimMode::Visual {
                                        if self.visual_anchor.is_none() {
                                            self.visual_anchor = Some(sorted.start);
                                        }
                                    }
                                }
                            } else if !is_interactive {
                                // Widget didn't report a cursor_range (because it's non-interactive).
                                // Map the mouse `pos` into a character index using the galley.
                                let mut best = 0usize;
                                let mut best_dist = f32::INFINITY;
                                let total = self.buffer.char_len();
                                for idx in 0..=total {
                                    let cursor = CCursor::new(idx);
                                    let rect = output.galley.pos_from_cursor(cursor);
                                    let screen = output.response.rect.min + rect.min.to_vec2();
                                    let dx = screen.x - pos.x;
                                    let dy = screen.y - pos.y;
                                    let dist = dx * dx + dy * dy;
                                    if dist < best_dist {
                                        best_dist = dist;
                                        best = idx;
                                    }
                                }
                                eprintln!("[dbg] pointer button: galley mapped pos -> {} (pressed={})", best, pressed);
                                self.buffer.caret_char_range = Some(best..best);
                                self.last_vim_key = None;
                                if *pressed {
                                    // start drag
                                    self.mouse_dragging = true;
                                    self.mouse_drag_anchor = Some(best);
                                } else {
                                    // mouse released
                                    if self.mouse_dragging {
                                        // ended a drag
                                        self.mouse_dragging = false;
                                        if let Some(anchor) = self.mouse_drag_anchor.take() {
                                            let cur = best;
                                            if anchor != cur {
                                                eprintln!("[dbg] mouse drag end anchor={} cur={} -> selection {}..{}", anchor, cur, anchor.min(cur), anchor.max(cur));
                                                if self.vim_enabled {
                                                    self.vim_mode = VimMode::Visual;
                                                    // store the original anchor; caret is the cursor
                                                    self.visual_anchor = Some(anchor);
                                                }
                                                // Keep canonical caret at the cursor position
                                                self.buffer.caret_char_range = Some(cur..cur);
                                            }
                                        }
                                    } else {
                                        // single click without drag
                                        if self.vim_enabled {
                                            if self.vim_mode == VimMode::Visual {
                                                self.vim_mode = VimMode::Normal;
                                                self.visual_anchor = None;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Handle pointer movement to update selection during drag
                    for ev in events.iter() {
                        if let Event::PointerMoved(pos) = ev {
                            if !self.mouse_dragging { continue; }
                            // compute nearest char index to mouse pos
                            let pos = *pos;
                            let mut best = 0usize;
                            let mut best_dist = f32::INFINITY;
                            let total = self.buffer.char_len();
                            for idx in 0..=total {
                                let cursor = CCursor::new(idx);
                                let rect = output.galley.pos_from_cursor(cursor);
                                let screen = output.response.rect.min + rect.min.to_vec2();
                                let dx = screen.x - pos.x;
                                let dy = screen.y - pos.y;
                                let dist = dx * dx + dy * dy;
                                if dist < best_dist {
                                    best_dist = dist;
                                    best = idx;
                                }
                            }
                            if let Some(anchor) = self.mouse_drag_anchor {
                                let cur = best;
                                let s = anchor.min(cur);
                                let e = anchor.max(cur).min(self.buffer.char_len());
                                eprintln!("[dbg] pointer moved drag update anchor={} best={} -> selection {}..{}", anchor, best, s, e);
                                // Keep canonical caret at the current cursor index
                                self.buffer.caret_char_range = Some(cur..cur);
                                if self.vim_enabled {
                                    self.vim_mode = VimMode::Visual;
                                    // keep the anchor as the original anchor
                                    self.visual_anchor = Some(anchor);
                                }
                                self.last_vim_key = None;
                            }
                        }
                    }

                    // Vim-only handling: Escape and text input handling for modal editing
                    if self.vim_enabled {
                        for ev in events.iter() {
                            match ev {
                                Event::Key { key: egui::Key::Escape, pressed: true, .. } => {
                                    // Escape always returns to Normal mode; end any insert grouping
                                    self.vim_mode = VimMode::Normal;
                                    self.last_vim_key = None;
                                    self.buffer.end_undo_group();
                                }
                                Event::Text(text) => {
                                    if text.is_empty() {
                                        continue;
                                    }
                                    let ch = text.chars().next().unwrap();
                                    match self.vim_mode {
                                        VimMode::Normal | VimMode::Visual | VimMode::VisualLine => {
                                            let focus = vim::ReportBuffer::handle_normal_key(&mut self.buffer, &mut self.vim_mode, &mut self.last_vim_key, &mut self.last_vim_object, &mut self.vim_count, &mut self.visual_anchor, ch);
                                            if focus {
                                                output.response.request_focus();
                                                if let Some(range) = &self.buffer.caret_char_range {
                                                    let start = CCursor::new(range.start);
                                                    let end = CCursor::new(range.end);
                                                    output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                                                    output.state.clone().store(ctx, output.response.id);
                                                } else {
                                                    let pos = self.buffer.report.chars().count();
                                                    let start = CCursor::new(pos);
                                                    output.state.cursor.set_char_range(Some(CCursorRange::one(start)));
                                                    output.state.clone().store(ctx, output.response.id);
                                                }
                                            }
                                        }
                                        VimMode::Insert => {
                                            // in Insert mode, normal text events are handled by TextEdit; we only intercept Escape above
                                        }
                                        _ => {}
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    // If we already have a desired caret position (e.g. from an insert earlier this frame), push it into widget state
                    if self.vim_mode == VimMode::Visual || self.vim_mode == VimMode::VisualLine {
                        // When in Visual mode, prefer showing a selection between the visual anchor
                        // and the current caret. This drives the TextEdit's selection rendering.
                        if let Some(anchor) = self.visual_anchor {
                            let cur = self.buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                                let start_char;
                                let display_end;
                                if self.vim_mode == VimMode::VisualLine {
                                    // compute full-line bounds for anchor and caret
                                    let (a_s, a_e) = self.buffer.line_bounds_at(anchor);
                                    let (c_s, c_e) = self.buffer.line_bounds_at(cur);
                                    start_char = a_s.min(c_s);
                                    let e = a_e.max(c_e).min(self.buffer.char_len());
                                    display_end = e;
                                } else {
                                    let s = anchor.min(cur);
                                    let e = anchor.max(cur);
                                    // Extend the shown cursor range for any non-empty selection
                                    // so the final character is included (ranges are end-exclusive).
                                    let extend = if e > s { 1 } else { 0 };
                                    start_char = s;
                                    display_end = (e).min(self.buffer.char_len()).saturating_add(extend);
                                }
                                let s = start_char;
                                let cur_pos = cur;
                                // Ensure textual selection mirrors visual selection
                                eprintln!("[dbg] visual sync anchor={} cur={} -> display {}..{}", anchor, cur_pos, s, display_end);
                                // Keep canonical caret as the caret position (`cur..cur`).
                                // The visible selection is driven by `visual_anchor` + the caret.
                                self.buffer.caret_char_range = Some(cur_pos..cur_pos);
                                let start = CCursor::new(s);
                                let end = CCursor::new(display_end);
                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                            output.state.store(ui.ctx(), output.response.id);
                        } else if let Some(range) = &self.buffer.caret_char_range {
                            let start = CCursor::new(range.start);
                            let end = CCursor::new(range.end);
                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                            output.state.store(ui.ctx(), output.response.id);
                        }
                    } else {
                        if let Some(range) = &self.buffer.caret_char_range {
                            let start = CCursor::new(range.start);
                            let end = CCursor::new(range.end);
                            output.state.cursor.set_char_range(Some(CCursorRange::two(start, end)));
                            output.state.store(ui.ctx(), output.response.id);
                        }
                    }

                    // Draw an unfocused caret indicator so the user can see the caret when the editor is not focused.
                    if !output.response.has_focus() {
                        // Try to use the widget-reported cursor_range, otherwise fall back to our stored `caret_char_range`.
                        let maybe_ccr = output.cursor_range.or_else(|| {
                            self.buffer.caret_char_range.as_ref().map(|r| CCursorRange::two(CCursor::new(r.start), CCursor::new(r.end)))
                        });

                        if let Some(ccr) = maybe_ccr {
                            // Use the primary cursor position
                            let ccursor = ccr.primary;
                            // Position inside the laid-out galley (pos_from_cursor returns a Rect)
                            let galley_pos = output.galley.pos_from_cursor(ccursor);
                            // Convert to screen coords: widget rect min + galley_pos offset
                            // Note: `galley_pos` is already positioned relative to the widget; avoid double-adding `output.galley_pos`.
                            let screen_pos = output.response.rect.min + galley_pos.min.to_vec2();
                            // Prepare painter for selection/caret drawing
                            let painter = ui.painter();

                            // If Visual mode is active and we have an anchor, draw a selection background
                            if self.vim_mode == VimMode::Visual || self.vim_mode == VimMode::VisualLine {
                                if let Some(anchor) = self.visual_anchor {
                                    let cur = self.buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                                    let (s, e_display) = if self.vim_mode == VimMode::VisualLine {
                                        let (a_s, a_e) = self.buffer.line_bounds_at(anchor);
                                        let (c_s, c_e) = self.buffer.line_bounds_at(cur);
                                        let s = a_s.min(c_s);
                                        let e = a_e.max(c_e).min(self.buffer.char_len());
                                        (s, e)
                                    } else {
                                        let s = anchor.min(cur);
                                        let e = anchor.max(cur);
                                        let e_display = if e > s { e.saturating_add(1) } else { e };
                                        (s, e_display)
                                    };
                                    if s < e_display {
                                        // Determine exact per-line glyph bounds by splitting the selected
                                        // substring on newline boundaries and mapping each segment back
                                        // to absolute char indices to query `pos_from_cursor`.
                                        let report = &self.buffer.report;
                                        // Avoid drawing a trailing empty segment when the selection
                                        // ends exactly at a newline in VisualLine mode; that
                                        // would produce a caret-width highlight on the next line.
                                        let mut draw_end = e_display;
                                        if self.vim_mode == VimMode::VisualLine && e_display > s {
                                            if report.chars().nth(e_display.saturating_sub(1)) == Some('\n') {
                                                draw_end = e_display.saturating_sub(1);
                                            }
                                        }
                                        let sel = report.chars().skip(s).take(draw_end.saturating_sub(s)).collect::<String>();
                                        let mut offset = 0usize;
                                        let mut abs_index = s;
                                        for (i, line) in sel.split('\n').enumerate() {
                                            let line_len = line.chars().count();
                                            let line_start = abs_index;
                                            let line_end = abs_index + line_len;

                                            // compute screen coords for line_start and line_end (end is exclusive)
                                            let start_cursor = CCursor::new(line_start);
                                            let end_cursor = CCursor::new(line_end);
                                            let start_pos = output.galley.pos_from_cursor(start_cursor);
                                            let mut end_pos = output.galley.pos_from_cursor(end_cursor);
                                            // Also compute the previous glyph rect and use the maximum
                                            // right edge to ensure the last character is included
                                            let prev_pos_opt = if line_len > 0 {
                                                let prev_idx = line_end.saturating_sub(1);
                                                Some(output.galley.pos_from_cursor(CCursor::new(prev_idx)))
                                            } else { None };

                                            let start_screen = output.response.rect.min + start_pos.min.to_vec2();
                                            let mut end_screen_x = output.response.rect.min.x + end_pos.max.x;
                                            if let Some(prev_pos) = prev_pos_opt {
                                                let prev_x = output.response.rect.min.x + prev_pos.max.x;
                                                if prev_x > end_screen_x {
                                                    end_screen_x = prev_x;
                                                }
                                            }
                                            let end_screen = egui::pos2(end_screen_x, output.response.rect.min.y);

                                            // derive a per-line height from the glyph extents
                                            let line_h = (start_pos.max.y - start_pos.min.y).abs().max(14.0_f32).min(48.0_f32);

                                            // For empty lines (line_len == 0), draw a caret-width selection
                                            let x0 = if line_len == 0 { start_screen.x } else { start_screen.x };
                                            let x1 = if line_len == 0 { start_screen.x + 8.0 } else { end_screen.x };
                                            let y0 = start_screen.y.clamp(output.response.rect.min.y, output.response.rect.max.y);
                                            let y1 = (y0 + line_h).min(output.response.rect.max.y);
                                            let sel_rect = egui::Rect::from_min_max(egui::pos2(x0.clamp(output.response.rect.min.x, output.response.rect.max.x), y0), egui::pos2(x1.clamp(output.response.rect.min.x, output.response.rect.max.x), y1));
                                            painter.rect_filled(sel_rect, 0.0, egui::Color32::from_rgba_unmultiplied(120, 160, 255, 140));

                                            // advance absolute index past this line and the newline (if present)
                                            abs_index = line_end + 1; // skip the newline; safe even if at end because we'll not use abs_index further
                                            offset += line_len + 1;
                                            if abs_index > draw_end { break; }
                                        }
                                    }
                                }
                            }
                            // No visible drag handles (selection is shown in-text).
                            // Draw a visible caret. When Vim emulation is enabled draw a thick block;
                            // otherwise draw a thin caret marker so it doesn't persist as a wide indicator.
                            let caret_height = 18.0_f32;
                            // Use the galley cursor rect width as a best-effort char width; fallback to 8.0
                            let mut char_w = (galley_pos.max.x - galley_pos.min.x).abs();
                            if char_w <= 0.1 {
                                char_w = 8.0;
                            }
                            // If vim is disabled use a narrow caret (1.0-2.0 px) instead of block width
                            let caret_w = if self.vim_enabled { char_w } else { 1.5_f32 };
                            // Nudge the caret slightly right for visual alignment
                            let nudge = 2.0_f32;
                            let x = (screen_pos.x + self.caret_x_offset + nudge).clamp(output.response.rect.min.x, output.response.rect.max.x - 1.0);
                            let y0 = screen_pos.y.clamp(output.response.rect.min.y, output.response.rect.max.y - caret_height);
                            let y1 = y0 + caret_height;
                            let x2 = (x + caret_w).min(output.response.rect.max.x - 1.0);
                            let rect = egui::Rect::from_min_max(egui::pos2(x, y0), egui::pos2(x2, y1));
                            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(255, 80, 80));
                            if self.show_caret_debug {
                                painter.circle_filled(egui::pos2(x, y0), 4.0, egui::Color32::from_rgb(80, 200, 255));
                                let info = format!("screen: {:.1},{:.1}  char_range: {:?}", screen_pos.x + self.caret_x_offset, screen_pos.y, self.buffer.caret_char_range);
                                painter.text(
                                    egui::pos2(output.response.rect.min.x + 6.0, output.response.rect.min.y + 6.0),
                                    egui::Align2::LEFT_TOP,
                                    info,
                                    egui::FontId::proportional(12.0),
                                    egui::Color32::WHITE,
                                );
                            }
                        }
                    }

                    if self.attach_requested {
                        if let Some(writers) = &self.ipc_writers {
                            let rect = output.response.rect;
                            let x = rect.min.x.round() as i32;
                            let y = rect.min.y.round() as i32;
                            let w = rect.width().round() as i32;
                            let h = rect.height().round() as i32;
                            let cmd = serde_json::json!({
                                "cmd": "show_overlay",
                                "text": self.buffer.report,
                                "x": x,
                                "y": y,
                                "w": w,
                                "h": h,
                            });
                            dragon_ipc::send_to_helpers(writers, &cmd);
                        }
                        self.attach_requested = false;
                    }

                    // Show caret position below the text edit so user always sees it
                    if let Some(range) = &self.buffer.caret_char_range {
                        let sel_text = if range.start != range.end {
                            format!("Selection: {}-{}", range.start, range.end)
                        } else {
                            "Selection: -".to_string()
                        };
                        ui.label(format!("Caret: {}    {}", range.end, sel_text));
                    } else {
                        ui.label("Caret: -    Selection: -");
                    }

                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.show_caret_debug, "Debug caret pos");
                        let vim_resp = ui.checkbox(&mut self.vim_enabled, "Vim emulation");
                        if vim_resp.changed() {
                            // ensure we always reset modal state when toggling
                            if self.vim_enabled {
                                // enabling vim: start in Normal mode
                                self.vim_mode = VimMode::Normal;
                                self.last_vim_key = None;
                                self.last_vim_object = None;
                                // preserve caret selection as-is
                            } else {
                                // disabling vim: clear any visual-mode artifacts
                                self.vim_mode = VimMode::Normal;
                                self.last_vim_key = None;
                                self.last_vim_object = None;
                                self.visual_anchor = None;
                                // collapse any selection to a canonical caret position
                                let cur = self.buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                                self.buffer.caret_char_range = Some(cur..cur);
                            }
                            // persist settings when user toggles Vim emulation
                            let _ = save_settings(&self.to_settings());
                        }
                        let mode_label = match self.vim_mode {
                            VimMode::Normal => "Normal",
                            VimMode::Insert => "Insert",
                            VimMode::Visual => "Visual",
                            VimMode::VisualLine => "Visual-Line",
                        };
                        ui.label(format!("Mode: {}", mode_label));
                    });
                    if self.show_caret_debug {
                        ui.add(egui::Slider::new(&mut self.caret_x_offset, -40.0..=40.0).text("Caret X offset"));
                    }

                    ui.horizontal(|ui| {
                        if ui.button("Preview").clicked() {
                            ctx.request_repaint();
                        }
                        if ui.button("Insert Template").clicked() {
                            self.show_templates_window = true;
                        }
                    });

                    ui.separator();

                    ui.horizontal(|ui| {
                        ui.label("Overlay X:");
                        ui.add(egui::DragValue::new(&mut self.overlay_x));
                        ui.label("Y:");
                        ui.add(egui::DragValue::new(&mut self.overlay_y));
                        ui.label("W:");
                        ui.add(egui::DragValue::new(&mut self.overlay_w));
                        ui.label("H:");
                        ui.add(egui::DragValue::new(&mut self.overlay_h));
                        if ui.button("Send Overlay Position").clicked() {
                            if let Some(writers) = &self.ipc_writers {
                                let cmd = serde_json::json!({
                                    "cmd": "set_overlay_position",
                                    "x": self.overlay_x,
                                    "y": self.overlay_y,
                                    "w": self.overlay_w,
                                    "h": self.overlay_h
                                });
                                dragon_ipc::send_to_helpers(writers, &cmd);
                            }
                        }
                    });
                });
            });

            // show spell suggestion window (if any)
            self.show_spell_window(ctx);

            // Templates area: render in the right column when requested
            if self.show_templates_window {
                ui.allocate_ui_at_rect(right_rect, |ui| {
                    ui.vertical(|ui| {
                        ui.heading("Templates");
                        ui.horizontal(|ui| {
                            if ui.small_button("Global Vars").clicked() {
                                // populate edit buffer from current global_vars
                                let txt = self.global_vars.iter().map(|(k,v)| format!("{}: {}", k, v)).collect::<Vec<_>>().join("\n");
                                self.global_vars_text = txt;
                                self.show_global_vars_dialog = true;
                            }
                        });
                        ui.separator();

                        ui.horizontal(|ui| {
                            ui.label("NICIP codes (comma-separated):");
                            ui.text_edit_singleline(&mut self.template_nicip);
                        });

                        ui.horizontal(|ui| {
                            ui.label("Search:");
                            ui.text_edit_singleline(&mut self.template_search);
                            if ui.button("Hide").clicked() {
                                self.show_templates_window = false;
                            }
                        });

                        // Template creation/edit/delete buttons removed — view-only in main UI.

                        let nicips: Vec<String> = self
                            .template_nicip
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();

                        // Build a list of visible template indices after applying filters so
                        // we can both render numeric shortcuts and react to Alt+<n> keys.
                        let templates_list = self.templates.clone();
                        let mut visible_indices: Vec<usize> = Vec::new();
                        for (i, t) in templates_list.iter().enumerate() {
                            if !nicips.is_empty() {
                                if !t.applicable_codes.is_empty() {
                                    let mut matched = false;
                                    for sc in &nicips {
                                        if t.applicable_codes.iter().any(|ac| ac.eq_ignore_ascii_case(sc)) {
                                            matched = true;
                                            break;
                                        }
                                    }
                                    if !matched {
                                        continue;
                                    }
                                }
                            }
                            let title = t.display_title();
                            if !self.template_search.is_empty()
                                && !title.to_lowercase().contains(&self.template_search.to_lowercase())
                                && !t.body.to_lowercase().contains(&self.template_search.to_lowercase())
                            {
                                continue;
                            }
                            visible_indices.push(i);
                        }

                        // Render visible templates with numeric prefixes (1-based) and an Insert button.
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            for (pos, &i) in visible_indices.iter().enumerate() {
                                let t = &templates_list[i];
                                let title = t.display_title();
                                let num = pos + 1; // 1-based numbering for shortcut keys
                                ui.horizontal(|ui| {
                                    ui.label(format!("[{}]", num));
                                    // small insertion-mode icon
                                    if t.insert_inline {
                                        ui.label("🔡 Inline");
                                    } else if t.ensure_surrounding_newlines {
                                        ui.label("📦 Block");
                                    } else {
                                        ui.label("↔️ Soft");
                                    }
                                    if ui.small_button("Insert").clicked() {
                                        self.insert_template_at_caret(t);
                                    }
                                    // per-template vars editing removed from this panel (use dedicated dialog)
                                    let selected = self.selected_template.map(|s| s == i).unwrap_or(false);
                                    if ui.selectable_label(selected, title).clicked() {
                                        if selected {
                                            self.selected_template = None;
                                        } else {
                                            self.selected_template = Some(i);
                                        }
                                    }
                                });
                            }
                        });

                        ui.separator();
                        // Show details for the currently selected template (if any)
                        if let Some(idx) = self.selected_template {
                            if idx < self.templates.len() {
                                let t = &self.templates[idx];
                                ui.group(|ui| {
                                    ui.label("Template details:");
                                    ui.horizontal(|ui| {
                                        ui.label("ID:");
                                        ui.monospace(t.id.clone().unwrap_or_default());
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Title:");
                                        ui.monospace(t.title.clone().unwrap_or_default());
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("NICIP codes:");
                                        ui.monospace(t.applicable_codes.join(", "));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Modalities:");
                                        ui.monospace(t.modalities.join(", "));
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Insert inline:");
                                        ui.label(if t.insert_inline { "yes" } else { "no" });
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Ensure surrounding newlines:");
                                        ui.label(if t.ensure_surrounding_newlines { "yes" } else { "no" });
                                    });
                                    ui.horizontal(|ui| {
                                        ui.label("Body preview:");
                                        ui.checkbox(&mut self.preview_replace_vars, "Replace variables");
                                    });
                                    let mut preview = if self.preview_replace_vars {
                                        // build merged vars: template defaults <- user overrides <- global vars (fallback)
                                        let mut vars: std::collections::HashMap<String, String> = t.vars.clone();
                                        let key = t.id.clone().unwrap_or_else(|| t.title.clone().unwrap_or_else(|| t.display_title()));
                                        if let Some(user_map) = self.user_template_vars.get(&key) {
                                            for (k, v) in user_map.iter() {
                                                vars.insert(k.clone(), v.clone());
                                            }
                                        }
                                        for (k, v) in self.global_vars.iter() {
                                            vars.entry(k.clone()).or_insert_with(|| v.clone());
                                        }
                                        templates::render_template(&t.body, &vars, &self.templates)
                                    } else {
                                        t.body.clone()
                                    };
                                    ui.add(egui::TextEdit::multiline(&mut preview).desired_rows(8).font(egui::TextStyle::Monospace).interactive(false));
                                    ui.horizontal(|ui| {
                                        // per-template vars editing removed from this panel
                                    });
                                });
                            }
                        }
                    });
                });
            }

            // Edit-vars dialog (per-user overrides)
            if let Some(key) = self.show_edit_vars_dialog.clone() {
                let mut open = true;
                let txt = &mut self.edit_vars_text;
                egui::Window::new("Edit template variables").open(&mut open).show(ctx, |ui| {
                    ui.label(format!("Template: {}", key));
                    ui.label("Enter one key: value per line:");
                    ui.add(egui::TextEdit::multiline(txt).desired_rows(10));
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            // parse into map
                            let mut m = HashMap::new();
                            for line in txt.lines() {
                                if let Some(idx) = line.find(':') {
                                    let k = line[..idx].trim();
                                    let v = line[idx+1..].trim();
                                    if !k.is_empty() {
                                        m.insert(k.to_string(), v.to_string());
                                    }
                                }
                            }
                            self.user_template_vars.insert(key.clone(), m);
                            self.user_template_vars_dirty = true;
                            self.show_edit_vars_dialog = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.show_edit_vars_dialog = None;
                        }
                    });
                });
                if !open { self.show_edit_vars_dialog = None; }
                // persist settings after dialog closed if dirty
                if self.user_template_vars_dirty {
                    let _ = save_settings(&self.to_settings());
                    self.user_template_vars_dirty = false;
                }
            }

                // Global variables dialog (centralised vars)
                if self.show_global_vars_dialog {
                    let mut open = true;
                    // collect names first to avoid simultaneous mutable/immutable borrows of self
                    let var_names = self.collect_template_var_names();
                    egui::Window::new("Global Variables").open(&mut open).show(ctx, |ui| {
                        ui.label("Variables referenced by templates (marked = present in global vars):");
                        ui.horizontal_wrapped(|ui| {
                            for name in var_names.iter() {
                                let present = self.global_vars.contains_key(name);
                                if present {
                                    ui.colored_label(egui::Color32::LIGHT_GREEN, format!("{} ✓", name));
                                } else {
                                    ui.colored_label(egui::Color32::LIGHT_RED, format!("{}", name));
                                }
                            }
                        });
                        ui.separator();
                        ui.label("Enter one key: value per line:");
                        ui.add(egui::TextEdit::multiline(&mut self.global_vars_text).desired_rows(12));
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                let mut m = HashMap::new();
                                for line in self.global_vars_text.lines() {
                                    if let Some(idx) = line.find(':') {
                                        let k = line[..idx].trim();
                                        let v = line[idx+1..].trim();
                                        if !k.is_empty() {
                                            m.insert(k.to_string(), v.to_string());
                                        }
                                    }
                                }
                                self.global_vars = m;
                                self.global_vars_dirty = true;
                                self.show_global_vars_dialog = false;
                            }
                            if ui.button("Cancel").clicked() {
                                self.show_global_vars_dialog = false;
                            }
                        });
                    });
                    if !open { self.show_global_vars_dialog = false; }
                    if self.global_vars_dirty {
                        let _ = save_settings(&self.to_settings());
                        self.global_vars_dirty = false;
                    }
                }

            // Template editor window
            if self.show_template_editor {
                let mut open = self.show_template_editor;
                egui::Window::new("Template Editor").open(&mut open).show(ctx, |ui| {
                    if let Some(mut t) = self.editing_template.clone() {
                        ui.horizontal(|ui| {
                            ui.label("ID:");
                            let mut idv = t.id.clone().unwrap_or_default();
                            ui.text_edit_singleline(&mut idv);
                            if idv.is_empty() { t.id = None; } else { t.id = Some(idv); }
                        });
                        ui.horizontal(|ui| {
                            ui.label("Title:");
                            let mut tv = t.title.clone().unwrap_or_default();
                            ui.text_edit_singleline(&mut tv);
                            if tv.is_empty() { t.title = None; } else { t.title = Some(tv); }
                        });
                        ui.horizontal(|ui| {
                            ui.label("NICIP codes (comma):");
                            let mut codes = t.applicable_codes.join(",");
                            ui.text_edit_singleline(&mut codes);
                            t.applicable_codes = codes.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                        });
                        ui.horizontal(|ui| {
                            ui.label("Modalities (comma):");
                            let mut mods = t.modalities.join(",");
                            ui.text_edit_singleline(&mut mods);
                            t.modalities = mods.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
                        });
                        ui.label("Body:");
                        ui.add(egui::TextEdit::multiline(&mut t.body).desired_rows(12));
                        ui.horizontal(|ui| {
                            if ui.button("Save").clicked() {
                                // ensure user templates dir
                                let user_dir = std::path::Path::new("templates/user");
                                let _ = std::fs::create_dir_all(user_dir);
                                // pick filename from id or title
                                let fname_base = t.id.as_ref().or_else(|| t.title.as_ref()).map(|s| s.clone()).unwrap_or_else(|| format!("template_{}", chrono::Utc::now().timestamp()));
                                let mut safe = fname_base.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect::<String>();
                                if safe.is_empty() { safe = format!("template_{}", chrono::Utc::now().timestamp()); }
                                let path = user_dir.join(format!("{}.yml", safe));
                                if let Ok(yml) = serde_yaml::to_string(&t) {
                                    let _ = std::fs::write(&path, yml);
                                }
                                // refresh templates list
                                self.templates = templates::load_templates();
                                self.show_template_editor = false;
                                self.editing_template = None;
                                self.editing_index = None;
                                return;
                            }
                            if ui.button("Cancel").clicked() {
                                self.show_template_editor = false;
                                self.editing_template = None;
                                self.editing_index = None;
                                return;
                            }
                        });
                        // save changes back into editing_template state while open
                        self.editing_template = Some(t);
                    } else {
                        ui.label("No template loaded.");
                    }
                });
                self.show_template_editor = open;
            }
        });
    }
}

fn main() {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Diforge — Radiology Report Editor",
        native_options,
        Box::new(|_cc| Ok(Box::new(ReportApp::default()))),
    );
}

