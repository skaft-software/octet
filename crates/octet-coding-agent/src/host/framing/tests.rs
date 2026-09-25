use super::*;

use tokio::io::BufReader;

use super::super::protocol::MAX_FRAME_BYTES;

#[tokio::test]
async fn frame_reader_discards_oversized_input_without_desynchronizing() {
    let data = format!("{}\n{{\"ok\":true}}\n", "x".repeat(MAX_FRAME_BYTES + 1));
    let mut reader = BufReader::new(data.as_bytes());
    assert!(matches!(
        read_frame(&mut reader).await.unwrap(),
        Some(Frame::Oversized)
    ));
    let Some(Frame::Data(next)) = read_frame(&mut reader).await.unwrap() else {
        panic!("expected the next bounded frame");
    };
    assert_eq!(next, br#"{"ok":true}"#);
}

#[test]
fn outbound_serialization_never_crosses_its_bound() {
    let oversized = serde_json::json!({"text": "x".repeat(MAX_FRAME_BYTES)});
    assert!(serialize_bounded(&oversized, MAX_FRAME_BYTES - 1)
        .unwrap()
        .is_none());

    let bounded = serialize_bounded(&serde_json::json!({"ok": true}), 128)
        .unwrap()
        .unwrap();
    assert!(bounded.len() <= 128);
}
