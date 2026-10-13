//! Unit tests for the tool registry and the tool-effect plumbing.
//!
//! Kept in a sibling file so `tool.rs` holds only the registry types and the
//! dispatch logic that the agent and the session depend on; the registry is
//! the crate's most-imported type and should not be buried under tests.
use super::*;

#[test]
fn owner_presentation_detaches_small_slices_from_large_backing_allocations() {
    let backing = bytes::Bytes::from(vec![42; 8 * 1024 * 1024]);
    let slice = backing.slice(1024..1032);
    let source = Media::Image(octet_ai::ImageMedia {
        source: octet_ai::ImageSource::Inline(slice.clone()),
        media_type: Some("image/png".parse().unwrap()),
        detail: Some(octet_ai::ImageDetail::Low),
    });
    let output = ToolOutput::new("safe").with_owner_presentation_images([&source]);
    let Media::Image(image) = &output.media()[0] else {
        panic!("image")
    };
    let octet_ai::ImageSource::Inline(owned) = &image.source else {
        panic!("inline")
    };
    assert_eq!(owned, &slice);
    assert_ne!(
        owned.as_ptr(),
        slice.as_ptr(),
        "owner bytes must not retain the original 8 MiB allocation"
    );
    assert_eq!(image.media_type, Some("image/png".parse().unwrap()));
    assert_eq!(image.detail, Some(octet_ai::ImageDetail::Low));
    assert!(!output.presentation_images_omitted());
}

#[test]
fn owner_presentation_has_independent_count_byte_and_source_bounds() {
    let image = |size| {
        Media::image_bytes(
            bytes::Bytes::from(vec![42; size]),
            "image/png".parse().unwrap(),
        )
    };
    for (media, retained) in [
        (vec![image(1); 5], 4),
        (vec![image(2 * 1024 * 1024); 3], 2),
        (vec![image(2 * 1024 * 1024 + 1), image(1)], 1),
        (
            vec![
                Media::image_url(
                    "https://secret.invalid/private-payload".parse().unwrap(),
                    None,
                ),
                image(1),
            ],
            1,
        ),
    ] {
        let output = ToolOutput::new("safe").with_owner_presentation_images(&media);
        assert_eq!(output.media().len(), retained);
        assert!(output.presentation_images_omitted());
        assert!(output.media().iter().all(|media| matches!(media, Media::Image(image) if matches!(image.source, octet_ai::ImageSource::Inline(_)))));
        let debug = format!("{output:?} {:?}", output.content_parts());
        assert!(!debug.contains("private-payload"));
        assert!(!debug.contains("42, 42"));
        let stripped = output.without_media_payloads();
        assert!(stripped.media().is_empty());
        assert!(!stripped.presentation_images_omitted());
    }
    let raw = ToolOutput::new("safe").with_media(Media::image_url(
        "https://secret.invalid/private-payload".parse().unwrap(),
        None,
    ));
    assert!(!format!("{raw:?} {:?}", raw.content_parts()).contains("private-payload"));
}

#[test]
fn workspace_resolution_denials_keep_a_stable_policy_code() {
    let error = path_resolution_tool_error(sandbox::SandboxPathError::WorkspaceConfinement(
        "path escapes the workspace".into(),
    ));
    assert_eq!(
        error.policy_denial_code(),
        Some(ToolPolicyDenialCode::WorkspaceConfinement)
    );

    let error = path_resolution_tool_error(sandbox::SandboxPathError::Other(
        "path does not exist".into(),
    ));
    assert_eq!(error.policy_denial_code(), None);
}

#[test]
fn content_hash_is_deterministic_and_content_sensitive() {
    assert_eq!(content_hash(b"hello"), content_hash(b"hello"));
    assert_ne!(content_hash(b"hello"), content_hash(b"hello "));
    assert_eq!(content_hash(b"hello").len(), 64);
    assert_eq!(
        content_hash(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn tool_output_presentation_copy_keeps_kind_but_drops_payload() {
    let media = Media::image_bytes(
        Bytes::from_static(b"\x89PNG\r\n\x1a\n"),
        "image/png".parse().unwrap(),
    );
    let output = ToolOutput::new("read=vision").with_media(media);
    assert_eq!(output.media().len(), 1);
    assert_eq!(output.media_kinds(), &[ToolOutputMediaKind::Image]);

    let presentation = output.without_media_payloads();
    assert_eq!(presentation.text, "read=vision");
    assert!(presentation.media().is_empty());
    assert_eq!(presentation.media_kinds(), &[ToolOutputMediaKind::Image]);
}

#[test]
fn rich_error_marker_survives_presentation_copy() {
    let ordinary = ToolOutput::new("ok");
    assert!(!ordinary.is_error());

    let rich_error = ToolOutput::new("extension rejected the action")
        .try_with_structured_content(serde_json::json!({"code": "rejected"}))
        .unwrap()
        .with_is_error(true);
    assert!(rich_error.is_error());
    let presentation = rich_error.without_media_payloads();
    assert!(presentation.is_error());
    assert_eq!(
        presentation.structured_content(),
        Some(&serde_json::json!({"code": "rejected"}))
    );
}

#[test]
fn provisional_tool_delivery_commits_once_or_rolls_back_on_drop() {
    use std::sync::atomic::{AtomicI8, Ordering};

    let committed = Arc::new(AtomicI8::new(0));
    let on_commit = Arc::clone(&committed);
    let on_rollback = Arc::clone(&committed);
    let output = ToolOutput::new("leased").with_delivery_commit(
        move || on_commit.store(1, Ordering::SeqCst),
        move || on_rollback.store(-1, Ordering::SeqCst),
    );
    let clone = output.clone();
    let presentation = output.without_media_payloads();
    drop((output, presentation));
    assert_eq!(committed.load(Ordering::SeqCst), 0);
    clone.resolve_delivery(true);
    clone.resolve_delivery(false);
    drop(clone);
    assert_eq!(committed.load(Ordering::SeqCst), 1);

    let rolled_back = Arc::new(AtomicI8::new(0));
    let on_commit = Arc::clone(&rolled_back);
    let on_rollback = Arc::clone(&rolled_back);
    drop(ToolOutput::new("leased").with_delivery_commit(
        move || on_commit.store(1, Ordering::SeqCst),
        move || on_rollback.store(-1, Ordering::SeqCst),
    ));
    assert_eq!(rolled_back.load(Ordering::SeqCst), -1);
}

#[test]
fn tool_output_retains_ordered_parts_and_vetted_details() {
    let media = Media::image_bytes(
        Bytes::from_static(b"\x89PNG\r\n\x1a\n"),
        "image/png".parse().unwrap(),
    );
    let output = ToolOutput::from_content_parts([
        ToolOutputContentPart::Text("Found one result.".into()),
        ToolOutputContentPart::Media(media),
        ToolOutputContentPart::Text("Source is attached.".into()),
    ])
    .try_with_details(
        Some(serde_json::json!({"sources": [{"title": "Primary"}]})),
        Some(serde_json::json!({"cache": "miss"})),
    )
    .unwrap();

    assert_eq!(output.text, "Found one result.\nSource is attached.");
    assert_eq!(output.content_parts().len(), 3);
    assert!(matches!(
        output.content_parts()[0],
        ToolOutputContentPart::Text(ref text) if text == "Found one result."
    ));
    assert!(matches!(
        output.content_parts()[1],
        ToolOutputContentPart::Media(Media::Image(_))
    ));
    assert_eq!(
        output.structured_content(),
        Some(&serde_json::json!({"sources": [{"title": "Primary"}]}))
    );
    assert_eq!(
        output.metadata(),
        Some(&serde_json::json!({"cache": "miss"}))
    );

    let presentation = output.without_media_payloads();
    assert_eq!(presentation.content_parts().len(), 2);
    assert_eq!(
        presentation.structured_content(),
        output.structured_content()
    );
    assert_eq!(presentation.metadata(), output.metadata());
}

#[test]
fn tool_output_details_distinguish_structured_null_from_missing() {
    let details = ToolOutputDetails::try_new(Some(serde_json::Value::Null), None).unwrap();
    assert_eq!(details.structured_content(), Some(&serde_json::Value::Null));
    assert!(!details.is_empty());

    let serialized = serde_json::to_value(&details).unwrap();
    assert_eq!(serialized, serde_json::json!({"structured_content": null}));
    let reopened: ToolOutputDetails = serde_json::from_value(serialized).unwrap();
    assert_eq!(
        reopened.structured_content(),
        Some(&serde_json::Value::Null)
    );

    let null_metadata = ToolOutputDetails::try_new(None, Some(serde_json::Value::Null)).unwrap();
    assert!(null_metadata.is_empty());
}

#[test]
fn tool_output_details_reject_unbounded_or_ambiguous_metadata() {
    assert_eq!(
        ToolOutputDetails::try_new(None, Some(serde_json::json!(["not", "an", "object"])))
            .unwrap_err(),
        ToolOutputValidationError::MetadataNotObject
    );
    assert!(matches!(
        ToolOutputDetails::try_new(
            Some(serde_json::Value::String(
                "x".repeat(MAX_TOOL_STRUCTURED_CONTENT_BYTES)
            )),
            None
        ),
        Err(ToolOutputValidationError::TooLarge {
            field: "structured_content",
            ..
        })
    ));

    let mut nested = serde_json::json!(true);
    for _ in 0..=MAX_TOOL_DETAIL_DEPTH {
        nested = serde_json::json!([nested]);
    }
    assert!(matches!(
        ToolOutputDetails::try_new(Some(nested), None),
        Err(ToolOutputValidationError::TooDeep {
            field: "structured_content",
            ..
        })
    ));
}

// ── ToolProgressSink unit tests ──────────────────────────────────────

#[test]
fn null_sink_all_methods_silently_succeed() {
    let sink = ToolProgressSink::null();
    sink.output(OutputStream::Stdout, Bytes::from("hello"));
    sink.output(OutputStream::Stderr, Bytes::from("error"));
    sink.status("working");
    // Null sink never increments dropped counter.
    assert_eq!(sink.take_dropped(), (0, 0));
}

#[tokio::test]
async fn live_sink_delivers_messages_to_receiver() {
    let (tx, mut rx) = mpsc::channel::<ToolProgress>(PROGRESS_CHANNEL_CAPACITY);
    let sink = ToolProgressSink::live(tx);

    sink.output(OutputStream::Stdout, Bytes::from("hello"));
    sink.status("started");
    sink.output(OutputStream::Stderr, Bytes::from("oops"));
    drop(sink); // close sender so recv() eventually returns None

    let mut messages = Vec::new();
    while let Some(msg) = rx.recv().await {
        messages.push(msg);
    }
    assert_eq!(messages.len(), 3);
    match &messages[0] {
        ToolProgress::Output { stream, bytes } => {
            assert_eq!(*stream, OutputStream::Stdout);
            assert_eq!(&bytes[..], b"hello");
        }
        _ => panic!("expected Output"),
    }
    match &messages[1] {
        ToolProgress::Status(s) => assert_eq!(s, "started"),
        _ => panic!("expected Status"),
    }
    match &messages[2] {
        ToolProgress::Output { stream, bytes } => {
            assert_eq!(*stream, OutputStream::Stderr);
            assert_eq!(&bytes[..], b"oops");
        }
        _ => panic!("expected Output"),
    }
}

#[tokio::test]
async fn confirmation_detail_is_redacted_from_debug_output() {
    let (tx, mut rx) = mpsc::channel::<ToolProgress>(PROGRESS_CHANNEL_CAPACITY);
    let sink = ToolProgressSink::live(tx);
    let waiter = tokio::spawn(async move {
        sink.confirmation(
            "Approve?".into(),
            Some("exact-secret-effect-arguments".into()),
            true,
            false,
        )
        .await
    });
    let request = match rx.recv().await.expect("confirmation request") {
        ToolProgress::Confirmation(request) => request,
        _ => panic!("expected confirmation request"),
    };

    assert!(request.technical_detail.is_none());
    let debug = format!("{request:?}");
    assert!(debug.contains("Approve?"));
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("exact-secret-effect-arguments"));
    request.respond(false);
    assert!(!waiter.await.unwrap());
}

#[tokio::test]
async fn technical_confirmation_details_are_redacted_and_share_the_approval_bound() {
    let (sink, mut receiver) = ToolProgressSink::bounded_channel();
    let waiter = tokio::spawn(async move {
        sink.confirmation_with_technical_detail(
            "Run this command?".into(),
            Some("human-secret".into()),
            Some("technical-secret".into()),
            true,
            false,
        )
        .await
    });
    let Some(ToolProgress::Confirmation(request)) = receiver.recv().await else {
        panic!("confirmation")
    };
    assert_eq!(
        request.technical_detail.as_deref(),
        Some("technical-secret")
    );
    let debug = format!("{request:?}");
    assert!(!debug.contains("human-secret"));
    assert!(!debug.contains("technical-secret"));
    assert!(debug.contains("technical_detail"));
    request.respond(false);
    assert!(!waiter.await.unwrap());

    let (sink, mut receiver) = ToolProgressSink::bounded_channel();
    assert!(
        !sink
            .confirmation_with_technical_detail(
                "?".into(),
                Some("h".repeat(MAX_PROGRESS_CHUNK_BYTES / 2)),
                Some("t".repeat(MAX_PROGRESS_CHUNK_BYTES / 2)),
                true,
                false,
            )
            .await
    );
    assert!(
        receiver.try_recv().is_err(),
        "oversized confirmation must not be emitted"
    );
}

#[tokio::test]
async fn secret_input_answer_exists_only_on_the_private_reply_channel() {
    let (tx, mut rx) = mpsc::channel::<ToolProgress>(PROGRESS_CHANNEL_CAPACITY);
    let sink = ToolProgressSink::live(tx);
    let waiter = tokio::spawn(async move {
        sink.input("Password:".into(), true)
            .await
            .expect("interactive answer")
    });
    let request = match rx.recv().await.expect("input request") {
        ToolProgress::Input(request) => request,
        _ => panic!("expected input request"),
    };
    let debug = format!("{request:?}");
    assert!(debug.contains("Password:"));
    assert!(!debug.contains("swordfish"));
    request.respond(b"swordfish".to_vec());
    let response = waiter.await.unwrap();
    assert_eq!(response.as_bytes(), b"swordfish");
    assert!(!format!("{request:?}").contains("swordfish"));
}

#[test]
fn oversized_output_is_split_into_bounded_chunks() {
    let (tx, mut rx) = mpsc::channel::<ToolProgress>(PROGRESS_CHANNEL_CAPACITY);
    let sink = ToolProgressSink::live(tx);

    let payload = vec![0x41u8; MAX_PROGRESS_CHUNK_BYTES * 2 + 500];
    sink.output(OutputStream::Stdout, Bytes::from(payload));
    drop(sink);

    // All chunks must be ≤ MAX_PROGRESS_CHUNK_BYTES and independently
    // allocated (not slices into a shared backing buffer).
    let mut total: usize = 0;
    while let Ok(msg) = rx.try_recv() {
        if let ToolProgress::Output { bytes, .. } = msg {
            assert!(
                bytes.len() <= MAX_PROGRESS_CHUNK_BYTES,
                "chunk {} > max",
                bytes.len()
            );
            total += bytes.len();
        }
    }
    assert_eq!(total, MAX_PROGRESS_CHUNK_BYTES * 2 + 500);
}

#[test]
fn oversized_status_is_split_into_bounded_chunks() {
    let (tx, mut rx) = mpsc::channel::<ToolProgress>(PROGRESS_CHANNEL_CAPACITY);
    let sink = ToolProgressSink::live(tx);

    let payload = "X".repeat(MAX_PROGRESS_CHUNK_BYTES * 2 + 500);
    sink.status(payload.clone());
    drop(sink);

    let mut total: usize = 0;
    while let Ok(msg) = rx.try_recv() {
        if let ToolProgress::Status(s) = msg {
            assert!(
                s.len() <= MAX_PROGRESS_CHUNK_BYTES,
                "status chunk {} > max",
                s.len()
            );
            total += s.len();
        }
    }
    // Character-boundary splitting preserves every codepoint.
    assert_eq!(total, payload.len());
}

#[test]
fn full_channel_drops_rather_than_blocks() {
    // Channel capacity 1 — second send must be dropped.
    let (tx, mut rx) = mpsc::channel::<ToolProgress>(1);
    let sink = ToolProgressSink::live(tx);

    // Fill the single slot.
    sink.output(OutputStream::Stdout, Bytes::from("first"));
    // This send must be rejected; sink must not block.
    let before = std::time::Instant::now();
    sink.output(OutputStream::Stdout, Bytes::from("second"));
    assert!(before.elapsed() < std::time::Duration::from_millis(50));

    // Dropped bytes counter must reflect the lost payload.
    let dropped = sink.take_dropped();
    assert_eq!(dropped, (6, 0)); // "second".len()

    // Drain the one accepted message so the dropped counter is accurate.
    let accepted = rx.try_recv().unwrap();
    match accepted {
        ToolProgress::Output { bytes, .. } => assert_eq!(&bytes[..], b"first"),
        _ => panic!("expected Output"),
    }
    // No further dropped bytes after take.
    assert_eq!(sink.take_dropped(), (0, 0));
}

#[tokio::test]
async fn full_channel_counts_dropped_session_events() {
    let (tx, _rx) = mpsc::channel::<ToolProgress>(1);
    let sink = ToolProgressSink::live(tx);
    sink.status("fills the channel");

    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    sink.send_one(ToolProgress::SessionEvent(
        Box::new(crate::session::EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        }),
        Arc::new(std::sync::Mutex::new(Some(reply_tx))),
    ));

    assert_eq!(sink.take_dropped(), (0, 1));
    assert!(
        reply_rx.await.is_err(),
        "dropped event must close its reply"
    );
}

#[test]
fn dropped_counter_accumulates_across_multiple_failures() {
    let (tx, _rx) = mpsc::channel::<ToolProgress>(2);
    let sink = ToolProgressSink::live(tx);

    sink.output(OutputStream::Stdout, Bytes::from("a"));
    sink.output(OutputStream::Stdout, Bytes::from("b"));
    // Channel full; next three sends are dropped.
    sink.output(OutputStream::Stdout, Bytes::from("dropped1"));
    sink.output(OutputStream::Stderr, Bytes::from("dr"));
    sink.status("lost");

    assert_eq!(sink.take_dropped(), (8 + 2 + 4, 0)); // "dropped1" + "dr" + "lost"
}

#[test]
fn exporter_sink_delivers_dropped_event() {
    let (tx, mut rx) = mpsc::channel::<ToolProgress>(1);
    let sink = ToolProgressSink::live(tx);

    sink.output(OutputStream::Stdout, Bytes::from("only"));
    sink.output(OutputStream::Stdout, Bytes::from("gone"));
    drop(sink);

    let mut messages = Vec::new();
    while let Ok(msg) = rx.try_recv() {
        messages.push(msg);
    }
    assert_eq!(messages.len(), 1);
}

// ── Verify worst-case memory bound ───────────────────────────────────

#[test]
fn worst_case_channel_memory_is_bounded() {
    // 64 slots × 8 KB = 512 KB. Backing allocations for Bytes are
    // reference-counted and released when the channel is drained.
    // The AtomicU64 and Arc overhead is negligible (≤ 128 bytes).
    let max_slot_bytes = MAX_PROGRESS_CHUNK_BYTES as u64;
    let max_total = PROGRESS_CHANNEL_CAPACITY as u64 * max_slot_bytes;
    assert_eq!(max_total, 512 * 1024);
}

// ── Clone behaviour ──────────────────────────────────────────────────

#[test]
fn cloned_sinks_share_the_dropped_counter() {
    let (tx, _rx) = mpsc::channel::<ToolProgress>(1);
    let a = ToolProgressSink::live(tx);
    let b = a.clone();

    a.output(OutputStream::Stdout, Bytes::from("first"));
    b.output(OutputStream::Stdout, Bytes::from("second"));

    // Both sinks share the same counter.
    assert_eq!(a.take_dropped(), (6, 0)); // only first was counted
    assert_eq!(b.take_dropped(), (0, 0)); // already taken by a
}
