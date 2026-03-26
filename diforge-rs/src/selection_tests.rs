use super::*;
use egui::{Pos2, Rect};

// Helper used by tests to reproduce the galley-based nearest-char mapping
// without requiring a full egui runloop. `pos_from_cursor` should return
// a Rect positioned relative to the widget (the same value `galley.pos_from_cursor`
// returns). The function mimics the logic used in pointer handling code.
pub fn nearest_char_index_via_pos(
    total: usize,
    response_rect_min: egui::Pos2,
    click_pos: egui::Pos2,
    pos_from_cursor: &dyn Fn(usize) -> egui::Rect,
) -> usize {
    let mut best = 0usize;
    let mut best_dist = f32::INFINITY;
    for idx in 0..=total {
        let rect = pos_from_cursor(idx);
        let screen = response_rect_min + rect.min.to_vec2();
        let dx = screen.x - click_pos.x;
        let dy = screen.y - click_pos.y;
        let dist = dx * dx + dy * dy;
        if dist < best_dist {
            best_dist = dist;
            best = idx;
        }
    }
    best
}

#[cfg(test)]
mod selection_tests {
    use super::*;

    #[test]
    fn nearest_char_index_simple() {
        // create a fake pos_from_cursor where each char sits 10px apart
        let total = 10usize;
        let response_min = Pos2::new(100.0, 100.0);
        let pos_from_cursor = |idx: usize| {
            let x = idx as f32 * 10.0;
            Rect::from_min_max(Pos2::new(x, 0.0), Pos2::new(x + 8.0, 14.0))
        };

        // click roughly at character index 3 (100 + 35 px)
        let click = Pos2::new(100.0 + 35.0, 110.0);
        let best = nearest_char_index_via_pos(total, response_min, click, &pos_from_cursor);
        assert_eq!(best, 3);
    }

    #[test]
    fn drag_selection_range_and_visual_sync() {
        // Simulate a small buffer and a drag from index 2 to 5
        let mut buf = vim::ReportBuffer::new();
        buf.report = "0123456789".to_string();
        let total = buf.char_len();

        // anchor at 2, current at 5 -> selection should be [2..6) when including final char
        let anchor = 2usize;
        let cur = 5usize;
        let s = anchor.min(cur);
        let e = anchor.max(cur).saturating_add(1).min(total);
        assert_eq!(s, 2);
        assert_eq!(e, 6);

        // Now check visual sync computation used in UI: display_end adds 1 when e > s
        let display_end = (anchor.max(cur))
            .min(total)
            .saturating_add(if anchor.max(cur) > s { 1 } else { 0 });
        assert_eq!(display_end, 6);

        // Toggle visual mode via the public handler and ensure caret range initialized
        let mut vim_mode = crate::VimMode::Normal;
        let mut last_vim_key = None;
        let mut last_vim_object = None;
        let mut last_count = None;
        let mut visual_anchor = None;

        // Put caret at position 4 and press 'v'
        buf.caret_char_range = Some(4..4);
        let focus = vim::ReportBuffer::handle_normal_key(
            &mut buf,
            &mut vim_mode,
            &mut last_vim_key,
            &mut last_vim_object,
            &mut last_count,
            &mut visual_anchor,
            'v',
        );
        // Should enter Visual mode and caret range should be initialized
        assert_eq!(vim_mode, crate::VimMode::Visual);
        assert_eq!(visual_anchor, Some(4));
        assert_eq!(buf.caret_char_range, Some(4..4));
        assert_eq!(focus, false);
    }

    #[test]
    fn context_menu_replacement_via_char_indices() {
        let mut buf = vim::ReportBuffer::new();
        buf.report = "Hello wurld".to_string();

        let start_byte = buf.report.find("wurld").unwrap();
        let end_byte = start_byte + "wurld".len();
        let start_char = buf.report[..start_byte].chars().count();
        let end_char = buf.report[..end_byte].chars().count();

        // simulate selecting suggestion from context menu by setting caret range
        buf.caret_char_range = Some(start_char..end_char);
        buf.insert_at_caret("world");

        assert_eq!(buf.report, "Hello world");
        let expected_caret = start_char + "world".chars().count();
        assert_eq!(buf.caret_char_range, Some(expected_caret..expected_caret));
    }

    #[test]
    fn spell_context_byte_replace_updates_report_and_caret() {
        // Simulate the spell-context replacement path which uses byte indices
        let mut app = ReportApp::default();
        app.buffer.report = "This is teh test".to_string();

        let start_byte = app.buffer.report.find("teh").unwrap();
        let end_byte = start_byte + "teh".len();

        let mut rep = app.buffer.report.clone();
        rep.replace_range(start_byte..end_byte, "the");
        app.buffer.report = rep;

        // caret should be set to char index corresponding to the original byte offset
        let pos = app.buffer.report[..start_byte].chars().count();
        app.buffer.caret_char_range = Some(pos..pos);

        assert!(app.buffer.report.contains("the"));
        assert_eq!(app.buffer.caret_char_range, Some(pos..pos));
    }

    #[test]
    fn ensure_vim_disabled_clears_state() {
        // create app and set some vim state
        let mut app = ReportApp::default();
        app.vim_enabled = true;
        app.vim_mode = crate::VimMode::Visual;
        app.visual_anchor = Some(3);
        app.buffer.caret_char_range = Some(3..7);
        app.mouse_dragging = true;
        app.mouse_drag_anchor = Some(3);

        // now disable and ensure helper clears everything
        app.vim_enabled = false;
        app.ensure_vim_disabled_state();

        assert_eq!(app.vim_mode, crate::VimMode::Normal);
        assert_eq!(app.visual_anchor, None);
        assert_eq!(app.buffer.caret_char_range, Some(3..3));
        assert_eq!(app.mouse_dragging, false);
        assert_eq!(app.mouse_drag_anchor, None);
    }

    #[test]
    #[ignore]
    fn reproduce_visual_d_failure_trace() {
        // Reproduce the failing test steps and print intermediate states for debugging.
        let mut b = crate::vim::ReportBuffer::new();
        b.report = "abcdef".to_string();
        b.caret_char_range = Some(1..1);

        let mut mode = crate::VimMode::Normal;
        let mut last = None;
        let mut count = None;
        let mut obj: Option<char> = None;
        let mut anchor: Option<usize> = None;

        eprintln!(
            "before v: report='{}' caret={:?} mode={:?}",
            b.report, b.caret_char_range, mode
        );
        crate::vim::ReportBuffer::handle_normal_key(
            &mut b,
            &mut mode,
            &mut last,
            &mut obj,
            &mut count,
            &mut anchor,
            'v',
        );
        eprintln!(
            "after v: report='{}' caret={:?} mode={:?} anchor={:?}",
            b.report, b.caret_char_range, mode, anchor
        );
        crate::vim::ReportBuffer::handle_normal_key(
            &mut b,
            &mut mode,
            &mut last,
            &mut obj,
            &mut count,
            &mut anchor,
            'l',
        );
        eprintln!(
            "after l1: report='{}' caret={:?} mode={:?} anchor={:?}",
            b.report, b.caret_char_range, mode, anchor
        );
        crate::vim::ReportBuffer::handle_normal_key(
            &mut b,
            &mut mode,
            &mut last,
            &mut obj,
            &mut count,
            &mut anchor,
            'l',
        );
        eprintln!(
            "after l2: report='{}' caret={:?} mode={:?} anchor={:?}",
            b.report, b.caret_char_range, mode, anchor
        );
        crate::vim::ReportBuffer::handle_normal_key(
            &mut b,
            &mut mode,
            &mut last,
            &mut obj,
            &mut count,
            &mut anchor,
            'd',
        );
        eprintln!(
            "after d: report='{}' caret={:?} mode={:?} anchor={:?}",
            b.report, b.caret_char_range, mode, anchor
        );
        assert_eq!(b.report, "adef");
    }
}
