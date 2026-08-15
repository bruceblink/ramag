use std::ops::Range;

use gpui::{Bounds, Context, EntityInputHandler, Pixels, Point, UTF16Selection, Window};
use tracing::warn;

use super::{LINE_HEIGHT, TerminalView};

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = utf16_to_utf8_range(&self.marked_text, range_utf16);
        adjusted_range.replace(utf8_to_utf16_range(&self.marked_text, range.clone()));
        self.marked_text.get(range).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.marked_selection_utf16.clone(),
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        (!self.marked_text.is_empty()).then(|| 0..self.marked_text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let bytes = self.marked_text.len();
        if bytes > 0
            && let Err(error) = self.core.send(self.marked_text.clone().into_bytes())
        {
            warn!(
                operation = "terminal_marked_text_commit",
                bytes,
                error = %error,
                "commit terminal marked text failed"
            );
        }
        self.marked_text.clear();
        self.marked_selection_utf16 = 0..0;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bytes = text.len();
        if bytes > 0
            && let Err(error) = self.core.send(text.as_bytes().to_vec())
        {
            warn!(
                operation = "terminal_text_input_commit",
                bytes,
                error = %error,
                "commit terminal text input failed"
            );
        }
        self.marked_text.clear();
        self.marked_selection_utf16 = 0..0;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range_utf16: Option<Range<usize>>,
        text: &str,
        selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = text.to_string();
        let length = text.encode_utf16().count();
        self.marked_selection_utf16 = selected_range_utf16
            .map(|range| range.start.min(length)..range.end.min(length))
            .unwrap_or(length..length);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let snapshot = self.core.snapshot();
        let cursor = snapshot.cursor?;
        Some(Bounds::new(
            Point {
                x: element_bounds.left() + self.cell_width * cursor.column as f32,
                y: element_bounds.top() + LINE_HEIGHT * cursor.row as f32,
            },
            gpui::Size {
                width: self.cell_width,
                height: LINE_HEIGHT,
            },
        ))
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(0)
    }
}

fn utf16_to_utf8_range(text: &str, range: Range<usize>) -> Range<usize> {
    let index = |target: usize| {
        let mut utf16 = 0usize;
        for (byte, character) in text.char_indices() {
            if utf16 >= target {
                return byte;
            }
            utf16 += character.len_utf16();
        }
        text.len()
    };
    index(range.start)..index(range.end.max(range.start))
}

fn utf8_to_utf16_range(text: &str, range: Range<usize>) -> Range<usize> {
    let count = |end: usize| {
        let mut end = end.min(text.len());
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        text[..end].encode_utf16().count()
    };
    count(range.start)..count(range.end.max(range.start))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_ranges_convert_without_splitting_unicode() {
        let text = "a中😀";
        assert_eq!(utf16_to_utf8_range(text, 1..2), 1..4);
        assert_eq!(utf8_to_utf16_range(text, 1..4), 1..2);
        assert_eq!(utf16_to_utf8_range(text, 2..4), 4..8);
    }
}
