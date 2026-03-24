use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(PartialEq, Eq, Serialize, Deserialize, Clone, Copy, Debug)]
pub enum VimMode {
    Normal,
    Insert,
        Visual,
        VisualLine,
}

impl Default for VimMode {
    fn default() -> Self {
        VimMode::Normal
    }
}

pub struct ReportBuffer {
    pub report: String,
    pub caret_char_range: Option<Range<usize>>,
    // undo/redo stacks store previous buffer states
    history: Vec<(String, Option<Range<usize>>)>,
    redo: Vec<(String, Option<Range<usize>>)>,
    // when true, modifications are grouped into a single undo step
    in_undo_group: bool,
}

impl ReportBuffer {
    pub fn new() -> Self {
        Self {
            report: String::new(),
            caret_char_range: None,
            history: Vec::new(),
            redo: Vec::new(),
            in_undo_group: false,
        }
    }

    fn snapshot(&self) -> (String, Option<Range<usize>>) {
        (self.report.clone(), self.caret_char_range.clone())
    }

    fn restore_snapshot(&mut self, snap: (String, Option<Range<usize>>)) {
        self.report = snap.0;
        self.caret_char_range = snap.1;
    }

    /// Start grouping subsequent edits into a single undo step.
    pub fn start_undo_group(&mut self) {
        if !self.in_undo_group {
            self.history.push(self.snapshot());
            self.redo.clear();
            self.in_undo_group = true;
        }
    }

    /// End grouping edits.
    pub fn end_undo_group(&mut self) {
        self.in_undo_group = false;
    }

    /// Push an undo snapshot unless we're currently grouping edits.
    fn push_undo_snapshot(&mut self) {
        if !self.in_undo_group {
            self.history.push(self.snapshot());
            self.redo.clear();
        }
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.history.pop() {
            let cur = self.snapshot();
            self.redo.push(cur);
            self.restore_snapshot(prev);
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            let cur = self.snapshot();
            self.history.push(cur);
            self.restore_snapshot(next);
        }
    }

    pub fn insert_at_caret(&mut self, insert: &str) {
        self.push_undo_snapshot();
        let (start_char, end_char) = if let Some(r) = &self.caret_char_range {
            (r.start, r.end)
        } else {
            (self.report.chars().count(), self.report.chars().count())
        };

        let mut cur = 0usize;
        let mut start_byte = self.report.len();
        let mut end_byte = self.report.len();
        for (b, _) in self.report.char_indices() {
            if cur == start_char {
                start_byte = b;
            }
            if cur == end_char {
                end_byte = b;
                break;
            }
            cur += 1;
        }
        if start_char >= self.report.chars().count() {
            start_byte = self.report.len();
        }
        if end_char >= self.report.chars().count() {
            end_byte = self.report.len();
        }

        self.report.replace_range(start_byte..end_byte, insert);

        let new_char_pos = start_char + insert.chars().count();
        self.caret_char_range = Some(new_char_pos..new_char_pos);
    }

    pub fn char_len(&self) -> usize {
        self.report.chars().count()
    }

    fn char_to_byte_index(&self, char_pos: usize) -> usize {
        let mut cur = 0usize;
        for (b, _) in self.report.char_indices() {
            if cur == char_pos {
                return b;
            }
            cur += 1;
        }
        self.report.len()
    }

    fn delete_range(&mut self, start_char: usize, end_char: usize) {
        if start_char >= end_char { return; }
        self.push_undo_snapshot();
        let start_byte = self.char_to_byte_index(start_char);
        let end_byte = self.char_to_byte_index(end_char);
        if start_byte <= end_byte && end_byte <= self.report.len() {
            self.report.replace_range(start_byte..end_byte, "");
            self.caret_char_range = Some(start_char..start_char);
        }
    }

    fn next_word_start_from(&self, pos: usize) -> usize {
        let chars: Vec<char> = self.report.chars().collect();
        let mut p = pos;
        let n = chars.len();
        if p < n && chars[p].is_alphanumeric() {
            while p < n && chars[p].is_alphanumeric() { p += 1; }
        }
        while p < n && !chars[p].is_alphanumeric() { p += 1; }
        p
    }

    fn prev_word_start_from(&self, pos: usize) -> usize {
        let chars: Vec<char> = self.report.chars().collect();
        if pos == 0 { return 0; }
        let mut i = pos;
        while i > 0 && !chars[i - 1].is_alphanumeric() { i -= 1; }
        while i > 0 && chars[i - 1].is_alphanumeric() { i -= 1; }
        i
    }

    fn word_end_from(&self, pos: usize) -> usize {
        let chars: Vec<char> = self.report.chars().collect();
        let mut p = pos;
        let n = chars.len();
        // advance to start of next word
        while p < n && !chars[p].is_alphanumeric() { p += 1; }
        // advance to end of that word
        while p < n && chars[p].is_alphanumeric() { p += 1; }
        if p > 0 { p - 1 } else { 0 }
    }

    /// Determine the start (inclusive) and end (exclusive) char indices of
    /// the word that contains `pos` or is nearest after it. Returns None if
    /// no word found.
    fn current_word_bounds(&self, pos: usize) -> Option<(usize, usize)> {
        let chars: Vec<char> = self.report.chars().collect();
        let n = chars.len();
        if n == 0 { return None; }
        let mut p = pos;
        if p >= n {
            p = n.saturating_sub(1);
        }
        // If on a non-alnum and previous is alnum, consider previous char
        if !chars[p].is_alphanumeric() && p > 0 && chars[p - 1].is_alphanumeric() {
            p -= 1;
        }
        // If still not on a word, try to find next word
        if !chars[p].is_alphanumeric() {
            let s = self.next_word_start_from(p);
            if s >= n { return None; }
            p = s;
        }
        // p is inside a word
        let mut start = p;
        while start > 0 && chars[start - 1].is_alphanumeric() { start -= 1; }
        let mut end = p;
        while end < n && chars[end].is_alphanumeric() { end += 1; }
        Some((start, end))
    }

    pub fn set_caret_pos(&mut self, pos: usize) {
        let p = pos.min(self.char_len());
        self.caret_char_range = Some(p..p);
    }

    pub fn move_caret_by(&mut self, delta: isize) {
        let cur = self.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
        let new = if delta < 0 {
            cur.saturating_sub((-delta) as usize)
        } else {
            (cur + delta as usize).min(self.char_len())
        };
        self.set_caret_pos(new);
    }

    pub fn move_word_forward(&mut self) {
        let chars: Vec<char> = self.report.chars().collect();
        let mut pos = self.caret_char_range.as_ref().map(|r| r.end).unwrap_or(0);
        let n = chars.len();
        // If currently on/inside a word, advance to its end first.
        if pos < n && chars[pos].is_alphanumeric() {
            while pos < n && chars[pos].is_alphanumeric() {
                pos += 1;
            }
        }
        // Then skip separators to land on the start of the next word.
        while pos < n && !chars[pos].is_alphanumeric() {
            pos += 1;
        }
        self.set_caret_pos(pos);
    }

    pub fn move_word_backward(&mut self) {
        let chars: Vec<char> = self.report.chars().collect();
        let mut pos = self.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
        if pos == 0 {
            return;
        }
        let mut i = pos;
        while i > 0 && !chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        while i > 0 && chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        self.set_caret_pos(i);
    }

    pub fn move_word_end(&mut self) {
        let chars: Vec<char> = self.report.chars().collect();
        let mut pos = self.caret_char_range.as_ref().map(|r| r.end).unwrap_or(0);
        let n = chars.len();
        // If we're already at the end of a word (either the caret is on the
        // last character of a word, or it's immediately after a word), advance
        // one position so we find the *next* word end (Vim's `e` behavior).
        let at_end_of_word = if pos < n {
            chars[pos].is_alphanumeric() && (pos + 1 == n || !chars[pos + 1].is_alphanumeric())
        } else {
            false
        };
        let after_word = pos > 0 && (pos == n || !chars[pos].is_alphanumeric()) && chars[pos - 1].is_alphanumeric();
        if at_end_of_word || after_word {
            pos = pos.saturating_add(1);
        }

        while pos < n && !chars[pos].is_alphanumeric() {
            pos += 1;
        }
        while pos < n && chars[pos].is_alphanumeric() {
            pos += 1;
        }
        if pos > 0 { pos -= 1; }
        self.set_caret_pos(pos);
    }

    pub fn move_to_line_bounds(&self) -> (usize, usize, usize) {
        let chars: Vec<char> = self.report.chars().collect();
        let raw_pos = self.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
        let n = chars.len();
        let pos = raw_pos.min(n);
        let mut start = 0usize;
        for i in (0..pos).rev() {
            if chars[i] == '\n' {
                start = i + 1;
                break;
            }
        }
        let mut end = n;
        for i in pos..n {
            if chars[i] == '\n' {
                end = i + 1;
                break;
            }
        }
        let col = pos.saturating_sub(start);
        (start, end, col)
    }

    /// Return the (start, end) char indices for the line containing `pos`.
    pub fn line_bounds_at(&self, pos: usize) -> (usize, usize) {
        let chars: Vec<char> = self.report.chars().collect();
        let n = chars.len();
        if n == 0 { return (0, 0); }
        let p = pos.min(n);
        let mut start = 0usize;
        for i in (0..p).rev() {
            if chars[i] == '\n' {
                start = i + 1;
                break;
            }
        }
        let mut end = n;
        for i in p..n {
            if chars[i] == '\n' {
                end = i + 1;
                break;
            }
        }
        (start, end)
    }

    pub fn move_line_up(&mut self) {
        let chars: Vec<char> = self.report.chars().collect();
        let pos = self.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
        let mut line_starts = Vec::new();
        let n = chars.len();
        line_starts.push(0usize);
        for i in 0..n {
            if chars[i] == '\n' && i + 1 < n {
                line_starts.push(i + 1);
            }
        }
        let mut line_idx = 0usize;
        for (i, &s) in line_starts.iter().enumerate() {
            if s <= pos { line_idx = i; } else { break; }
        }
        if line_idx == 0 { return; }
        let (_, cur_end, col) = self.move_to_line_bounds();
        let prev_start = line_starts[line_idx - 1];
        let mut prev_end = n;
        for i in prev_start..n {
            if chars[i] == '\n' { prev_end = i + 1; break; }
        }
        let target = (prev_start + col).min(prev_end.saturating_sub(1));
        self.set_caret_pos(target);
    }

    pub fn move_line_down(&mut self) {
        let chars: Vec<char> = self.report.chars().collect();
        let pos = self.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
        let mut line_starts = Vec::new();
        let n = chars.len();
        line_starts.push(0usize);
        for i in 0..n {
            if chars[i] == '\n' && i + 1 < n {
                line_starts.push(i + 1);
            }
        }
        let mut line_idx = 0usize;
        for (i, &s) in line_starts.iter().enumerate() {
            if s <= pos { line_idx = i; } else { break; }
        }
        if line_idx + 1 >= line_starts.len() { return; }
        let (cur_start, cur_end, col) = self.move_to_line_bounds();
        let next_start = line_starts[line_idx + 1];
        let mut next_end = n;
        for i in next_start..n {
            if chars[i] == '\n' { next_end = i + 1; break; }
        }
        let target = (next_start + col).min(next_end.saturating_sub(1));
        self.set_caret_pos(target);
    }

    /// Return the start (char index) of the line containing `pos` or the
    /// start of the previous line when moving upward. Used by operator ranges.
    fn prev_line_start_from(&self, pos: usize) -> usize {
        let chars: Vec<char> = self.report.chars().collect();
        if pos == 0 { return 0; }
        let mut i = pos.saturating_sub(1);
        while i > 0 {
            if chars[i] == '\n' { return i + 1; }
            i = i.saturating_sub(1);
        }
        0
    }

    /// Delete `count` lines starting at `start_char` (char indices). If the
    /// requested range extends past EOF, clamp to EOF.
    fn delete_lines_from(&mut self, start_char: usize, count: usize) {
        if count == 0 { return; }
        let chars: Vec<char> = self.report.chars().collect();
        let n = chars.len();
        if start_char >= n {
            return;
        }
        let mut end = start_char;
        let mut lines_deleted = 0usize;
        while end < n && lines_deleted < count {
            if let Some(pos) = chars[end..].iter().position(|&c| c == '\n') {
                end = end + pos + 1; // include the newline
            } else {
                end = n;
            }
            lines_deleted += 1;
        }
        if start_char < end {
            self.delete_range(start_char, end);
        }
    }

    /// Move caret to the start of the specified 1-based line number. If the
    /// line number is out of range, clamp to the first/last line.
    pub fn goto_line_start(&mut self, line_1based: usize) {
        if line_1based == 0 {
            return;
        }
        let chars: Vec<char> = self.report.chars().collect();
        let n = chars.len();
        if n == 0 {
            self.set_caret_pos(0);
            return;
        }
        let mut line = 1usize;
        let mut pos = 0usize;
        if line_1based == 1 {
            self.set_caret_pos(0);
            return;
        }
        while pos < n && line < line_1based {
            if chars[pos] == '\n' {
                line += 1;
                if line == line_1based {
                    pos += 1; // move to char after newline
                    break;
                }
            }
            pos += 1;
        }
        // If we reached end, clamp to end-of-file start
        if pos > n { pos = n; }
        self.set_caret_pos(pos);
    }

    /// Move caret to the end of the buffer (after the last character).
    pub fn goto_end_of_file(&mut self) {
        let pos = self.char_len();
        self.set_caret_pos(pos);
    }

    pub fn delete_char_at_cursor(&mut self) {
        if let Some(range) = &self.caret_char_range {
            let pos = range.start;
            let total = self.report.chars().count();
            if pos < total {
                self.push_undo_snapshot();
                let mut cur = 0usize;
                let mut bstart = self.report.len();
                let mut bend = self.report.len();
                for (b, _) in self.report.char_indices() {
                    if cur == pos {
                        bstart = b;
                    }
                    if cur == pos + 1 {
                        bend = b;
                        break;
                    }
                    cur += 1;
                }
                if bstart <= bend {
                    self.report.replace_range(bstart..bend, "");
                }
            }
        }
    }

    pub fn delete_current_line(&mut self) {
        if let Some(range) = &self.caret_char_range {
            let pos = range.start;
            let s = &self.report;
            let mut cur = 0usize;
            let mut line_start_byte = 0usize;
            let mut line_end_byte = s.len();
            for (b, ch) in s.char_indices() {
                if cur == pos {
                    let prefix = &s[..b];
                    if let Some(idx) = prefix.rfind('\n') {
                        line_start_byte = idx + 1;
                    } else {
                        line_start_byte = 0;
                    }
                    if let Some(rest) = s[b..].find('\n') {
                        line_end_byte = b + rest + 1;
                    } else {
                        line_end_byte = s.len();
                    }
                    break;
                }
                cur += 1;
            }
            if line_start_byte < line_end_byte {
                self.push_undo_snapshot();
                self.report.replace_range(line_start_byte..line_end_byte, "");
                self.caret_char_range = Some(line_start_byte..line_start_byte);
            }
        }
    }

    /// Insert a newline at the end-of-line insertion point (used by `o`):
    /// computes insertion position, inserts '\n', and leaves caret at start of new line.
    pub fn open_line_below(&mut self) {
        let (_s, e, _c) = self.move_to_line_bounds();
        let insert_pos = if e > 0 {
            let prev = self.report.chars().nth(e - 1);
            if prev == Some('\n') { e.saturating_sub(1) } else { e }
        } else { e };
        self.set_caret_pos(insert_pos);
        self.push_undo_snapshot();
        self.insert_at_caret("\n");
    }

    /// Insert a newline at the start-of-line insertion point (used by `O`):
    /// inserts above the current line and leaves caret at start of new line.
    pub fn open_line_above(&mut self) {
        let (s, _e, _c) = self.move_to_line_bounds();
        let insert_pos = s.min(self.char_len());
        // Insert a newline at the start of the current line, then move the caret
        // to the start of the newly inserted blank line (so its line number
        // matches the original line index).
        self.set_caret_pos(insert_pos);
        self.push_undo_snapshot();
        self.insert_at_caret("\n");
        // After insertion `insert_at_caret` places the caret after the inserted
        // text; move it back to the start of the new blank line so callers
        // observing line numbers see the expected value.
        self.set_caret_pos(insert_pos);
    }

    /// Move caret to the append (end-of-line) insertion point (used by `A`).
    pub fn append_at_end_of_line(&mut self) {
        let (_s, e, _c) = self.move_to_line_bounds();
        let target = if e > 0 {
            let prev = self.report.chars().nth(e - 1);
            if prev == Some('\n') { e.saturating_sub(1) } else { e }
        } else { e };
        self.set_caret_pos(target);
    }

    /// Shared Normal-mode key handler used by the app and tests.
    /// Returns `true` when the caller should request focus (i.e., entering Insert mode).
    pub fn handle_normal_key(
        buffer: &mut ReportBuffer,
        vim_mode: &mut crate::VimMode,
        last_vim_key: &mut Option<char>,
        last_vim_object: &mut Option<char>,
        last_vim_count: &mut Option<usize>,
        visual_anchor: &mut Option<usize>,
        ch: char,
    ) -> bool {
        // If the user typed digits as a count prefix, accumulate them and
        // wait for the next command. Special-case: a lone leading '0' with
        // no previous count is treated as the `0` motion (start of line),
        // not as a numeric prefix.
        if ch.is_ascii_digit() {
            if ch == '0' && last_vim_count.is_none() {
                // fall through to handle '0' as a motion
            } else {
                let d = ch.to_digit(10).unwrap() as usize;
                if let Some(prev) = last_vim_count.take() {
                    *last_vim_count = Some(prev * 10 + d);
                } else {
                    *last_vim_count = Some(d);
                }
                return false;
            }
        }
        match ch {
            'i' => {
                // If this follows an operator (e.g., 'd' or 'c') then treat
                // this as the start of operator+textobject (e.g., 'diw').
                if let Some(op) = last_vim_key.clone() {
                    if op == 'd' || op == 'c' {
                        *last_vim_object = Some('i');
                        return false;
                    }
                }
                *vim_mode = crate::VimMode::Insert;
                buffer.start_undo_group();
                *last_vim_key = None;
                *last_vim_count = None;
                true
            }
            'a' => {
                // operator + 'a' as text-object prefix
                if let Some(op) = last_vim_key.clone() {
                    if op == 'd' || op == 'c' {
                        *last_vim_object = Some('a');
                        return false;
                    }
                }
                buffer.move_caret_by(1);
                *vim_mode = crate::VimMode::Insert;
                buffer.start_undo_group();
                *last_vim_key = None;
                *last_vim_count = None;
                true
            }
            'A' => {
                buffer.append_at_end_of_line();
                *vim_mode = crate::VimMode::Insert;
                buffer.start_undo_group();
                *last_vim_key = None;
                *last_vim_count = None;
                true
            }
            'I' => {
                let (s, _e, _c) = buffer.move_to_line_bounds();
                buffer.set_caret_pos(s);
                *vim_mode = crate::VimMode::Insert;
                buffer.start_undo_group();
                *last_vim_key = None;
                *last_vim_count = None;
                true
            }
            'o' => {
                // If we're in Visual mode (char or line), `o` should move the caret to the
                // other end of the selection (reverse selection endpoints).
                if *vim_mode == crate::VimMode::Visual || *vim_mode == crate::VimMode::VisualLine {
                    if let Some(anchor_pos) = *visual_anchor {
                        let cur = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                        *visual_anchor = Some(cur);
                        buffer.set_caret_pos(anchor_pos);
                    }
                    *last_vim_key = None;
                    *last_vim_count = None;
                    false
                } else {
                    // Start grouping before creating the new line so subsequent typing
                    // becomes part of the same undo step (Vim-like behavior).
                    buffer.start_undo_group();
                    buffer.open_line_below();
                    *vim_mode = crate::VimMode::Insert;
                    *last_vim_key = None;
                    *last_vim_count = None;
                    true
                }
            }
            'O' => {
                buffer.start_undo_group();
                buffer.open_line_above();
                *vim_mode = crate::VimMode::Insert;
                *last_vim_key = None;
                *last_vim_count = None;
                true
            }
            'u' => { buffer.undo(); *last_vim_key = None; *last_vim_count = None; false }
            'v' => {
                // toggle Visual mode and set/clear anchor
                if *vim_mode == crate::VimMode::Visual {
                    *vim_mode = crate::VimMode::Normal;
                    *visual_anchor = None;
                } else {
                    let pos = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    *visual_anchor = Some(pos);
                    // initialize the caret range so UI shows selection immediately
                    buffer.caret_char_range = Some(pos..pos);
                    *vim_mode = crate::VimMode::Visual;
                }
                *last_vim_key = None;
                *last_vim_count = None;
                false
            }
            'V' => {
                // Visual Line mode: select whole lines
                if *vim_mode == crate::VimMode::VisualLine {
                    *vim_mode = crate::VimMode::Normal;
                    *visual_anchor = None;
                } else {
                    let pos = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    let (s, _e, _c) = buffer.move_to_line_bounds();
                    *visual_anchor = Some(s);
                    buffer.caret_char_range = Some(s..s);
                    *vim_mode = crate::VimMode::VisualLine;
                }
                *last_vim_key = None;
                *last_vim_count = None;
                false
            }
            
            'j' => {
                // operator-pending: d/j or c/j ranges
                if *last_vim_key == Some('d') {
                    let start = buffer.move_to_line_bounds().0;
                    let op_count = last_vim_count.take().unwrap_or(1);
                    // include current line + op_count motions
                    buffer.delete_lines_from(start, op_count + 0);
                    *last_vim_key = None;
                    false
                } else if *last_vim_key == Some('c') {
                    let start = buffer.move_to_line_bounds().0;
                    let op_count = last_vim_count.take().unwrap_or(1);
                    buffer.start_undo_group();
                    buffer.delete_lines_from(start, op_count + 0);
                    *last_vim_key = None;
                    *vim_mode = crate::VimMode::Insert;
                    true
                } else {
                    let repeats = last_vim_count.take().unwrap_or(1);
                    for _ in 0..repeats { buffer.move_line_down(); }
                    *last_vim_key = None; false
                }
            }
            'k' => {
                if *last_vim_key == Some('d') {
                    let end = buffer.move_to_line_bounds().0;
                    let op_count = last_vim_count.take().unwrap_or(1);
                    let mut start = end;
                    for _ in 0..op_count {
                        start = buffer.prev_line_start_from(start);
                    }
                    buffer.delete_range(start, end);
                    *last_vim_key = None;
                    false
                } else if *last_vim_key == Some('c') {
                    let end = buffer.move_to_line_bounds().0;
                    let op_count = last_vim_count.take().unwrap_or(1);
                    let mut start = end;
                    for _ in 0..op_count {
                        start = buffer.prev_line_start_from(start);
                    }
                    buffer.start_undo_group();
                    buffer.delete_range(start, end);
                    *last_vim_key = None;
                    *vim_mode = crate::VimMode::Insert;
                    true
                } else {
                    let repeats = last_vim_count.take().unwrap_or(1);
                    for _ in 0..repeats { buffer.move_line_up(); }
                    *last_vim_key = None; false
                }
            }
            'w' => {
                // text-object handling (i/a + w) takes precedence when present
                if last_vim_object.is_some() {
                    if let Some(obj) = last_vim_object.take() {
                        if obj == 'i' || obj == 'a' {
                            let cur = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                            if let Some((start_char, end_char)) = buffer.current_word_bounds(cur) {
                                let mut s = start_char;
                                let mut e = end_char;
                                if obj == 'a' {
                                    let chars: Vec<char> = buffer.report.chars().collect();
                                    if s > 0 && chars[s - 1].is_whitespace() { s -= 1; }
                                    if e < chars.len() && chars[e].is_whitespace() { e += 1; }
                                }
                                if *last_vim_key == Some('d') {
                                    buffer.delete_range(s, e);
                                    *last_vim_key = None;
                                    return false;
                                }
                                if *last_vim_key == Some('c') {
                                    buffer.start_undo_group();
                                    buffer.delete_range(s, e);
                                    *last_vim_key = None;
                                    *vim_mode = crate::VimMode::Insert;
                                    return true;
                                }
                            }
                        }
                    }
                }

                // operator 'd' (delete words)
                if *last_vim_key == Some('d') {
                    let start = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    let mut end = start;
                    let op_count = last_vim_count.take().unwrap_or(1);
                    for _ in 0..op_count {
                        end = buffer.next_word_start_from(end);
                    }
                    buffer.delete_range(start, end);
                    *last_vim_key = None;
                    return false;
                }

                // operator 'c' (change words)
                if *last_vim_key == Some('c') {
                    let start = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    let mut end = start;
                    let op_count = last_vim_count.take().unwrap_or(1);
                    for _ in 0..op_count {
                        end = buffer.next_word_start_from(end);
                    }
                    buffer.start_undo_group();
                    buffer.delete_range(start, end);
                    *last_vim_key = None;
                    *vim_mode = crate::VimMode::Insert;
                    return true;
                }

                // default: move by words
                let repeats = last_vim_count.take().unwrap_or(1);
                for _ in 0..repeats { buffer.move_word_forward(); }
                *last_vim_key = None; false
            }
            'b' => {
                if *last_vim_key == Some('d') {
                    let end = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    let mut start = end;
                    let op_count = last_vim_count.take().unwrap_or(1);
                    for _ in 0..op_count { start = buffer.prev_word_start_from(start); }
                    buffer.delete_range(start, end);
                    *last_vim_key = None;
                    false
                } else if *last_vim_key == Some('c') {
                    let end = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    let mut start = end;
                    let op_count = last_vim_count.take().unwrap_or(1);
                    for _ in 0..op_count { start = buffer.prev_word_start_from(start); }
                    buffer.start_undo_group();
                    buffer.delete_range(start, end);
                    *last_vim_key = None;
                    *vim_mode = crate::VimMode::Insert;
                    true
                } else {
                    let repeats = last_vim_count.take().unwrap_or(1);
                    for _ in 0..repeats { buffer.move_word_backward(); }
                    *last_vim_key = None; false
                }
            }
            'e' => {
                if *last_vim_key == Some('d') {
                    let start = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    let mut end_char = start;
                    let op_count = last_vim_count.take().unwrap_or(1);
                    for _ in 0..op_count { end_char = buffer.word_end_from(end_char).saturating_add(1); }
                    buffer.delete_range(start, end_char);
                    *last_vim_key = None;
                    false
                } else if *last_vim_key == Some('c') {
                    let start = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    let mut end_char = start;
                    let op_count = last_vim_count.take().unwrap_or(1);
                    for _ in 0..op_count { end_char = buffer.word_end_from(end_char).saturating_add(1); }
                    buffer.start_undo_group();
                    buffer.delete_range(start, end_char);
                    *last_vim_key = None;
                    *vim_mode = crate::VimMode::Insert;
                    true
                } else {
                    let repeats = last_vim_count.take().unwrap_or(1);
                    for _ in 0..repeats { buffer.move_word_end(); }
                    *last_vim_key = None; false
                }
            }
            // motions that can be used with operators (d, c)
            'h' | 'l' => {
                if *last_vim_key == Some('d') {
                    let cur = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    let op_count = last_vim_count.take().unwrap_or(1);
                    let (start, end) = if ch == 'h' {
                        (cur.saturating_sub(op_count), cur)
                    } else {
                        (cur, (cur + op_count).min(buffer.char_len()))
                    };
                    buffer.delete_range(start, end);
                    *last_vim_key = None;
                    false
                } else if *last_vim_key == Some('c') {
                    buffer.start_undo_group();
                    let cur = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    let op_count = last_vim_count.take().unwrap_or(1);
                    let (start, end) = if ch == 'h' {
                        (cur.saturating_sub(op_count), cur)
                    } else {
                        (cur, (cur + op_count).min(buffer.char_len()))
                    };
                    buffer.delete_range(start, end);
                    *last_vim_key = None;
                    *vim_mode = crate::VimMode::Insert;
                    true
                } else {
                    let repeats = last_vim_count.take().unwrap_or(1);
                    for _ in 0..repeats { if ch == 'h' { buffer.move_caret_by(-1); } else { buffer.move_caret_by(1); } }
                    *last_vim_key = None;
                    false
                }
            }
            'g' => {
                if *last_vim_key == Some('g') {
                    // 'gg' -> go to first line or to count if provided (Ngg)
                    let line = last_vim_count.take().unwrap_or(1);
                    buffer.goto_line_start(line);
                    *last_vim_key = None;
                    *last_vim_count = None;
                    false
                } else {
                    *last_vim_key = Some('g');
                    false
                }
            }
            'G' => {
                // operator-pending: dG/cG delete/change to specified line or EOF
                if *last_vim_key == Some('d') {
                    let start = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    if let Some(n) = last_vim_count.take() {
                        // delete to start of line n
                        let mut target = 0usize;
                        // goto_line_start uses 1-based lines
                        buffer.goto_line_start(n);
                        target = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                        buffer.delete_range(start, target);
                    } else {
                        // delete to EOF
                        let end = buffer.char_len();
                        buffer.delete_range(start, end);
                    }
                    *last_vim_key = None;
                    false
                } else if *last_vim_key == Some('c') {
                    let start = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                    if let Some(n) = last_vim_count.take() {
                        buffer.goto_line_start(n);
                        let target = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                        buffer.start_undo_group();
                        buffer.delete_range(start, target);
                    } else {
                        let end = buffer.char_len();
                        buffer.start_undo_group();
                        buffer.delete_range(start, end);
                    }
                    *last_vim_key = None;
                    *vim_mode = crate::VimMode::Insert;
                    true
                } else {
                    // 'G' goes to end of file, or to specified line if a count is present
                    if let Some(n) = last_vim_count.take() {
                        buffer.goto_line_start(n);
                    } else {
                        buffer.goto_end_of_file();
                    }
                    *last_vim_key = None;
                    *last_vim_count = None;
                    false
                }
            }
            '$' => {
                // end-of-line motion; can be used with d/c
                let start = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                let (_s, e, _c) = buffer.move_to_line_bounds();
                // compute end-of-line char index (exclude newline)
                let end_char = if e > 0 {
                    if buffer.report.chars().nth(e - 1) == Some('\n') { e.saturating_sub(1) } else { e }
                } else { e };
                if *last_vim_key == Some('d') {
                    buffer.delete_range(start, end_char);
                    *last_vim_key = None;
                    false
                } else if *last_vim_key == Some('c') {
                    buffer.start_undo_group();
                    buffer.delete_range(start, end_char);
                    *last_vim_key = None;
                    *vim_mode = crate::VimMode::Insert;
                    true
                } else {
                    buffer.set_caret_pos(end_char);
                    *last_vim_key = None;
                    false
                }
            }
            '0' => {
                // move to start of line; if operator pending, delete/change to start
                let cur = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                let (line_start, _e, _c) = buffer.move_to_line_bounds();
                if *last_vim_key == Some('d') {
                    buffer.delete_range(line_start, cur);
                    *last_vim_key = None;
                    false
                } else if *last_vim_key == Some('c') {
                    buffer.start_undo_group();
                    buffer.delete_range(line_start, cur);
                    *last_vim_key = None;
                    *vim_mode = crate::VimMode::Insert;
                    true
                } else {
                    buffer.set_caret_pos(line_start);
                    *last_vim_key = None;
                    false
                }
            }
            'D' => {
                // delete to end of line
                let start = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                let (_s, e, _c) = buffer.move_to_line_bounds();
                let end_char = if e > 0 { if buffer.report.chars().nth(e - 1) == Some('\n') { e.saturating_sub(1) } else { e } } else { e };
                buffer.delete_range(start, end_char);
                *last_vim_key = None;
                false
            }
            'C' => {
                // change to end of line (enter Insert)
                let start = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                let (_s, e, _c) = buffer.move_to_line_bounds();
                let end_char = if e > 0 { if buffer.report.chars().nth(e - 1) == Some('\n') { e.saturating_sub(1) } else { e } } else { e };
                buffer.start_undo_group();
                buffer.delete_range(start, end_char);
                *last_vim_key = None;
                *vim_mode = crate::VimMode::Insert;
                true
            }
            'x' => {
                // If in Visual mode (char or line), delete the selected range instead of a single char
                if *vim_mode == crate::VimMode::Visual || *vim_mode == crate::VimMode::VisualLine {
                    if let Some(anchor) = visual_anchor.take() {
                        let cur = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                        if *vim_mode == crate::VimMode::VisualLine {
                            let (as_, ae) = buffer.line_bounds_at(anchor);
                            let (cs, ce) = buffer.line_bounds_at(cur);
                            let s = as_.min(cs);
                            let e = ae.max(ce).min(buffer.char_len());
                            buffer.delete_range(s, e);
                        } else {
                            let s = anchor.min(cur);
                            let e = anchor.max(cur).saturating_add(1).min(buffer.char_len());
                            buffer.delete_range(s, e);
                        }
                        *last_vim_key = None;
                        *vim_mode = crate::VimMode::Normal;
                        return false;
                    }
                }
                buffer.delete_char_at_cursor();
                *last_vim_key = None;
                false
            }
            'd' => {
                // If we're in Visual mode (char or line), "d" should delete the visual selection.
                if *vim_mode == crate::VimMode::Visual || *vim_mode == crate::VimMode::VisualLine {
                    if let Some(anchor) = visual_anchor.take() {
                        let cur = buffer.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
                        if *vim_mode == crate::VimMode::VisualLine {
                            let (as_, ae) = buffer.line_bounds_at(anchor);
                            let (cs, ce) = buffer.line_bounds_at(cur);
                            let s = as_.min(cs);
                            let e = ae.max(ce).min(buffer.char_len());
                            buffer.delete_range(s, e);
                        } else {
                            let s = anchor.min(cur);
                            // compute canonical end-exclusive end
                            let e = anchor.max(cur).min(buffer.char_len());
                            buffer.delete_range(s, e);
                        }
                        *last_vim_key = None;
                        *vim_mode = crate::VimMode::Normal;
                        return false;
                    }
                }

                if *last_vim_key == Some('d') {
                    // 'dd' or 'Ndd' deletes whole lines. Honor count if present.
                    let count = last_vim_count.take().unwrap_or(1);
                    let start = buffer.move_to_line_bounds().0;
                    buffer.delete_lines_from(start, count);
                    *last_vim_key = None;
                } else {
                    *last_vim_key = Some('d');
                }
                false
            }
            'c' => {
                // operator-pending: wait for next motion (handled in motion arms)
                *last_vim_key = Some('c');
                false
            }
            _ => { *last_vim_key = None; false }
        }
    }
    pub fn get_caret_line_number(&self) -> usize {
        let chars: Vec<char> = self.report.chars().collect();
        let pos = self.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
        let mut line_num = 0usize;
        for i in 0..pos.min(chars.len()) {
            if chars[i] == '\n' {
                line_num += 1;
            }
        }
        line_num
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_word_end_from_middle_of_word() {
        let mut b = ReportBuffer::new();
        b.report = "hello world".to_string();
        // place caret after first char (inside 'hello')
        b.caret_char_range = Some(1..1);
        b.move_word_end();
        let pos = b.caret_char_range.as_ref().unwrap().start;
        // expect to land on the 'o' of 'hello' (index 4)
        assert_eq!(pos, 4);
    }

    #[test]
    fn move_word_end_from_end_of_word_advances_to_next_word_end() {
        let mut b = ReportBuffer::new();
        b.report = "hello world".to_string();
        // place caret at end of 'hello' (index 5, after 'o')
        b.caret_char_range = Some(5..5);
        b.move_word_end();
        let pos = b.caret_char_range.as_ref().unwrap().start;
        // expect to land on 'd' of 'world' (index 10)
        assert_eq!(pos, 10);
    }

    #[test]
    fn move_word_end_with_punctuation() {
        let mut b = ReportBuffer::new();
        b.report = "abc, def.".to_string();
        // caret after 'c' (index 3) -- on punctuation, `e` should move to next word end
        b.caret_char_range = Some(3..3);
        b.move_word_end();
        let pos = b.caret_char_range.unwrap().start;
        // Vim moves to the end of the next word (index 7)
        assert_eq!(pos, 7);

        // now move from just after comma (index 4) to end of next word
        b.caret_char_range = Some(4..4);
        b.move_word_end();
        let pos2 = b.caret_char_range.as_ref().unwrap().start;
        // expect to land on 'f' (index 7)
        assert_eq!(pos2, 7);
    }

    #[test]
    fn move_word_end_multiple_times() {
        let mut b = ReportBuffer::new();
        b.report = "one two three".to_string();
        // start at beginning
        b.caret_char_range = Some(0..0);

        // first e: end of 'one' -> index 2
        b.move_word_end();
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, 2);

        // second e: end of 'two' -> index 6
        b.move_word_end();
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, 6);

        // third e: end of 'three' -> index 12
        b.move_word_end();
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, 12);
    }

    #[test]
    fn move_word_forward_start_of_next_word() {
        let mut b = ReportBuffer::new();
        b.report = "one two three".to_string();
        // start at beginning
        b.caret_char_range = Some(0..0);

        // first w: start of 'two' -> index 4
        b.move_word_forward();
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, 4);

        // second w: start of 'three' -> index 8
        b.move_word_forward();
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, 8);
    }

    #[test]
    fn vim_insert_commands_append_a() {
        let mut b = ReportBuffer::new();
        b.report = "one two".to_string();
        b.caret_char_range = Some(0..0);

        // 'a' should move caret one character right (append)
        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'a');
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, 1);
    }

    #[test]
    fn vim_insert_commands_append_A() {
        let mut b = ReportBuffer::new();
        b.report = "first line\nsecond".to_string();
        // place caret in the first line
        b.caret_char_range = Some(2..2);
        // 'A' should move caret to the insertion point at end of current line
        let (_s, e, _c) = b.move_to_line_bounds();
        let expected = if e > 0 {
            let prev = b.report.chars().nth(e - 1);
            if prev == Some('\n') { e.saturating_sub(1) } else { e }
        } else { e };
        b.append_at_end_of_line();
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, expected);
    }

    #[test]
    fn vim_insert_commands_insert_I() {
        let mut b = ReportBuffer::new();
        b.report = "  indented\nline".to_string();
        // place caret somewhere on the indented line
        b.caret_char_range = Some(3..3);
        let (s, _e, _c) = b.move_to_line_bounds();

        // 'I' should move caret to the start of the line
        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'I');
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, s);
    }

    #[test]
    fn vim_insert_commands_open_o() {
        let mut b = ReportBuffer::new();
        b.report = "one\ntwo".to_string();
        // place caret on first line
        b.caret_char_range = Some(1..1);
        // 'o' should insert a newline after the end-of-line insertion point
        let (_, e, _) = b.move_to_line_bounds();
        let insert_pos = if e > 0 {
            let prev = b.report.chars().nth(e - 1);
            if prev == Some('\n') { e.saturating_sub(1) } else { e }
        } else { e };
        // Use the shared Normal-mode handler so tests match runtime behavior
        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'o');
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, insert_pos + 1);
    }

    #[test]
    fn vim_insert_commands_open_O() {
        let mut b = ReportBuffer::new();
        b.report = "one\ntwo".to_string();
        // place caret on second line
        b.caret_char_range = Some(4..4);
        let (s, _e, _c) = b.move_to_line_bounds();
        // 'O' should move to start of line, insert newline above, and caret should be at start of new line (s+1)
        let start_line = b.get_caret_line_number();
        b.open_line_above();
        let new_line = b.get_caret_line_number();
        assert_eq!(new_line, start_line); // still on the same line number because we inserted above
        // caret should be at the start of the newly inserted blank line
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, s);
    }

    #[test]
    fn grouped_undo_for_o_then_type() {
        let mut b = ReportBuffer::new();
        b.report = "first\nsecond".to_string();
        // place caret on first line
        b.caret_char_range = Some(2..2);

        // use normal handler to perform 'o' which should start an undo group
        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        let prev = b.report.clone();
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'o');

        // simulate typing in Insert mode (TextEdit would normally do this)
        b.insert_at_caret("hello");

        // end the undo group (simulate pressing Escape)
        b.end_undo_group();

        // take the post-change snapshot for redo verification
        let after = b.report.clone();

        // a single undo should revert both the inserted newline and typed text
        b.undo();
        assert_eq!(b.report, prev);

        // redo should restore the grouped change
        b.redo();
        assert_eq!(b.report, after);
    }

    #[test]
    fn undo_redo_dw() {
        let mut b = ReportBuffer::new();
        b.report = "one two three".to_string();
        b.caret_char_range = Some(4..4);

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        let prev = b.report.clone();
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'd');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'w');

        assert_eq!(b.report, "one three");

        b.undo();
        assert_eq!(b.report, prev);

        b.redo();
        assert_eq!(b.report, "one three");
    }

    #[test]
    fn undo_redo_cw_grouped() {
        let mut b = ReportBuffer::new();
        b.report = "one two three".to_string();
        b.caret_char_range = Some(4..4);

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        let prev = b.report.clone();

        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'c');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'w');

        // now in Insert mode; simulate typing
        b.insert_at_caret("X");
        b.end_undo_group();

        let after = b.report.clone();

        b.undo();
        assert_eq!(b.report, prev);

        b.redo();
        assert_eq!(b.report, after);
    }

    #[test]
    fn undo_redo_dd() {
        let mut b = ReportBuffer::new();
        b.report = "a\nb\nc".to_string();
        // place caret at start of second line ('b')
        b.caret_char_range = Some(2..2);

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        let prev = b.report.clone();

        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'd');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'd');

        assert_eq!(b.report, "a\nc");

        b.undo();
        assert_eq!(b.report, prev);

        b.redo();
        assert_eq!(b.report, "a\nc");
    }

    #[test]
    fn numeric_prefix_h_moves_left() {
        let mut b = ReportBuffer::new();
        b.report = "abcdefgh".to_string();
        b.caret_char_range = Some(5..5); // at 'f'

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        // '3h' should move left 3 chars
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, '3');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'h');
        assert_eq!(b.caret_char_range.as_ref().unwrap().start, 2);
    }

    #[test]
    fn numeric_prefix_3dw() {
        let mut b = ReportBuffer::new();
        b.report = "one two three four five".to_string();
        b.caret_char_range = Some(4..4); // start of 'two'

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, '3');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'd');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'w');

        assert_eq!(b.report, "one five");
    }

    #[test]
    fn numeric_prefix_2cw_grouped() {
        let mut b = ReportBuffer::new();
        b.report = "one two three".to_string();
        b.caret_char_range = Some(4..4); // start of 'two'

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        let prev = b.report.clone();

        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, '2');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'c');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'w');

        // in Insert mode now; simulate typing
        b.insert_at_caret("X");
        b.end_undo_group();

        let after = b.report.clone();
        b.undo();
        assert_eq!(b.report, prev);
        b.redo();
        assert_eq!(b.report, after);
    }

    #[test]
    fn numeric_prefix_3dd() {
        let mut b = ReportBuffer::new();
        b.report = "a\nb\nc\nd\ne\nf".to_string();
        // place caret at start of second line ('b')
        // 'a' (0), '\n'(1), 'b' (2)
        b.caret_char_range = Some(2..2);

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;

        let mut anchor: Option<usize> = None;
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, '3');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'd');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'd');

        // expect lines b,c,d removed -> remaining a\ne\nf
        assert_eq!(b.report, "a\ne\nf");
    }

    #[test]
    fn undo_redo_dollar_and_c_dollar() {
        let mut b = ReportBuffer::new();
        b.report = "hello world\nnext line".to_string();
        // place caret at 'w' of world (index 6)
        b.caret_char_range = Some(6..6);

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let prev = b.report.clone();

        // d$ should delete to EOL
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'd');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, '$');
        assert_eq!(b.report, "hello \nnext line");

        b.undo();
        assert_eq!(b.report, prev);

        // now test C (change to end of line) grouped
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'C');
        // simulate typing
        b.insert_at_caret("X");
        b.end_undo_group();
        let after = b.report.clone();
        b.undo();
        assert_eq!(b.report, prev);
        b.redo();
        assert_eq!(b.report, after);
    }

    #[test]
    fn diw_deletes_inner_word_only() {
        let mut b = ReportBuffer::new();
        b.report = "one  two  three".to_string();
        // place caret at start of 'two' (index 5)
        b.caret_char_range = Some(5..5);

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;

        // perform 'd' 'i' 'w'
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'd');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'i');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'w');

        // expect only the word 'two' removed, surrounding spaces preserved
        assert_eq!(b.report, "one    three");
    }

    #[test]
    fn daw_deletes_word_and_surrounding_spaces() {
        let mut b = ReportBuffer::new();
        b.report = "one  two  three".to_string();
        // place caret at start of 'two' (index 5)
        b.caret_char_range = Some(5..5);

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;

        // perform 'd' 'a' 'w'
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'd');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'a');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'w');

        // expect the word and adjacent whitespace to be removed (current behavior preserves one space on each side)
        assert_eq!(b.report, "one  three");
    }

    #[test]
    fn visual_d_deletes_selection_and_exits_visual() {
        let mut b = ReportBuffer::new();
        b.report = "abcdef".to_string();
        // place caret at index 1 (on 'b')
        b.caret_char_range = Some(1..1);

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;

        // enter visual mode
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'v');
        assert_eq!(mode, crate::VimMode::Visual);

        // move right twice (two 'l' motions)
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'l');
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'l');

        // now delete selection with 'd' (should remove indices 1..3 -> 'b','c')
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, &mut obj, &mut count, &mut anchor, 'd');

        assert_eq!(b.report, "adef");
        assert_eq!(mode, crate::VimMode::Normal);
    }
}
