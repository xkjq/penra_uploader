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
}

impl ReportBuffer {
    pub fn new() -> Self {
        Self {
            report: String::new(),
            caret_char_range: None,
        }
    }

    pub fn insert_at_caret(&mut self, insert: &str) {
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
        while pos < n && !chars[pos].is_alphanumeric() {
            pos += 1;
        }
        while pos < n && chars[pos].is_alphanumeric() {
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
        let pos = self.caret_char_range.as_ref().map(|r| r.start).unwrap_or(0);
        let n = chars.len();
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
                self.report.replace_range(line_start_byte..line_end_byte, "");
                self.caret_char_range = Some(line_start_byte..line_start_byte);
            }
        }
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
        let pos = b.caret_char_range.unwrap().start;
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
        let pos = b.caret_char_range.unwrap().start;
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
        let pos2 = b.caret_char_range.unwrap().start;
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
}
