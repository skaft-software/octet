//! Prompt-history recall bounds and motion, editor navigation, paste collapsing and splicing, and
//! attachment chips. Separate because they all mutate the editor buffer and then assert what
//! survives the drain back into text.

use super::support::*;

use super::*;

#[test]
fn seeded_prompt_history_is_older_newest_first_deduplicated_and_preserves_the_draft() {
    let mut shell = InteractiveShell::test_shell();
    shell.on_prompt_submitted("native");
    shell.extension_set_editor("draft".into());
    shell.seed_prompt_history(vec![
        "newer".into(),
        "native".into(),
        "older".into(),
        "newer".into(),
    ]);
    assert_eq!(shell.pending(), "draft");
    assert_eq!(
        shell
            .state
            .borrow()
            .prompt_history
            .iter()
            .map(|entry| entry.display_text.as_str())
            .collect::<Vec<_>>(),
        vec!["older", "newer", "native"]
    );
    shell.state.borrow_mut().editor.set_cursor(0);
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), "native");
    shell.seed_prompt_history(vec!["oldest".into()]);
    shell.apply_edit(EditAction::Up);
    assert_eq!(
        shell.pending(),
        "newer",
        "seeding shifted the active history index"
    );
    shell.apply_edit(EditAction::Down);
    shell.apply_edit(EditAction::Down);
    assert_eq!(
        shell.pending(),
        "draft",
        "seeding discarded the saved draft"
    );
}

#[test]
fn seeded_prompt_history_keeps_the_most_recent_entries_and_native_attachments() {
    let mut shell = InteractiveShell::test_shell();
    shell.apply_edit(EditAction::Paste("native line\n".repeat(20)));
    let sent = shell.drain_composed();
    shell.on_composed_prompt_submitted(&sent);
    let mut entries: Vec<_> = (0..100).rev().map(|i| format!("imported {i}")).collect();
    entries.insert(0, sent.display_text.clone());
    shell.seed_prompt_history(entries);
    let state = shell.state.borrow();
    assert_eq!(state.prompt_history.len(), MAX_PROMPT_HISTORY_ENTRIES);
    assert_eq!(state.prompt_history[0].display_text, "imported 1");
    assert_eq!(state.prompt_history.last().unwrap().attachments.len(), 1);
    drop(state);
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.drain_composed().attachments.len(), 1);
}

#[test]
fn prompt_history_repeats_with_bounds_and_restores_an_empty_draft() {
    let mut shell = InteractiveShell::test_shell();
    shell.on_prompt_submitted("first");
    shell.on_prompt_submitted("second");

    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), "second");
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), "first");
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), "first", "oldest history wrapped");
    shell.apply_edit(EditAction::Down);
    assert_eq!(shell.pending(), "second");
    shell.apply_edit(EditAction::Down);
    assert_eq!(shell.pending(), "", "newest history lost the empty draft");
    assert_eq!(shell.state.borrow().editor.cursor(), 0);
    assert!(shell.state.borrow().prompt_history_navigation.is_none());
    assert_eq!(shell.state.borrow().prompt_history.len(), 2);
}

#[test]
fn prompt_history_keeps_multiline_motion_away_from_text_boundaries() {
    let mut shell = InteractiveShell::test_shell();
    shell.on_prompt_submitted("sent");
    for character in "first\nsecond".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    let draft = shell.pending();

    shell.state.borrow_mut().editor.set_cursor(7);
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), draft);
    assert!(shell.state.borrow().prompt_history_navigation.is_none());

    let before_down = shell.state.borrow().editor.cursor();
    shell.state.borrow_mut().editor.set_cursor(draft.len() - 1);
    shell.apply_edit(EditAction::Down);
    assert_eq!(shell.pending(), draft);
    assert!(shell.state.borrow().editor.cursor() >= before_down);
    assert!(shell.state.borrow().prompt_history_navigation.is_none());
}

#[test]
fn prompt_history_editing_does_not_mutate_the_recalled_original() {
    let mut shell = InteractiveShell::test_shell();
    shell.on_prompt_submitted("original");
    shell.apply_edit(EditAction::Up);
    shell.apply_edit(EditAction::Char('!'));
    assert_eq!(shell.pending(), "original!");
    assert_eq!(
        shell.state.borrow().prompt_history[0].display_text,
        "original"
    );

    let resubmitted = shell.drain_composed();
    shell.on_prompt_submitted(&resubmitted.display_text);
    assert_eq!(shell.state.borrow().prompt_history.len(), 2);
    assert_eq!(
        shell.state.borrow().prompt_history[0].display_text,
        "original"
    );
    assert_eq!(
        shell.state.borrow().prompt_history[1].display_text,
        "original!"
    );
}

#[test]
fn prompt_history_preserves_collapsed_paste_masks_and_payloads() {
    let mut shell = InteractiveShell::test_shell();
    shell.apply_edit(EditAction::Paste("sent line\n".repeat(20)));
    let sent = shell.drain_composed();
    let display = sent.display_text.clone();
    assert_eq!(sent.attachments.len(), 1);
    shell.on_composed_prompt_submitted(&sent);

    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), display);
    let recalled = shell.drain_composed();
    assert_eq!(recalled.display_text, display);
    assert_eq!(recalled.attachments.len(), 1);
    assert!(matches!(
        recalled.parts.as_slice(),
        [octet_agent::InputPart::Text(text)] if text.contains("sent line")
    ));
    assert_eq!(shell.state.borrow().prompt_history[0].attachments.len(), 1);
}

#[test]
fn prompt_history_restores_the_draft_cursor_and_payload_at_newest_boundary() {
    let mut shell = InteractiveShell::test_shell();
    shell.on_prompt_submitted("sent");
    shell.apply_edit(EditAction::Paste("draft line\n".repeat(20)));
    let draft_display = shell.pending();
    let paste_cursor = shell.state.borrow().editor.cursor();
    assert!(
        paste_cursor > 0,
        "paste must leave its chip insertion cursor"
    );

    // A chip is one visual row: recall immediately, preserving its insertion
    // cursor instead of first snapping the unsent draft to byte zero.
    let draft_cursor = paste_cursor;
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), "sent");
    assert!(shell.state.borrow().prompt_history_navigation.is_some());
    shell.apply_edit(EditAction::Down);
    assert_eq!(shell.pending(), draft_display);
    assert_eq!(shell.state.borrow().editor.cursor(), draft_cursor);
    assert!(shell.state.borrow().prompt_history_navigation.is_none());
    let restored = shell.drain_composed();
    assert_eq!(restored.display_text, draft_display);
    assert!(matches!(
        restored.parts.as_slice(),
        [octet_agent::InputPart::Text(text)] if text.contains("draft line")
    ));
}

#[test]
fn prompt_history_single_line_drafts_recall_immediately_idle_and_active() {
    for active in [false, true] {
        for cursor in [2, "é🦀 draft".len()] {
            let mut shell = InteractiveShell::test_shell();
            shell.on_prompt_submitted("oldest\nmultiline");
            shell.on_prompt_submitted("newest\nmultiline");
            shell.prefill_editor("é🦀 draft".into());
            shell.state.borrow_mut().editor.set_cursor(cursor);
            let run = active.then(|| shell.begin_run("background work"));
            shell.apply_edit(EditAction::Up);
            assert_eq!(shell.pending(), "newest\nmultiline");
            shell.apply_edit(EditAction::Up);
            assert_eq!(shell.pending(), "oldest\nmultiline");
            shell.apply_edit(EditAction::Up);
            assert_eq!(shell.pending(), "oldest\nmultiline");
            shell.apply_edit(EditAction::Down);
            assert_eq!(shell.pending(), "newest\nmultiline");
            shell.apply_edit(EditAction::Down);
            assert_eq!(shell.pending(), "é🦀 draft");
            assert_eq!(shell.state.borrow().editor.cursor(), cursor);
            assert_eq!(shell.current_run_id(), run);
        }
    }
}

#[test]
fn prompt_history_respects_wrapped_visual_rows_before_recall() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(16, 24);
    shell.on_prompt_submitted("previous");
    let draft = "a wrapped unsent draft with several visual rows";
    shell.prefill_editor(draft.into());
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), draft);
    assert!(shell.state.borrow().prompt_history_navigation.is_none());
    assert!(shell.state.borrow().editor.cursor() < draft.len());
    shell.state.borrow_mut().editor.set_cursor(2);
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), "previous");
    shell.apply_edit(EditAction::Down);
    assert_eq!(shell.pending(), draft);
    assert_eq!(shell.state.borrow().editor.cursor(), 2);
}

#[test]
fn prompt_history_preserves_chip_drafts_while_a_run_is_active() {
    let mut shell = InteractiveShell::test_shell();
    shell.on_prompt_submitted("previous");
    shell.apply_edit(EditAction::Paste("unsent payload\n".repeat(30)));
    let draft = shell.pending();
    let cursor = shell.state.borrow().editor.cursor();
    let run = shell.begin_run("background work");
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), "previous");
    shell.apply_edit(EditAction::Down);
    assert_eq!(shell.pending(), draft);
    assert_eq!(shell.state.borrow().editor.cursor(), cursor);
    assert_eq!(shell.current_run_id(), Some(run));
    let restored = shell.drain_composed();
    assert_eq!(restored.attachments.len(), 1);
    assert!(
        matches!(restored.parts.as_slice(), [octet_agent::InputPart::Text(text)]
        if text.contains("unsent payload"))
    );
}

#[test]
fn prompt_history_is_bounded_to_recent_successful_prompts() {
    let mut shell = InteractiveShell::test_shell();
    for index in 0..(MAX_PROMPT_HISTORY_ENTRIES + 3) {
        shell.on_prompt_submitted(&format!("prompt {index}"));
    }
    assert_eq!(
        shell.state.borrow().prompt_history.len(),
        MAX_PROMPT_HISTORY_ENTRIES
    );
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.pending(), "prompt 102");
    for _ in 0..MAX_PROMPT_HISTORY_ENTRIES {
        shell.apply_edit(EditAction::Up);
    }
    assert_eq!(
        shell.pending(),
        "prompt 3",
        "oldest retained prompt wrapped"
    );
}

#[test]
fn vertical_editor_navigation_snaps_to_document_boundaries_in_one_step() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(40, 12);
    for character in "first\nsecond\nthird".chars() {
        shell.apply_edit(EditAction::Char(character));
    }

    shell.state.borrow_mut().editor.set_cursor(3);
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.state.borrow().editor.cursor(), 0);

    let editor_len = shell.state.borrow().editor.text().len();
    shell.state.borrow_mut().editor.set_cursor(editor_len - 2);
    shell.apply_edit(EditAction::Down);
    assert_eq!(shell.state.borrow().editor.cursor(), editor_len);
}

#[test]
fn vertical_editor_navigation_snaps_at_soft_wrapped_boundaries() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(8, 12);
    for character in "abcdefghijklm".chars() {
        shell.apply_edit(EditAction::Char(character));
    }

    shell.state.borrow_mut().editor.set_cursor(3);
    shell.apply_edit(EditAction::Up);
    assert_eq!(shell.state.borrow().editor.cursor(), 0);

    let editor_len = shell.state.borrow().editor.text().len();
    shell.state.borrow_mut().editor.set_cursor(editor_len - 2);
    assert_eq!(
        {
            let state = shell.state.borrow();
            let geometry = crate::tui::composer_surface::composer_editor_geometry(&state, 8);
            let cursor_row = state.composer_editor_projection(geometry).cursor_row();
            cursor_row
        },
        2,
        "fixture cursor must begin on the bottom soft-wrapped row"
    );
    shell.apply_edit(EditAction::Down);
    assert_eq!(shell.state.borrow().editor.cursor(), editor_len);
}

#[test]
fn clear_editor_discards_attachments_and_resets_composer_navigation() {
    let mut shell = InteractiveShell::test_shell();
    shell.apply_edit(EditAction::Paste("discarded\n".repeat(20)));
    assert!(!shell.state.borrow().ledger.is_empty());
    {
        let mut state = shell.state.borrow_mut();
        state.editor.set_cursor(3);
        state.slash_selection = 4;
        state.slash_scroll = 2;
        state.slash_popup_dismissed = true;
    }

    shell.clear_editor();

    {
        let state = shell.state.borrow();
        assert!(state.editor.is_empty());
        assert_eq!(state.editor.cursor(), 0);
        assert!(state.ledger.is_empty());
        assert_eq!(state.slash_selection, 0);
        assert_eq!(state.slash_scroll, 0);
        assert!(!state.slash_popup_dismissed);
    }

    shell.apply_edit(EditAction::Paste("kept\n".repeat(20)));
    assert!(
        shell.pending().starts_with("[Pasted text #2:"),
        "clearing a draft must not reuse an attachment ID"
    );
    let composed = shell.drain_composed();
    assert!(matches!(
        composed.parts.as_slice(),
        [octet_agent::InputPart::Text(text)]
            if text.contains("kept") && !text.contains("discarded")
    ));
}

#[test]
fn bracketed_paste_preserves_multiline_editor_text_without_submitting() {
    let mut shell = InteractiveShell::test_shell();
    shell.apply_edit(EditAction::Char('a'));
    shell.apply_edit(EditAction::Paste("b\r\nc\rd".into()));
    assert_eq!(shell.pending(), "ab\nc\nd");
    assert_eq!(shell.state.borrow().editor.cursor(), "ab\nc\nd".len());
    let rendered = render_shell(&shell.state.borrow(), 120);
    assert!(rendered.iter().any(|line| line.contains("ab")));
    assert!(rendered.iter().any(|line| line.contains("c")));
}

#[test]
fn composer_delegates_grapheme_edits_and_repaints_only_the_native_frame_suffix() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(24, 12);
    shell.notice("committed transcript row");

    let now = Instant::now();
    let mut frame = ShellFrameState::default();
    let _initial = render_shell_update(&shell.state.borrow(), 24, now, &mut frame);
    let committed = frame.transcript_len;

    shell.apply_edit(EditAction::Paste("e\u{301}👩‍💻界".into()));
    shell.apply_edit(EditAction::Left);
    shell.apply_edit(EditAction::Backspace);
    assert_eq!(shell.pending(), "e\u{301}界");
    shell.apply_edit(EditAction::Delete);
    assert_eq!(shell.pending(), "e\u{301}");
    assert!(shell.state.borrow().editor.cursor_is_valid());

    let update = render_shell_update(&shell.state.borrow(), 24, now, &mut frame);
    assert_eq!(update.stable_prefix, committed);
    assert!(!update.rebuild_scrollback);
    assert!(
        !update
            .replacement
            .iter()
            .any(|line| line.contains("committed transcript row")),
        "draft edits must not replay committed native history"
    );
    assert_eq!(
        update
            .replacement
            .iter()
            .map(|line| line.matches(CURSOR_MARKER).count())
            .sum::<usize>(),
        1,
        "the reusable editor projection must emit one cursor marker"
    );
    assert!(update
        .replacement
        .iter()
        .map(|line| strip_terminal_sequences(line))
        .any(|line| line.contains("e\u{301}")));
}

#[test]
fn media_path_paste_attaches_a_chip_and_composes_media_parts() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("shot.png");
    std::fs::write(&image, b"png").unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_input_modalities(
        octet_ai::ModalitySet::none()
            .with(octet_ai::Modality::Image)
            .with(octet_ai::Modality::Audio),
    );
    for character in "see ".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    shell.apply_edit(EditAction::Paste(image.display().to_string()));

    let composed = shell.drain_composed();
    assert_eq!(composed.display_text, "see [Image #1]");
    assert!(composed
        .parts
        .iter()
        .any(|part| matches!(part, octet_agent::InputPart::Media(_))));
}

#[test]
fn explicit_paste_wires_quoted_escaped_batches_and_preserves_duplicate_payloads() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("first image.png");
    let audio = dir.path().join("voice memo.wav");
    std::fs::write(&image, b"first image bytes").unwrap();
    std::fs::write(&audio, b"audio bytes").unwrap();

    let escaped_image = image.display().to_string().replace(' ', r"\ ");
    let pasted = format!("{escaped_image} '{}' {escaped_image}", audio.display());
    let mut shell = InteractiveShell::test_shell();
    shell.set_input_modalities(
        octet_ai::ModalitySet::none()
            .with(octet_ai::Modality::Image)
            .with(octet_ai::Modality::Audio),
    );
    shell.apply_edit(EditAction::Paste(pasted));

    let expected = "[Image #1] [Audio #2] [Image #3]";
    assert_eq!(shell.pending(), expected);
    let composed = shell.drain_composed();
    assert_eq!(composed.display_text, expected);
    assert_eq!(
        composed
            .attachments
            .iter()
            .map(|attachment| attachment.chip.as_str())
            .collect::<Vec<_>>(),
        vec!["[Image #1]", "[Audio #2]", "[Image #3]"]
    );

    let media = composed
        .parts
        .iter()
        .filter_map(|part| match part {
            octet_agent::InputPart::Media(media) => Some(media),
            octet_agent::InputPart::Text(_) => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(media.len(), 3);
    assert!(matches!(media[0], &octet_ai::Media::Image(_)));
    assert!(matches!(media[1], &octet_ai::Media::Audio(_)));
    assert!(matches!(media[2], &octet_ai::Media::Image(_)));
    fn inline_bytes(media: &octet_ai::Media) -> &[u8] {
        match media {
            octet_ai::Media::Image(image) => match &image.source {
                octet_ai::ImageSource::Inline(bytes) => bytes.as_ref(),
                _ => panic!("expected inline image"),
            },
            octet_ai::Media::Audio(audio) => match &audio.payload {
                octet_ai::AudioPayload::Inline(bytes) => bytes.as_ref(),
                _ => panic!("expected inline audio"),
            },
        }
    }
    assert_eq!(inline_bytes(media[0]), b"first image bytes");
    assert_eq!(inline_bytes(media[1]), b"audio bytes");
    assert_eq!(inline_bytes(media[2]), b"first image bytes");

    // Separate paste events must not reuse the first mask or merge the two
    // payloads merely because they identify the same file.
    let mut consecutive = InteractiveShell::test_shell();
    consecutive.set_input_modalities(
        octet_ai::ModalitySet::none()
            .with(octet_ai::Modality::Image)
            .with(octet_ai::Modality::Audio),
    );
    consecutive.apply_edit(EditAction::Paste(escaped_image));
    consecutive.apply_edit(EditAction::Paste(format!("'{}'", image.display())));
    assert_eq!(consecutive.pending(), "[Image #1][Image #2]");
    let consecutive_input = consecutive.drain_composed();
    assert_eq!(consecutive_input.attachments.len(), 2);
    assert_eq!(
        consecutive_input
            .attachments
            .iter()
            .map(|attachment| attachment.chip.as_str())
            .collect::<Vec<_>>(),
        vec!["[Image #1]", "[Image #2]"]
    );
    assert_eq!(
        consecutive_input
            .parts
            .iter()
            .filter(|part| matches!(part, octet_agent::InputPart::Media(_)))
            .count(),
        2
    );
}

#[test]
fn explicit_paste_batch_failure_keeps_original_text_without_partial_masks() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("good.png");
    let bad = dir.path().join("bad.flac");
    std::fs::write(&good, b"good image").unwrap();
    std::fs::write(&bad, b"not a native chat codec").unwrap();
    let pasted = format!("'{}' {}", good.display(), bad.display());

    let mut shell = InteractiveShell::test_shell();
    shell.set_input_modalities(
        octet_ai::ModalitySet::none()
            .with(octet_ai::Modality::Image)
            .with(octet_ai::Modality::Audio),
    );
    shell.apply_edit(EditAction::Paste(pasted.clone()));

    assert_eq!(shell.pending(), pasted);
    assert!(shell.state.borrow().ledger.is_empty());
    let diagnostic = shell.debug_snapshot();
    assert!(diagnostic.contains("WAV or MP3"), "{diagnostic}");
    let composed = shell.drain_composed();
    assert_eq!(composed.transcript_text, pasted);
    assert!(composed.attachments.is_empty());
    assert!(composed
        .parts
        .iter()
        .all(|part| matches!(part, octet_agent::InputPart::Text(_))));
}

#[test]
fn explicit_paste_rejections_keep_input_visible_and_explain_the_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("shot.png");
    let flac = dir.path().join("voice.flac");
    let video = dir.path().join("clip.mp4");
    let oversized = dir.path().join("oversized.png");
    std::fs::write(&image, b"image").unwrap();
    std::fs::write(&flac, b"flac").unwrap();
    std::fs::write(&video, b"video").unwrap();
    std::fs::write(
        &oversized,
        vec![0_u8; (crate::tui::composer::MAX_IMAGE_BYTES + 1) as usize],
    )
    .unwrap();

    let all_modalities = || {
        octet_ai::ModalitySet::none()
            .with(octet_ai::Modality::Image)
            .with(octet_ai::Modality::Audio)
    };
    let assert_rejected = |shell: &mut InteractiveShell, pasted: String, message: &str| {
        shell.apply_edit(EditAction::Paste(pasted.clone()));
        assert_eq!(shell.pending(), pasted);
        assert!(shell.state.borrow().ledger.is_empty());
        let diagnostic = shell.debug_snapshot();
        assert!(
            diagnostic.contains(message),
            "expected {message:?} in {diagnostic:?}"
        );
    };

    let mut no_capability = InteractiveShell::test_shell();
    no_capability.set_input_modalities(octet_ai::ModalitySet::none());
    assert_rejected(
        &mut no_capability,
        image.display().to_string(),
        "does not accept image input",
    );

    let mut unsupported_codec = InteractiveShell::test_shell();
    unsupported_codec.set_input_modalities(all_modalities());
    assert_rejected(
        &mut unsupported_codec,
        flac.display().to_string(),
        "WAV or MP3",
    );

    let mut too_large = InteractiveShell::test_shell();
    too_large.set_input_modalities(all_modalities());
    assert_rejected(
        &mut too_large,
        oversized.display().to_string(),
        "5 MB limit",
    );

    let mut unsupported_video = InteractiveShell::test_shell();
    unsupported_video.set_input_modalities(all_modalities());
    assert_rejected(
        &mut unsupported_video,
        video.display().to_string(),
        "unsupported video input",
    );
}

#[test]
fn ordinary_pasted_text_stays_editable_and_slash_commands_are_not_submitted() {
    let mut shell = InteractiveShell::test_shell();
    let text = "ordinary pasted text\nwith no attachment consent";
    shell.apply_edit(EditAction::Paste(text.to_owned()));
    assert_eq!(shell.pending(), text);
    assert!(shell.state.borrow().ledger.is_empty());
    assert!(
        shell.debug_snapshot().is_empty(),
        "paste must not submit a prompt"
    );

    let composed = shell.drain_composed();
    assert!(matches!(
        composed.parts.as_slice(),
        [octet_agent::InputPart::Text(value)] if value == text
    ));

    let mut command = InteractiveShell::test_shell();
    command.apply_edit(EditAction::Paste("/sta".into()));
    assert_eq!(command.pending(), "/sta");
    assert!(command.slash_popup_open());
    command.slash_menu(SlashMenuAction::Select);
    assert_eq!(command.pending(), "/status");
    assert!(
        command.debug_snapshot().is_empty(),
        "picker selection must not submit"
    );
}

#[test]
fn ordinary_typed_media_path_is_not_upload_consent() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("screen shot.png");
    std::fs::write(&image, b"synthetic-private-image-sentinel").unwrap();
    let mut shell = InteractiveShell::test_shell();
    shell.set_input_modalities(octet_ai::ModalitySet::none().with(octet_ai::Modality::Image));
    let escaped = image.display().to_string().replace(' ', "\\ ");
    let prompt = format!("Explain this pathname; do not open it: {escaped}");
    for character in prompt.chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    let composed = shell.drain_composed();
    assert_eq!(composed.display_text, prompt);
    assert!(composed
        .parts
        .iter()
        .all(|part| matches!(part, octet_agent::InputPart::Text(_))));
    assert!(composed.attachments.is_empty());
}

#[test]
fn media_paste_without_capability_inserts_plain_path_and_notice() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("shot.png");
    std::fs::write(&image, b"png").unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_input_modalities(octet_ai::ModalitySet::none());
    shell.apply_edit(EditAction::Paste(image.display().to_string()));

    let composed = shell.drain_composed();
    assert_eq!(composed.display_text, image.display().to_string());
    assert!(composed
        .parts
        .iter()
        .all(|part| matches!(part, octet_agent::InputPart::Text(_))));
    assert!(shell
        .debug_snapshot()
        .contains("does not accept image input"));
}

#[test]
fn large_paste_collapses_to_chip_and_splices_back_on_drain() {
    let mut shell = InteractiveShell::test_shell();
    let large = "line\n".repeat(20);
    shell.apply_edit(EditAction::Paste(large.clone()));

    let state_text = shell.pending();
    assert!(state_text.starts_with("[Pasted text #1: 20 lines]"));

    let composed = shell.drain_composed();
    assert!(matches!(
        composed.parts.as_slice(),
        [octet_agent::InputPart::Text(text)] if text.matches("line").count() == 20
    ));
}

#[test]
fn small_paste_still_inserts_verbatim() {
    let mut shell = InteractiveShell::test_shell();
    shell.apply_edit(EditAction::Paste("first\nsecond".into()));
    assert_eq!(shell.pending(), "first\nsecond");
}

#[test]
fn steering_restore_returns_chips_and_attachments() {
    let mut shell = InteractiveShell::test_shell();
    let large = "line\n".repeat(20);
    shell.apply_edit(EditAction::Paste(large));
    let composed = shell.drain_composed();
    shell.queue_steering(&composed);

    shell.restore_queued_steering();
    assert!(shell.pending().contains("[Pasted text #1: 20 lines]"));
    // The ledger got its entry back: draining resolves the chip again.
    let recomposed = shell.drain_composed();
    assert!(matches!(
        recomposed.parts.as_slice(),
        [octet_agent::InputPart::Text(text)] if text.matches("line").count() == 20
    ));
}

#[test]
fn aborted_final_frame_shows_interruption_and_restored_steering() {
    use octet_agent::{EntryId, FinishReason};

    const WIDTH: u16 = 72;
    const HEIGHT: u16 = 18;
    for synchronized_output in [false, true] {
        let (mut shell, bytes) = emulated_shell_with_sync(
            crate::tui::theme::test_theme(),
            WIDTH,
            HEIGHT,
            synchronized_output,
        );
        let run_id = shell.begin_run("temper");
        shell.queue_steering(&ComposedInput::from_text("inspect renderer".into()));
        shell.queue_steering(&ComposedInput::from_text("then run tests".into()));

        // This is the production ordering at the terminal run boundary:
        // settle the outcome, restore any undelivered queue, then publish
        // one complete frame.
        shell.on_run_event(
            run_id,
            &AgentEvent::RunFinished {
                head: EntryId("aborted-head".into()),
                reason: FinishReason::Aborted,
            },
        );
        shell.restore_queued_steering();
        shell.render();

        let output = bytes.lock().unwrap().clone();
        let mut terminal = vt100::Parser::new(HEIGHT, WIDTH, 128);
        terminal.process(&output);
        let physical = terminal.screen().contents();
        assert_eq!(physical.matches("interrupted").count(), 1, "{physical}");
        assert!(physical.contains("inspect renderer"), "{physical}");
        assert!(physical.contains("then run tests"), "{physical}");
        assert!(!physical.contains("Steering prompt"), "{physical}");
    }
}

#[test]
fn steering_delivery_is_positional_fifo() {
    let mut shell = InteractiveShell::test_shell();
    shell.apply_edit(EditAction::Paste("go left".into()));
    let first = shell.drain_composed();
    shell.queue_steering(&first);
    shell.apply_edit(EditAction::Paste("go right".into()));
    let second = shell.drain_composed();
    shell.queue_steering(&second);

    shell.on_agent_event(&AgentEvent::SteeringDelivered {
        messages: vec!["go left".into()],
    });
    let snapshot = shell.debug_snapshot();
    assert!(snapshot.contains("go left"));
    // Second message still pending.
    assert!(render_shell(&shell.state.borrow(), 120)
        .iter()
        .any(|line| line.contains("go right")));
}

#[test]
fn prompt_bar_cursor_tracks_insertions_and_cursor_motion() {
    let mut shell = InteractiveShell::test_shell();
    for character in "abcdef".chars() {
        shell.apply_edit(EditAction::Char(character));
    }
    shell.apply_edit(EditAction::Left);
    shell.apply_edit(EditAction::Left);
    shell.apply_edit(EditAction::Char('X'));
    assert_eq!(shell.state.borrow().editor.text(), "abcdXef");

    let rendered = render_shell(&shell.state.borrow(), 120);
    let line = rendered
        .iter()
        .find(|line| line.contains(CURSOR_MARKER))
        .unwrap();
    assert!(line.find("abcdX").unwrap() < line.find(CURSOR_MARKER).unwrap());
    assert!(line.find(CURSOR_MARKER).unwrap() < line.find("ef").unwrap());
}
