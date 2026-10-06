//! The read-only document panel, the presentation contract, image reservations, and paste chips
//! against a live shell. Separate because they own the panels that overlay the transcript
//! instead of scrolling with it.

use super::support::*;

use super::*;

#[test]
fn read_only_document_panel_scrolls_and_returns_to_its_owner() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(60, 14);
    shell.open_panel(Panel::ReadOnlyDocument {
        title: "worker · read-only transcript".into(),
        text: (0..30)
            .map(|line| format!("transcript line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n")
            .into(),
        styled: false,
        scroll_from_bottom: 0,
    });

    let initial = render_panel(&shell.state.borrow(), 60)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(initial.contains("transcript line 29"), "{initial}");
    assert!(!initial.contains("transcript line 00"), "{initial}");

    shell.panel_input(&panel_key(crossterm::event::KeyCode::Home));
    let top = render_panel(&shell.state.borrow(), 60)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(top.contains("transcript line 00"), "{top}");
    assert!(!top.contains("transcript line 29"), "{top}");

    shell.update_read_only_document(
        (0..45)
            .map(|line| format!("transcript line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let refreshed_top = render_panel(&shell.state.borrow(), 60)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        refreshed_top.contains("transcript line 00"),
        "{refreshed_top}"
    );
    assert!(
        !refreshed_top.contains("transcript line 44"),
        "{refreshed_top}"
    );

    assert!(matches!(
        shell.panel_input(&panel_key(crossterm::event::KeyCode::Left)),
        Some((PanelResult::Cancel, PanelAction::ReadOnlyDocument))
    ));
    assert!(!shell.has_panel());
}

#[test]
fn read_only_document_home_reaches_top_with_wrapped_error_and_header_chrome() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(42, 16);
    shell.set_identity("local", "model", "high");
    shell.error(
        "a wrapped error consumes several rows before the focused document panel can render"
            .repeat(3),
    );
    shell.open_panel(Panel::ReadOnlyDocument {
        title: "worker · read-only transcript".into(),
        text: (0..40)
            .map(|line| format!("document row {line:02}"))
            .collect::<Vec<_>>()
            .join("\n")
            .into(),
        styled: false,
        scroll_from_bottom: 0,
    });

    shell.panel_input(&panel_key(crossterm::event::KeyCode::Home));
    let rendered = shell_chrome(&shell.state.borrow(), 42, Instant::now())
        .panel
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(rendered.contains("document row 00"), "{rendered}");
}

#[test]
fn panel_border_layout_degrades_to_unframed_narrow_picker() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 20);
    shell.set_theme(theme_with_layout(
        r#"
                show_panel_borders = true
                narrow_breakpoint = 60
                narrow_show_panel_borders = false
            "#,
    ));
    open_select_panel(&mut shell, &["alpha", "beta", "gamma"]);

    let wide = render_panel(&shell.state.borrow(), 80)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    assert_eq!(wide.len(), 8);
    assert!(wide
        .first()
        .is_some_and(|line| line.chars().all(|ch| ch == '─')));
    assert!(wide
        .last()
        .is_some_and(|line| line.chars().all(|ch| ch == '─')));

    let narrow = render_panel(&shell.state.borrow(), 40)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect::<Vec<_>>();
    assert_eq!(narrow.len(), 6);
    assert!(narrow
        .first()
        .is_some_and(|line| line.contains("Select model")));
    assert!(narrow.iter().all(|line| !line.chars().all(|ch| ch == '─')));
}

#[test]
fn custom_theme_keeps_safe_transcript_geometry_across_color_and_width_profiles() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::TerminalBackground;

    let fixed_surface_theme = format!("{SURFACE_TEST_THEME}\n[model]\nuse_lab_color = false\n");
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(96, 80);
    shell.set_theme(crate::tui::theme::test_theme_source_with(
        &fixed_surface_theme,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Dark,
    ));
    populate_theme_fixture(&mut shell);
    let transcript = shell.state.borrow().rendered_transcript(96).join("\n");
    assert!(
        !transcript.contains("\x1b[48;2;255;112;24m"),
        "custom theme leaked the default model-adaptive provenance paint"
    );
    assert!(
        !transcript.contains("\x1b[38;2;255;112;24m"),
        "custom theme rendered provenance as foreground-only"
    );
    let unclosed_backgrounds = transcript
        .lines()
        .filter(|line| ansi_background_is_open_at_end(line))
        .collect::<Vec<_>>();
    assert!(
        unclosed_backgrounds.is_empty(),
        "custom theme leaked a painted surface beyond its row: {unclosed_backgrounds:?}"
    );

    let mut plain_shell = InteractiveShell::test_shell();
    plain_shell.set_size(96, 80);
    plain_shell.set_theme(crate::tui::theme::test_theme_source_with(
        &fixed_surface_theme,
        TerminalCapabilities::test(false, false, ColorDepth::None),
        TerminalBackground::Dark,
    ));
    populate_theme_fixture(&mut plain_shell);
    let plain = plain_shell
        .state
        .borrow()
        .rendered_transcript(96)
        .join("\n");
    assert!(
        !plain.contains('\x1b'),
        "custom theme emitted ANSI in no-color mode"
    );

    let mut narrow_shell = InteractiveShell::test_shell();
    narrow_shell.set_size(40, 80);
    narrow_shell.set_theme(crate::tui::theme::test_theme_source_with(
        &fixed_surface_theme,
        TerminalCapabilities::test(false, false, ColorDepth::None),
        TerminalBackground::Dark,
    ));
    populate_theme_fixture(&mut narrow_shell);
    let narrow_frame = narrow_shell
        .state
        .borrow()
        .rendered_transcript(40)
        .join("\n");
    assert!(
        narrow_frame.lines().all(|line| visible_width(line) <= 40),
        "custom theme overflowed a narrow terminal"
    );

    if std::env::var_os("OCTET_DUMP_THEME_FRAMES").is_some() {
        eprintln!(
            "\n===== custom / wide =====\n{}",
            strip_terminal_sequences(&transcript)
        );
        eprintln!("\n===== custom / narrow =====\n{narrow_frame}");
    }
}

#[test]
fn presentation_contract_renders_short_regular_and_wide_frames() {
    for (label, width, height) in [("short", 46, 8), ("regular", 80, 24), ("wide", 120, 40)] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(width, height);
        populate_theme_fixture(&mut shell);

        let frame = render_shell(&shell.state.borrow(), width);
        assert!(!frame.is_empty(), "{label} frame is empty");
        assert!(
            frame
                .iter()
                .all(|line| visible_width(line) <= usize::from(width)),
            "{label} frame overflowed {width} columns: {frame:?}"
        );
        let plain = frame
            .iter()
            .map(|line| strip_terminal_sequences(line))
            .collect::<Vec<_>>();
        assert!(
            plain.iter().any(|line| line.contains("Review src/lib.rs")),
            "{label} frame lost the durable prompt: {plain:?}"
        );
        assert!(
            plain
                .iter()
                .any(|line| line.contains("draft a local patch")),
            "{label} frame lost the composer draft: {plain:?}"
        );
        assert!(
            plain.iter().any(|line| line.contains("completed")),
            "{label} frame lost the terminal outcome: {plain:?}"
        );
    }
}

#[test]
fn styled_read_only_document_preserves_trusted_ansi_and_sanitizes_plain_documents() {
    let shell = InteractiveShell::test_shell();
    let theme = crate::tui::theme::test_theme();
    let styled_text = format!(
        "{} {}",
        theme.bold(&theme.fg("foreground", "Worker")),
        "\x1b[31mraw-esc\x1b[0m"
    );
    // Styled documents keep theme styling but the producer had already
    // sanitized content; rendering must not re-sanitize away the bold.
    let lines = crate::tui::view::panel_render_test_hook::document_lines(&styled_text, 80, true);
    assert!(
        lines.iter().any(|line| line.contains("\x1b[1m")),
        "styled document lost its trusted ANSI: {lines:?}"
    );
    // Plain documents still sanitize embedded escapes.
    let lines =
        crate::tui::view::panel_render_test_hook::document_lines("before \x1b[31mafter", 80, false);
    assert!(
        lines.iter().all(|line| !line.contains("\x1b")),
        "plain document kept a raw escape: {lines:?}"
    );
    let _ = shell;
}

/// Exercises the real HTTP codec, core read tool, owner stream, terminal
/// projection and disk reopen together; the model decision is deterministic.
#[tokio::test]
async fn actual_read_image_reaches_live_shell_and_reopened_session() {
    use octet_agent::{
        Agent, AgentConfig, CoreTools, EffectBroker, EffectPolicy, ExtensionHost, SandboxConfig,
    };
    use octet_ai::{
        AiClient, Auth, Capabilities, Endpoint, EndpointId, ModelLimits, ModelSpec, Protocol,
        ReasoningConfig,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(move |_: &wiremock::Request| {
            let chunk = if count.fetch_add(1, Ordering::SeqCst) == 0 {
                serde_json::json!({"choices":[{"index":0,"delta":{"tool_calls":[{
                    "index":0,"id":"image-read","type":"function",
                    "function":{"name":"read","arguments":"{\"path\":\"pixel.png\"}"}
                }]},"finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"index":0,"delta":{"content":"accepted"},"finish_reason":"stop"}]})
            };
            ResponseTemplate::new(200).insert_header("content-type", "text/event-stream")
                .set_body_string(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
        }).mount(&server).await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session_path = sessions.path().join("image.jsonl");
    let png: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4,
        0, 0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5,
        1, 1, 39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    std::fs::write(workspace.path().join("pixel.png"), png).unwrap();
    let model = Model {
        spec: Arc::new(ModelSpec {
            preset: Default::default(),
            id: ModelId("image-fixture".into()),
            endpoint: EndpointId("local".into()),
            api_name: "image-fixture".into(),
            display_name: None,
            protocol: Protocol::OpenAiChat,
            capabilities: Capabilities {
                responses_features: Default::default(),
                input_modalities: ModalitySet::none().with(octet_ai::Modality::Image),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 200_000,
                max_output_tokens: 8192,
            },
            pricing: None,
            cache: octet_ai::CacheCompatibility::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("local".into()),
            base_url: server.uri().parse().unwrap(),
            auth: Auth::None,
            default_headers: http::HeaderMap::new(),
            transport: octet_ai::EndpointTransport::Http,
            runtime: octet_ai::RequestRuntime::default(),
            timeout: Duration::from_secs(5),
        }),
    };
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session: Session::create(&session_path).unwrap(),
        system: "Synthetic image fixture".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(3),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    agent.set_owner_tool_images_enabled(true);
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("local");
    let mut run = agent.prompt("read pixel.png").await.unwrap();
    while let Some(event) = run.next().await {
        shell.on_run_event(run_id, &event);
    }
    drop(run);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let images = |shell: &InteractiveShell| {
        shell
            .state
            .borrow()
            .transcript
            .iter()
            .filter_map(|block| match block {
                TranscriptBlock::Tool(panel) => Some(panel.images.clone()),
                _ => None,
            })
            .flatten()
            .collect::<Vec<_>>()
    };
    let live = images(&shell);
    assert_eq!(live.len(), 1);
    assert!(matches!(&live[0], ToolResultImage::Ready { .. }));
    assert!(live[0].id().is_some());
    drop(agent);
    let reopened = Session::open(&session_path).unwrap();
    let mut resumed = InteractiveShell::test_shell();
    resumed.hydrate(&reopened).unwrap();
    assert_eq!(images(&resumed), live);
    for width in [46, 80] {
        for shell in [&mut shell, &mut resumed] {
            shell.set_size(width, 24);
            shell.set_show_images(false);
            let text = render_shell(&shell.state.borrow(), width).join("\n");
            assert!(!text.contains("iVBOR"));
            assert!(!text.contains("137, 80, 78, 71"));
            assert!(!text.contains("\x1b_G"));
            assert!(!text.contains("\x1b]1337;File="));
        }
    }
}

#[test]
fn default_tool_image_reservation_keeps_following_rows_physically_empty() {
    use sexy_tui_rs::{ImageAnchor, ImageId, ImageLayout, ImageProtocol};

    let theme = crate::tui::theme::test_theme();
    let args = serde_json::json!({"path": "comparison-home.png"});
    let block = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("read-image".into()),
        "read".into(),
        args.to_string(),
        summarize_tool("read", &args),
        String::new(),
        true,
        false,
        None,
        None,
    )));
    let plan = compile_surface_plan(None, &block, &theme, 80);
    let anchor = ImageAnchor::new(
        ImageProtocol::Kitty,
        ImageId::new(1).unwrap(),
        ImageLayout::new(30, 16).unwrap(),
    )
    .marker();
    let mut content = vec![anchor.clone()];
    content.extend(vec![String::new(); 15]);
    let rows = super::surface_frame::decorate_surface_with_frame(
        content, &plan, &theme, 80, None, false, None,
    );
    let start = rows.iter().position(|row| row.contains(&anchor)).unwrap();
    for (offset, row) in rows[start + 1..start + 16].iter().enumerate() {
        assert_eq!(visible_width(row), 0, "reserved row {offset}: {row:?}");
    }
}

#[test]
fn finish_transcript_block_preserves_trailing_image_reservation_rows() {
    use sexy_tui_rs::{ImageAnchor, ImageId, ImageLayout, ImageProtocol};

    // A tool panel whose only output is an image ends its rows with the anchor
    // plus zero-width reservation rows. The generic trailing-blank trim must not
    // collapse them, or later transcript rows are painted over the image.
    for reserved in [1usize, 2, 16] {
        let layout = ImageLayout::new(50, reserved as u16).unwrap();
        let anchor =
            ImageAnchor::new(ImageProtocol::Kitty, ImageId::new(42).unwrap(), layout).marker();
        let mut rows = vec![
            "Read screenshot.png".to_string(),
            format!("\u{2514} {anchor}"),
        ];
        rows.extend(vec![String::new(); reserved - 1]);
        let finished = super::finish_transcript_block(rows);
        assert_eq!(
            finished.len(),
            1 + reserved,
            "reservation of {reserved} rows must survive the trailing trim"
        );
        let start = finished
            .iter()
            .position(|row| row.contains(&anchor))
            .unwrap();
        for (offset, row) in finished[start + 1..].iter().enumerate() {
            assert_eq!(visible_width(row), 0, "reserved row {offset}: {row:?}");
        }
    }

    // A block with no image still trims its decorative trailing blanks.
    assert_eq!(
        super::finish_transcript_block(vec!["done".to_string(), String::new()]),
        vec!["done".to_string()]
    );
}

#[test]
fn inline_screenshot_without_cell_report_uses_a_readable_bounded_reservation() {
    use sexy_tui_rs::{ImageDimensions, ImageLayout, ImageProtocol};

    let kitty = ImageCapabilities::forced(Some(ImageProtocol::Kitty), None);
    let screenshot = ImageDimensions::new(320, 1280).unwrap();
    let wide_screenshot = ImageDimensions::new(1600, 900).unwrap();
    for width in [46, 80] {
        let viewport = tool_image_viewport(width, kitty);
        let layout = ImageLayout::fit(screenshot, viewport).unwrap();
        // A tall portrait capture is fitted against the card's column budget and
        // stays within the row cap. It is no longer forced to the cap regardless
        // of aspect, and it is no longer squeezed to a tiny column strip: the
        // earlier (8, 16) expectation encoded the defect, not the intent.
        assert!(layout.columns() >= 1);
        assert!(layout.rows() <= MAX_TOOL_IMAGE_RENDER_ROWS);
        assert!(layout.columns() <= viewport.columns());
        // A portrait image must still be taller than it is wide.
        assert!(layout.rows() >= layout.columns() / 2);

        let wide = ImageLayout::fit(wide_screenshot, viewport).unwrap();
        assert!(wide.columns() >= 40 && wide.rows() >= 10);
        assert!(wide.columns() <= width && wide.rows() <= MAX_TOOL_IMAGE_RENDER_ROWS);
    }
}

#[test]
fn shell_backspace_atomically_removes_paste_and_attachment_chips() {
    let dir = tempfile::tempdir().unwrap();
    let mut pastes = vec!["DELETED-PASTE-PAYLOAD\n".repeat(20)];
    for name in ["image.png", "audio.wav", "document.pdf"] {
        let path = dir.path().join(name);
        std::fs::write(&path, b"fixture attachment payload").unwrap();
        pastes.push(path.display().to_string());
    }
    for paste in pastes {
        // Exercise both the chip's end and an interior editor cursor through
        // the shell's real key-action path, not the ledger helper alone.
        for interior in [false, true] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_input_modalities(
                octet_ai::ModalitySet::none()
                    .with(octet_ai::Modality::Image)
                    .with(octet_ai::Modality::Audio),
            );
            shell.apply_edit(EditAction::Paste("keep ".into()));
            shell.apply_edit(EditAction::Paste(paste.clone()));
            assert!(shell.pending().starts_with("keep ["), "{}", shell.pending());
            assert!(!shell.state.borrow().ledger.is_empty());
            if interior {
                for _ in 0..3 {
                    shell.apply_edit(EditAction::Left);
                }
            }
            {
                let mut state = shell.state.borrow_mut();
                state.slash_selection = 7;
                state.slash_scroll = 3;
                state.slash_popup_dismissed = true;
            }
            shell.apply_edit(EditAction::Backspace);
            assert_eq!(shell.pending(), "keep ");
            {
                let state = shell.state.borrow();
                assert!(state.ledger.is_empty());
                assert_eq!(state.editor.cursor(), "keep ".len());
                assert_eq!(state.slash_selection, 0);
                assert_eq!(state.slash_scroll, 0);
                assert!(!state.slash_popup_dismissed);
            }
            // Ordinary Backspace still reaches the generic editor, and the
            // revoked payload cannot be composed after its mask disappears.
            shell.apply_edit(EditAction::Backspace);
            let composed = shell.drain_composed();
            assert_eq!(composed.display_text, "keep");
            assert!(matches!(composed.parts.as_slice(),
                [octet_agent::InputPart::Text(text)] if text == "keep"));
        }
    }
}

#[test]
fn provider_retry_removes_closed_reasoning_and_text_without_removing_independent_rows() {
    for application_viewport in [false, true] {
        let (mut shell, bytes) = emulated_shell_with_mode(
            crate::tui::theme::test_theme(),
            80,
            12,
            true,
            application_viewport,
        );
        let id = shell.begin_run("test");
        shell.on_run_event(
            id,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Reasoning,
                text: "rejected reasoning\n".repeat(40),
            },
        );
        shell.on_run_event(
            id,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: "rejected answer\n".repeat(40),
            },
        );
        shell.on_run_event(
            id,
            &AgentEvent::OutputMedia {
                index: 2,
                media: octet_ai::Media::image_bytes(
                    bytes::Bytes::from_static(b"rejected-media"),
                    mime::IMAGE_PNG,
                ),
            },
        );
        shell.notice("independent notice");
        {
            let mut state = shell.state.borrow_mut();
            state.set_subagent_activity(subagent_transcript_test_view(true));
            let index = state
                .transcript
                .iter()
                .position(|block| {
                    matches!(block,
                TranscriptBlock::Notice(text) if text == "independent notice")
                })
                .unwrap();
            state.transcript_selection = Some(TranscriptSelection {
                anchor: TranscriptPosition {
                    block: index,
                    offset: 0,
                    trailing_affinity: false,
                },
                focus: TranscriptPosition {
                    block: index,
                    offset: 5,
                    trailing_affinity: false,
                },
            });
        }
        shell.render();
        bytes.lock().unwrap().clear();
        shell.on_run_event(id, &retry_event(1));
        shell.render();
        let snapshot = shell.debug_snapshot();
        assert!(!snapshot.contains("rejected"), "{snapshot}");
        assert!(snapshot.contains("independent notice"));
        {
            let state = shell.state.borrow();
            let selected = state
                .transcript_selection
                .as_ref()
                .expect("independent selection survives");
            assert!(
                matches!(&state.transcript[selected.anchor.block], TranscriptBlock::Notice(text) if text == "independent notice")
            );
            assert!(
                state.subagent_activity.is_some(),
                "independent telemetry survives"
            );
            assert!(shell_chrome(&state, 80, Instant::now())
                .subagents
                .is_empty());
            assert!(state
                .transcript
                .iter()
                .any(|block| matches!(block, TranscriptBlock::Subagents(_))));
        }
        shell.select_all_transcript();
        let copy = shell.copy_selected_plain_text().unwrap();
        assert!(!copy.contains("rejected"));
        assert!(!copy.contains("diagnostic-only"));
        assert!(copy.contains("independent notice"));
        assert_eq!(copy.matches("Subagents").count(), 1);
        assert!(!copy.contains("Read changelog"));
        if !application_viewport {
            assert!(
                bytes
                    .lock()
                    .unwrap()
                    .windows(4)
                    .any(|part| part == b"\x1b[3J"),
                "offscreen rejection must replay native saved history"
            );
        }
        shell.on_run_event(id, &retry_event(2));
        let state = shell.state.borrow();
        assert_eq!(
            state
                .transcript
                .iter()
                .filter(|block| matches!(block,
            TranscriptBlock::Reasoning(reasoning) if reasoning.retry_activity.is_some()))
                .count(),
            1
        );
        assert_eq!(state.transcript.len(), state.transcript_commit_ids.len());
        assert_eq!(state.transcript.len(), state.block_revisions.len());
    }
}
