//! Unit tests for `crate::protocol::sse`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `sse.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol::sse`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn test_sse_decoder_basic() {
    let mut decoder = SseDecoder::new();
    let payload = b"event: message\ndata: hello world\n\n";
    let events = decoder.push(payload).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, Some("message".to_string()));
    assert_eq!(events[0].data, "hello world");
}

#[test]
fn test_sse_decoder_crlf_and_comments() {
    let mut decoder = SseDecoder::new();
    let payload = b":keep-alive\r\nevent: message\r\ndata: first line\r\ndata: second line\r\n\r\n";
    let events = decoder.push(payload).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event, Some("message".to_string()));
    assert_eq!(events[0].data, "first line\nsecond line");
}

#[test]
fn push_frames_preserves_comments_without_changing_ordinary_callers() {
    let payload = b": octet-lifecycle: loading; warming\ndata: hello\n\n";

    let mut framed = SseDecoder::new();
    assert_eq!(
        framed.push_frames(payload).unwrap(),
        vec![
            SseFrame::Comment("octet-lifecycle: loading; warming".into()),
            SseFrame::Event(SseEvent {
                event: None,
                data: "hello".into(),
            }),
        ]
    );

    let mut ordinary = SseDecoder::new();
    assert_eq!(
        ordinary.push(payload).unwrap(),
        vec![SseEvent {
            event: None,
            data: "hello".into(),
        }]
    );
}

#[test]
fn test_sse_decoder_utf8_split_chunking() {
    // UTF-8 character '🎉' is [0xF0, 0x9F, 0x8E, 0x89]
    let mut decoder = SseDecoder::new();
    let chunk1 = b"data: \xf0\x9f";
    let chunk2 = b"\x8e\x89\n\n";

    let events1 = decoder.push(chunk1).unwrap();
    assert!(events1.is_empty());

    let events2 = decoder.push(chunk2).unwrap();
    assert_eq!(events2.len(), 1);
    assert_eq!(events2[0].data, "🎉");
}

#[test]
fn test_sse_decoder_boundary_chunking_property() {
    let payload = b":comment\nevent: ping\ndata: {\"msg\": \"ok\"}\n\ndata: [DONE]\n\n";

    // Feed the payload byte-by-byte to prove it produces identical events
    let mut decoder = SseDecoder::new();
    let mut all_events = Vec::new();
    for &byte in payload.iter() {
        let evs = decoder.push(&[byte]).unwrap();
        all_events.extend(evs);
    }
    let final_ev = decoder.finish().unwrap();
    if let Some(ev) = final_ev {
        all_events.push(ev);
    }

    assert_eq!(all_events.len(), 2);
    assert_eq!(all_events[0].event, Some("ping".to_string()));
    assert_eq!(all_events[0].data, "{\"msg\": \"ok\"}");
    assert_eq!(all_events[1].event, None);
    assert_eq!(all_events[1].data, "[DONE]");
}

#[test]
fn oversized_unterminated_event_is_rejected_before_buffering_it() {
    let mut decoder = SseDecoder::new();
    let payload = vec![b'x'; MAX_SSE_EVENT_BYTES + 1];
    assert!(matches!(
        decoder.push(&payload),
        Err(DecodeError::BodyTooLarge)
    ));
    assert!(decoder.buf.is_empty());
}

#[test]
fn a_large_chunk_of_separate_events_is_not_treated_as_one_event() {
    let data = "x".repeat(MAX_SSE_EVENT_BYTES / 2);
    let event = format!("data: {data}\n\n");
    let payload = event.repeat(3);
    let mut decoder = SseDecoder::new();
    let events = decoder.push(payload.as_bytes()).unwrap();
    assert_eq!(events.len(), 3);
    assert!(events.iter().all(|event| event.data == data));
}

#[test]
fn test_sse_decoder_finish_trailing_no_newline() {
    let decoder = SseDecoder {
        buf: b"data: unfinished".to_vec(),
        current_event: None,
        current_data: Vec::new(),
        current_event_bytes: 0,
    };
    let ev = decoder.finish().unwrap().unwrap();
    assert_eq!(ev.data, "unfinished");
}
