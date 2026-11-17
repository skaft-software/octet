#![no_main]

use libfuzzer_sys::fuzz_target;
use octet_agent::extension_process::{
    ExtensionHookOutput, ExtensionRequestId, InitializeResponse, DEFAULT_EXTENSION_MESSAGE_BYTES,
};
use octet_agent::ExtensionRemoteUiFrameNotification;

#[path = "../../crates/octet-agent/src/extension_process/framing.rs"]
mod framing;

fn decode(line: &[u8]) {
    let _ = serde_json::from_slice::<InitializeResponse>(line);
    let _ = serde_json::from_slice::<ExtensionHookOutput>(line);
    let _ = serde_json::from_slice::<ExtensionRemoteUiFrameNotification>(line);
    let _ = serde_json::from_slice::<ExtensionRequestId>(line);
    if let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) {
        // The host dispatches only after parsing the JSON envelope; decode the
        // same API 0.4 provider data without dispatching any process operations.
        let params = value.get("params").unwrap_or(&value).clone();
        let _ = octet_agent::extension_api_v04::parse_provider_stream_event(params);
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() > 64 * 1024 {
        return;
    }
    decode(data); // Also reach arbitrary JSON without a trailing LF.
    for limit in [
        usize::from(data.first().copied().unwrap_or(0)) + 1,
        DEFAULT_EXTENSION_MESSAGE_BYTES,
    ] {
        let mut line = Vec::new();
        for byte in data {
            let result = framing::push_protocol_byte(&mut line, *byte, limit);
            assert!(line.len() <= limit);
            match result {
                Ok(true) => {
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    if !line.is_empty() {
                        decode(&line);
                    }
                    line.fill(0);
                    line.clear();
                }
                Ok(false) => {}
                Err(_) => break,
            }
        }
    }
});
