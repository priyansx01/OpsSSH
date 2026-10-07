use std::ops::Range;

/// Editable IME preedit buffer. Native ranges are UTF-16, while Rust strings use UTF-8.
#[derive(Debug, Default)]
pub(crate) struct Composition {
    pub text: String,
    pub selected: Range<usize>,
}

impl Composition {
    pub fn clear(&mut self) {
        self.text.clear();
        self.selected = 0..0;
    }

    pub fn range(&self, requested: Range<usize>) -> (Range<usize>, Range<usize>) {
        let mut offset = 0;
        let mut start = self.text.len();
        let mut end = self.text.len();
        let mut actual_start = self.text.encode_utf16().count();
        let mut actual_end = actual_start;
        for (byte, ch) in self.text.char_indices() {
            if offset >= requested.start && start == self.text.len() {
                start = byte;
                actual_start = offset;
            }
            if offset >= requested.end.max(requested.start) {
                end = byte;
                actual_end = offset;
                break;
            }
            offset += ch.len_utf16();
        }
        (
            start.min(end)..end,
            actual_start.min(actual_end)..actual_end,
        )
    }

    pub fn replace(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) {
        let range = range.unwrap_or(0..self.text.encode_utf16().count());
        let (bytes, adjusted) = self.range(range);
        self.text.replace_range(bytes, text);
        let caret = adjusted.start + text.encode_utf16().count();
        let selected = selected
            .map(|range| adjusted.start + range.start..adjusted.start + range.end)
            .unwrap_or(caret..caret);
        self.selected = self.range(selected).1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_utf16_edits_preserve_surrogate_pairs_and_selection() {
        let mut composition = Composition::default();
        composition.replace(None, "a😀界", Some(1..3));
        assert_eq!(composition.selected, 1..3);
        composition.replace(Some(1..3), "文", Some(0..1));
        assert_eq!(composition.text, "a文界");
        assert_eq!(composition.selected, 1..2);
        let (bytes, adjusted) = composition.range(500..700);
        assert_eq!(&composition.text[bytes], "");
        assert_eq!(adjusted, 3..3);
        composition.clear();
        assert!(composition.text.is_empty());
        assert_eq!(composition.selected, 0..0);
    }

    #[test]
    fn range_inside_surrogate_is_adjusted_without_panicking() {
        let mut composition = Composition::default();
        composition.replace(None, "😀x", None);
        let (bytes, adjusted) = composition.range(1..2);
        assert_eq!(&composition.text[bytes], "");
        assert_eq!(adjusted, 2..2);
    }
}
