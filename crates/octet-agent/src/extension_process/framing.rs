//! Bounded LF framing shared by the process reader and its offline fuzz target.

/// Return true at a delimiter; the caller decodes and clears the retained line.
/// The negotiated limit includes the LF, so a payload must be strictly smaller.
pub(super) fn push_protocol_byte(
    line: &mut Vec<u8>,
    byte: u8,
    max_message_bytes: usize,
) -> Result<bool, String> {
    if byte == b'\n' {
        return Ok(true);
    }
    line.push(byte);
    if line.len() >= max_message_bytes {
        return Err(format!("stdout message exceeded {max_message_bytes} bytes"));
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_limit_includes_the_delimiter() {
        let mut line = Vec::new();
        for byte in b"{}\r" {
            assert!(!push_protocol_byte(&mut line, *byte, 4).unwrap());
        }
        assert!(push_protocol_byte(&mut line, b'\n', 4).unwrap());
        assert_eq!(line, b"{}\r");
        line.clear();
        for byte in b"123" {
            assert!(!push_protocol_byte(&mut line, *byte, 4).unwrap());
        }
        assert!(push_protocol_byte(&mut line, b'4', 4).is_err());
        assert_eq!(line.len(), 4);
    }
}
