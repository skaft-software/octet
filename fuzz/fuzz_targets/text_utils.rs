#![no_main]

use libfuzzer_sys::fuzz_target;
use sexy_tui_rs::utils::{
    strip_terminal_sequences, terminal_tokens, truncate_to_width, visible_width,
    wrap_text_with_ansi, TerminalToken,
};
use unicode_segmentation::UnicodeSegmentation;

fuzz_target!(|data: &[u8]| {
    if data.len() > 4096 {
        return;
    }
    let text = String::from_utf8_lossy(data);
    let width = usize::from(data.first().copied().unwrap_or(0)) % 129;
    let stripped = strip_terminal_sequences(&text);
    assert_eq!(strip_terminal_sequences(&stripped), stripped);
    let _ = visible_width(&text);
    for ellipsis in [None, Some(""), Some("…"), Some("👩‍💻")] {
        let clipped = truncate_to_width(&text, width, ellipsis);
        assert!(
            visible_width(&clipped) <= width,
            "{text:?} => {clipped:?}, width={width}"
        );
    }
    let _ = wrap_text_with_ansi(&text, width);
    // Check clipping boundaries independently on literal text: generated ANSI
    // resets must not cause an otherwise partial grapheme to look complete.
    let literal: String = terminal_tokens(&text)
        .into_iter()
        .filter_map(|token| match token {
            TerminalToken::Text(value) => Some(value),
            _ => None,
        })
        .collect();
    let clipped = strip_terminal_sequences(&truncate_to_width(&literal, width, Some("")));
    if !literal.contains('\x1b') && !literal.contains('\u{009b}') {
        assert!(literal.starts_with(&clipped));
        assert!(
            clipped.len() == literal.len()
                || literal
                    .grapheme_indices(true)
                    .any(|(offset, _)| offset == clipped.len())
        );
    }
});
