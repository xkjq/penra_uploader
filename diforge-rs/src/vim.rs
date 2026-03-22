use serde::{Deserialize, Serialize};
use std::ops::Range;

#[derive(PartialEq, Eq, Serialize, Deserialize, Clone, Copy, Debug)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
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
        ch: char,
    ) -> bool {
        match ch {
            'i' => {
                *vim_mode = crate::VimMode::Insert;
                buffer.start_undo_group();
                *last_vim_key = None;
                true
            }
            'a' => {
                buffer.move_caret_by(1);
                *vim_mode = crate::VimMode::Insert;
                buffer.start_undo_group();
                *last_vim_key = None;
                true
            }
            'A' => {
                buffer.append_at_end_of_line();
                *vim_mode = crate::VimMode::Insert;
                buffer.start_undo_group();
                *last_vim_key = None;
                true
            }
            'I' => {
                let (s, _e, _c) = buffer.move_to_line_bounds();
                buffer.set_caret_pos(s);
                *vim_mode = crate::VimMode::Insert;
                buffer.start_undo_group();
                *last_vim_key = None;
                true
            }
            'o' => {
                buffer.open_line_below();
                *vim_mode = crate::VimMode::Insert;
                buffer.start_undo_group();
                *last_vim_key = None;
                true
            }
            'O' => {
                buffer.open_line_above();
                *vim_mode = crate::VimMode::Insert;
                buffer.start_undo_group();
                *last_vim_key = None;
                true
            }
            'u' => { buffer.undo(); *last_vim_key = None; false }
            'h' => { buffer.move_caret_by(-1); *last_vim_key = None; false }
            'l' => { buffer.move_caret_by(1); *last_vim_key = None; false }
            'j' => { buffer.move_line_down(); *last_vim_key = None; false }
            'k' => { buffer.move_line_up(); *last_vim_key = None; false }
            'w' => { buffer.move_word_forward(); *last_vim_key = None; false }
            'b' => { buffer.move_word_backward(); *last_vim_key = None; false }
            'e' => { buffer.move_word_end(); *last_vim_key = None; false }
            'x' => { buffer.delete_char_at_cursor(); *last_vim_key = None; false }
            'd' => {
                if *last_vim_key == Some('d') {
                    buffer.delete_current_line();
                    *last_vim_key = None;
                } else {
                    *last_vim_key = Some('d');
                }
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
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, 'a');
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
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, 'I');
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
        ReportBuffer::handle_normal_key(&mut b, &mut mode, &mut last, 'o');
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
}
