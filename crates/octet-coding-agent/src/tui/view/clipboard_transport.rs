//! Bounded OSC 52 transport. The full semantic copy remains in the shell and
//! native clipboard; remote terminals receive only a UTF-8-safe prefix.
use std::io::{self, Write};

use base64::{engine::general_purpose::STANDARD, Engine};

const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_SOURCE_BYTES: usize = MAX_PAYLOAD_BYTES / 4 * 3;

pub(super) fn write_osc52(out: &mut impl Write, text: &str) -> io::Result<()> {
    let mut end = text.len().min(MAX_SOURCE_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let payload = STANDARD.encode(&text[..end]);
    // Encode once, after bounding the source, not once per excess scalar.
    // BEL avoids a printable ST suffix in terminals that decline OSC 52.
    out.write_all(format!("\x1b]52;c;{payload}\x07").as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_transport_emits_exact_bel_terminated_bytes() {
        let mut out = Vec::new();
        write_osc52(&mut out, "hello 界").unwrap();
        assert_eq!(out, b"\x1b]52;c;aGVsbG8g55WM\x07");
    }

    #[test]
    fn clipboard_transport_bounds_large_unicode_before_encoding() {
        for prefix in 0..4 {
            let text = format!("{}{}", "a".repeat(prefix), "🦀".repeat(1024 * 1024));
            let mut out = Vec::new();
            write_osc52(&mut out, &text).unwrap();
            assert!(out.starts_with(b"\x1b]52;c;"));
            assert!(out.ends_with(b"\x07"));
            let payload = &out[7..out.len() - 1];
            assert!(payload.len() <= MAX_PAYLOAD_BYTES);
            let decoded = String::from_utf8(STANDARD.decode(payload).unwrap()).unwrap();
            assert!(text.starts_with(&decoded));
            assert!((MAX_SOURCE_BYTES - 3..=MAX_SOURCE_BYTES).contains(&decoded.len()));
        }
    }
}
