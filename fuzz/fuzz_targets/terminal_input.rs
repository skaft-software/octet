#![no_main]

use libfuzzer_sys::fuzz_target;

// Compile the actual stdin decoder without linking the CLI, opening a terminal,
// or exposing a fuzz-only public SDK. Its parser and frame ceiling stay shared.
#[allow(dead_code)]
#[path = "../../crates/octet-coding-agent/src/tui/terminal_input/codec.rs"]
mod codec;

fuzz_target!(|data: &[u8]| {
    if data.len() > 4096 {
        return;
    }
    for chunk_size in [1, 7, 64] {
        let mut decoder = codec::Decoder::default();
        for chunk in data.chunks(chunk_size) {
            for (index, byte) in chunk.iter().enumerate() {
                let _ = decoder.push(*byte, index + 1 < chunk.len());
                assert!(decoder.buffered_bytes() <= codec::MAX_SEQUENCE_BYTES);
            }
            let _ = decoder.finish_pending_escape();
        }
        let _ = decoder.finish_pending_escape();
    }
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = codec::replacement(text);
    }
});
