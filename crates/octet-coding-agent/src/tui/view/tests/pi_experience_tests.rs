//! Pi presentation across built-in, direct JSON and adapter-normalized snapshots.
use super::*;
use crate::extensions::resource_paths::pi_theme;
use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
use crate::tui::theme::{compiled_theme_for_selector, test_theme_from_source};
use sexy_tui_rs::Color;

fn themes() -> Vec<OctetTheme> {
    let mut document = pi_theme::fixture("Pi presentation QA", "#00aaff");
    for (token, color) in [
        ("text", "#ddeeff"),
        ("userMessageText", "#ddeeff"),
        ("userMessageBg", "#223344"),
        ("toolPendingBg", "#182838"),
        ("toolSuccessBg", "#183828"),
        ("toolErrorBg", "#381828"),
    ] {
        document["colors"][token] = serde_json::json!(color);
    }
    let source = pi_theme::native_source(std::path::Path::new("qa.json"), &document.to_string())
        .unwrap()
        .into_owned();
    vec![
        compiled_theme_for_selector(
            "pi",
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            TerminalBackground::Dark,
        )
        .unwrap()
        .unwrap(),
        test_theme_from_source(&source),
        test_theme_from_source(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../extensions/octet-pi-compat/test/fixtures/pi-native-layout.toml"
        ))),
    ]
}

#[test]
fn existing_theme_notice_rendering_golden() {
    let capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
    let expected = [
        ("octet Default", ["", "• golden notice", ""]),
        ("Cards", ["", "  golden notice", ""]),
        ("Still", ["", "   golden notice", ""]),
        ("octet default reference", ["", "• golden notice", ""]),
    ];
    let themes = ["default", "Cards", "Still"]
        .into_iter()
        .map(|name| {
            compiled_theme_for_selector(name, capabilities, TerminalBackground::Dark)
                .unwrap()
                .unwrap()
        })
        .chain(std::iter::once(test_theme_from_source(include_str!(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../examples/themes/octet-default.toml"
            )
        ))));
    for (theme, (expected_name, expected_rows)) in themes.zip(expected) {
        assert_eq!(theme.metadata().name, expected_name);
        let renderer = theme.rich_renderer();
        let rows = render_block(
            None,
            &TranscriptBlock::Notice("golden notice".into()),
            &theme,
            &renderer,
            &renderer,
            40,
            false,
        );
        let plain = rows
            .iter()
            .map(|row| strip_terminal_sequences(row))
            .collect::<Vec<_>>();
        assert_eq!(plain, expected_rows, "{}", theme.metadata().name);
    }
}

#[test]
fn pi_prompt_is_a_three_row_full_width_band_without_a_chevron_or_model_wash() {
    for theme in themes() {
        assert!(theme.is_pi_theme());
        let renderer = theme.rich_renderer();
        let block = TranscriptBlock::User {
            text: "Hello **Pi**".into(),
            model_lab: Some(ModelLab::Anthropic),
            prompt_color: Some("#ff0000".into()),
            persisted: false,
        };
        for width in [20, 80, 160] {
            let rows = render_block(None, &block, &theme, &renderer, &renderer, width, false);
            assert_eq!(rows.len(), 3, "{rows:?}");
            let content = strip_terminal_sequences(&rows[1]);
            assert!(content.starts_with(" Hello Pi"), "{content:?}");
            assert!(!content.contains(theme.glyph("prompt")));
            let expected = theme
                .semantic_style("extension.pi.userMessageBg")
                .background;
            for row in &rows {
                let parser = surface_frame::emulate_row_for_test(row, width);
                for column in 0..width {
                    let color = parser.screen().cell(0, column).unwrap().bgcolor();
                    if let Color::Rgb(r, g, b) = expected {
                        assert_eq!(color, vt100::Color::Rgb(r, g, b));
                    }
                }
            }
            assert!(
                rows[1].contains("\x1b[1m"),
                "Markdown emphasis survives band painting"
            );
        }
    }
}

#[test]
fn pi_tool_lifecycle_selects_pending_success_and_error_fills_before_framing() {
    for theme in themes() {
        let renderer = theme.rich_renderer();
        for (finished, is_error, token) in [
            (false, false, "toolPendingBg"),
            (true, false, "toolSuccessBg"),
            (true, true, "toolErrorBg"),
        ] {
            let args = serde_json::json!({"command":"printf result"});
            let block = TranscriptBlock::Tool(Box::new(ToolPanel::new(
                ToolCallId("pi-bash".into()),
                "bash".into(),
                args.to_string(),
                summarize_tool("bash", &args),
                "result".into(),
                finished,
                is_error,
                None,
                None,
            )));
            let expected = theme
                .semantic_style(&format!("extension.pi.{token}"))
                .background;
            let role = match token {
                "toolPendingBg" => "surface.tool.pending",
                "toolSuccessBg" => "surface.tool.success",
                _ => "surface.tool.error",
            };
            let plan = compile_surface_plan(None, &block, &theme, 80);
            assert_eq!(plan.content_role, role);
            let rows = render_block(None, &block, &theme, &renderer, &renderer, 80, false);
            for row in rows.iter().filter(|row| !row.is_empty()) {
                let parser = surface_frame::emulate_row_for_test(row, 80);
                for column in [0, 1, 79] {
                    if let Color::Rgb(r, g, b) = expected {
                        assert_eq!(
                            parser.screen().cell(0, column).unwrap().bgcolor(),
                            vt100::Color::Rgb(r, g, b)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn pi_working_spinner_advances_without_label_shimmer_and_stops_on_settlement() {
    for theme in themes() {
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        shell.set_identity("test", "test-model", "max");
        let run_id = shell.begin_run("test");
        let mut state = shell.state.borrow_mut();
        assert!(state.has_active_thinking_spinner());
        assert!(!state.has_active_status_shimmer());
        let raw = state.rendered_transcript(80).join("\n");
        let plain = strip_terminal_sequences(&raw);
        assert!(
            plain.lines().any(|row| row.starts_with(" ⠋ Working")),
            "{plain:?}"
        );
        let label = raw.split_once("Working").unwrap().1.to_owned();
        state.advance_thinking_spinner(1);
        let next = state.rendered_transcript(80).join("\n");
        assert!(
            strip_terminal_sequences(&next).contains("⠙ Working"),
            "{next:?}"
        );
        assert_eq!(next.split_once("Working").unwrap().1, label);
        assert_eq!(state.status_shimmer_frame, 0);
        drop(state);
        shell.fail_run(run_id, "fixture ended");
        assert!(!shell.state.borrow().has_active_thinking_spinner());
    }
}

#[test]
fn pi_extension_working_frames_honor_disabled_host_animation() {
    let source = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../extensions/octet-pi-compat/test/fixtures/pi-native-layout.toml"
    ));
    for animation in [true, false] {
        let mut capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
        capabilities.animation = animation;
        let theme = crate::tui::theme::test_theme_source_with(
            source,
            capabilities,
            TerminalBackground::Dark,
        );
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        shell.begin_run("test");
        shell.set_extension_ui(ShellExtensionUi {
            working: Some(ShellExtensionWorking {
                frames: Some(vec!["X".into(), "Y".into()]),
                interval_ms: Some(60_000),
                ..Default::default()
            }),
            ..Default::default()
        });
        let rows =
            strip_terminal_sequences(&shell.state.borrow_mut().rendered_transcript(80).join("\n"));
        assert!(rows.contains("Working"), "{rows}");
        assert_eq!(rows.contains("X Working"), animation, "{rows}");
    }
}

#[test]
fn pi_palette_keeps_exact_accent_and_explicit_default_backgrounds_after_model_changes() {
    for mut theme in themes() {
        let accent = theme.role_rgb("model_accent");
        for lab in [ModelLab::Anthropic, ModelLab::OpenAi, ModelLab::Unknown] {
            crate::tui::theme::apply_model_lab(&mut theme, lab);
            assert_eq!(theme.model_rgb(Some(lab)), accent);
        }
        for role in ["surface.assistant", "surface.reasoning"] {
            assert_eq!(theme.semantic_style(role).background, Color::Default);
        }
    }
}

#[test]
fn pi_terminal_default_backgrounds_are_not_replaced_by_native_fallback_colors() {
    let mut document = pi_theme::fixture("Pi default backgrounds", "#00aaff");
    let tokens = [
        "selectedBg",
        "searchMatchBg",
        "userMessageBg",
        "customMessageBg",
        "toolPendingBg",
        "toolSuccessBg",
        "toolErrorBg",
    ];
    for token in tokens {
        document["colors"][token] = serde_json::json!("");
    }
    let source = pi_theme::native_source(std::path::Path::new("qa.json"), &document.to_string())
        .unwrap()
        .into_owned();
    let mut adapter = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../extensions/octet-pi-compat/test/fixtures/pi-native-layout.toml"
    ))
    .to_owned();
    for color in ["#00aaff", "#223344", "#182838", "#183828", "#381828"] {
        adapter = adapter.replace(
            &format!("background = \"{color}\""),
            "background = \"default\"",
        );
    }
    for source in [source, adapter] {
        let shell = InteractiveShell::test_shell_with_theme(test_theme_from_source(&source));
        let palette = shell.extension_theme_get(None).unwrap();
        for token in tokens {
            assert_eq!(palette["backgrounds"][token], "", "{token}");
        }
    }
}

#[test]
fn pi_simple_extension_footer_replaces_native_metadata_and_withdrawal_restores_it() {
    for theme in themes() {
        let mut shell = InteractiveShell::test_shell_with_theme(theme);
        shell.set_identity("test", "Stock model", "off");
        shell.set_extension_ui(ShellExtensionUi {
            footer: vec![ShellExtensionUiLine {
                text: "Reviewed Pi footer".into(),
                style_role: None,
                priority: 0,
            }],
            ..Default::default()
        });
        let state = shell.state.borrow();
        let chrome = shell_chrome(&state, 80, Instant::now());
        assert!(!chrome.composer.join("\n").contains("Stock model"));
        assert!(chrome
            .extension_below
            .join("\n")
            .contains("Reviewed Pi footer"));
        drop(state);
        shell.set_extension_ui(Default::default());
        let state = shell.state.borrow();
        assert!(shell_chrome(&state, 80, Instant::now())
            .composer
            .join("\n")
            .contains("Stock model"));
    }
}

#[test]
fn pi_themes_stream_thinking_expanded_and_the_default_theme_collapses_it() {
    for theme in themes() {
        assert_eq!(
            theme.resolve::<bool>("thinking_expanded"),
            Some(true),
            "every Pi source (built-in, native projection, adapter output) expands thinking"
        );
    }
    let default = compiled_theme_for_selector(
        crate::tui::theme::DEFAULT_THEME_NAME,
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Dark,
    )
    .unwrap()
    .unwrap();
    assert_ne!(default.resolve::<bool>("thinking_expanded"), Some(true));
}

fn row_containing<'a>(rows: &'a [String], needle: &str) -> &'a str {
    rows.iter()
        .find(|row| strip_terminal_sequences(row).contains(needle))
        .unwrap_or_else(|| panic!("missing {needle:?} in {rows:?}"))
}

fn assert_content_cell(rows: &[String], needle: &str, column: u16, italic: bool, width: u16) {
    let row = row_containing(rows, needle);
    let plain = strip_terminal_sequences(row);
    assert_eq!(plain.find(needle), Some(usize::from(column)), "{plain:?}");
    let parser = surface_frame::emulate_row_for_test(row, width);
    assert_eq!(
        parser.screen().cell(0, column).unwrap().italic(),
        italic,
        "{plain:?}"
    );
}

#[test]
fn pi_sources_render_uniform_transcript_padding_italic_reasoning_and_list_bullets() {
    for theme in themes() {
        assert_eq!(theme.layout().block_padding_x, Some(1));
        assert!(theme.semantic_style("thinking").attributes.italic);
        assert_eq!(theme.glyph("list_bullet"), "-");
        for width in [40, 80, 160] {
            let rich = theme.rich_renderer();
            let thinking = theme.reasoning_renderer();
            let render = |block: &TranscriptBlock| {
                render_block(None, block, &theme, &rich, &thinking, width, true)
            };
            let reasoning = TranscriptBlock::Reasoning(Box::new(
                AssistantBlock::finalized_reasoning("Plan\n\n- item".into())
                    .with_reasoning_expanded(true),
            ));
            let rows = render(&reasoning);
            assert_content_cell(&rows, "Plan", 1, true, width);
            assert_content_cell(&rows, "- item", 1, true, width);
            let assistant = TranscriptBlock::Assistant(Box::new(AssistantBlock::finalized(
                "Answer\n\n- item".into(),
            )));
            let rows = render(&assistant);
            assert_content_cell(&rows, "Answer", 1, false, width);
            assert_content_cell(&rows, "- item", 1, false, width);
            let user = TranscriptBlock::User {
                text: "Prompt".into(),
                model_lab: None,
                prompt_color: None,
                persisted: true,
            };
            assert_content_cell(&render(&user), "Prompt", 1, false, width);
            let notice = TranscriptBlock::Notice("Notice".into());
            assert_content_cell(&render(&notice), "Notice", 1, false, width);
            let outcome = TranscriptBlock::Outcome(OutcomeBlock::new(
                RunOutcome::Interrupted {
                    elapsed: Duration::from_secs(1),
                },
                None,
            ));
            let compaction = TranscriptBlock::Compaction(Box::new(CompactionBlock {
                label: "Context compacted".into(),
                summary: "A compacted summary".into(),
                expanded: true,
            }));
            for block in [&outcome, &compaction] {
                let plan = compile_surface_plan(None, block, &theme, width);
                assert_eq!(plan.geometry.content_left, 1);
                let first = render(block)
                    .iter()
                    .map(|row| strip_terminal_sequences(row))
                    .find(|row| !row.trim().is_empty())
                    .expect("outcome or compaction header");
                assert!(
                    first.starts_with(' ') && !first.starts_with("  "),
                    "{first:?}"
                );
            }
            assert_content_cell(&render(&compaction), "A compacted summary", 3, false, width);
            let args = serde_json::json!({"path":"file.txt"});
            let tool = TranscriptBlock::Tool(Box::new(ToolPanel::new(
                ToolCallId("pi-read".into()),
                "read".into(),
                args.to_string(),
                summarize_tool("read", &args),
                "result".into(),
                true,
                false,
                None,
                None,
            )));
            let plan = compile_surface_plan(None, &tool, &theme, width);
            assert_eq!(plan.geometry.content_left, 1);
            let rows = render(&tool);
            let first = rows
                .iter()
                .map(|row| strip_terminal_sequences(row))
                .find(|row| !row.trim().is_empty())
                .expect("tool header");
            assert_eq!(first.len() - first.trim_start().len(), 1, "{first:?}");
            let args = serde_json::json!({"command":"printf fixture"});
            let bash = TranscriptBlock::Tool(Box::new(ToolPanel::new(
                ToolCallId("pi-bash-body".into()),
                "bash".into(),
                args.to_string(),
                summarize_tool("bash", &args),
                "body output".into(),
                true,
                false,
                None,
                None,
            )));
            let rows = render(&bash);
            assert_content_cell(&rows, "body output", 1, false, width);
        }
    }
}

#[test]
fn custom_themes_render_overrides_and_keep_other_themes_upright() {
    for (padding, italic, bullet) in [(3, true, '*'), (0, false, '+')] {
        let source = format!(
            r#"
            [metadata]
            profile = "pi"
            [layout]
            block_padding_x = {padding}
            [roles.thinking]
            italic = {italic}
            [glyphs]
            list_bullet = "{bullet}"
        "#
        );
        let theme = test_theme_from_source(&source);
        let rich = theme.rich_renderer();
        let thinking = theme.reasoning_renderer();
        let block = TranscriptBlock::Reasoning(Box::new(
            AssistantBlock::finalized_reasoning("Plan\n\n- item".into())
                .with_reasoning_expanded(true),
        ));
        let rows = render_block(None, &block, &theme, &rich, &thinking, 40, true);
        assert_content_cell(&rows, "Plan", padding, italic, 40);
        assert_content_cell(&rows, &format!("{bullet} item"), padding, italic, 40);
    }
    let theme = test_theme_from_source(
        r##"
        [colors]
        margin_markers = false
        [layout]
        transcript_inset = 0
        block_padding_x = 3
        [surfaces.reasoning]
        marker_gutter = false
        [roles.thinking]
        foreground = "#123456"
        italic = true
        bold = true
    "##,
    );
    let block = TranscriptBlock::Reasoning(Box::new(
        AssistantBlock::finalized_reasoning("Plan".into()).with_reasoning_expanded(true),
    ));
    let rows = render_block(
        None,
        &block,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        80,
        true,
    );
    assert_content_cell(&rows, "Plan", 3, true, 80);
    let parser = surface_frame::emulate_row_for_test(row_containing(&rows, "Plan"), 80);
    let cell = parser.screen().cell(0, 3).unwrap();
    assert!(cell.bold());
    assert_eq!(cell.fgcolor(), vt100::Color::Rgb(0x12, 0x34, 0x56));
    for name in ["default", "Cards", "Still"] {
        let existing = compiled_theme_for_selector(
            name,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            TerminalBackground::Dark,
        )
        .unwrap()
        .unwrap();
        let output = AssistantBlock::finalized_reasoning("Plan".into()).render(
            &existing.reasoning_renderer(),
            &existing,
            80,
        );
        let parser = surface_frame::emulate_row_for_test(row_containing(&output, "Plan"), 80);
        assert!(!parser.screen().cell(0, 0).unwrap().italic(), "{name}");
        let output = AssistantBlock::finalized("- item".into()).render(
            &existing.rich_renderer(),
            &existing,
            80,
        );
        assert!(
            strip_terminal_sequences(&output.join("\n")).contains("— item"),
            "{name}"
        );
    }
}

#[test]
fn scroll_hint_padding_and_theme_replacement_follow_theme_data() {
    let pi = themes().remove(0);
    let default = compiled_theme_for_selector(
        "default",
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Dark,
    )
    .unwrap()
    .unwrap();
    let mut shell = InteractiveShell::test_shell_with_theme(default);
    shell.set_size(80, 12);
    for index in 0..40 {
        shell.notice(format!("history {index}"));
    }
    shell.scroll(-1);
    let hint = |shell: &InteractiveShell| {
        let rows = render_shell_viewport_at(&shell.state.borrow(), 80, Instant::now());
        strip_terminal_sequences(row_containing(&rows, "rows back"))
    };
    assert!(hint(&shell).starts_with("↑ "));
    shell.set_theme(pi);
    assert!(hint(&shell).starts_with(" ↑ "));
    let ascii = crate::tui::theme::test_theme_source_with(
        "[metadata]\nprofile = 'pi'\n",
        TerminalCapabilities::test(true, false, ColorDepth::TrueColor),
        TerminalBackground::Dark,
    );
    shell.set_theme(ascii);
    assert!(hint(&shell).starts_with(" ^ "));
}

#[test]
fn legacy_pi_snapshots_keep_explicit_spacing_without_silently_applying_the_new_profile() {
    let source = r##"
        [metadata]
        name = "Legacy Pi"
        [colors]
        margin_markers = false
        tool_title = "#123456"
        [layout]
        transcript_inset = 0
        [roles."extension.pi.accent"]
        foreground = "#123456"
        [roles."extension.pi.toolSuccessBg"]
        background = "#142536"
        [surfaces.user]
        chrome = "band"
        padding = 1
        narrow_chrome = "band"
        narrow_padding = 1
        [surfaces.reasoning]
        padding = 0
        [surfaces.tool]
        chrome = "band"
        padding = 1
        narrow_chrome = "band"
        narrow_padding = 1
    "##;
    let theme = test_theme_from_source(source);
    assert_eq!(theme.layout().block_padding_x, None);
    assert!(!theme.semantic_style("thinking").attributes.italic);
    assert!(!theme.surface_for_width("user", 80).prompt_marker);
    let reasoning = TranscriptBlock::Reasoning(Box::new(
        AssistantBlock::finalized_reasoning("Plan".into()).with_reasoning_expanded(true),
    ));
    let rows = render_block(
        None,
        &reasoning,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        80,
        true,
    );
    assert_content_cell(&rows, "Plan", 0, false, 80);
    assert_eq!(
        theme.semantic_style("surface.tool.success").background,
        Color::Rgb(0x14, 0x25, 0x36)
    );
    for kind in ["tool", "shell"] {
        let partial = format!("{source}\n[roles.\"surface.{kind}.success\"]\nbold = true\n");
        let partial = test_theme_from_source(&partial);
        let style = partial.semantic_style(&format!("surface.{kind}.success"));
        assert_eq!(style.background, Color::Rgb(0x14, 0x25, 0x36));
        assert!(style.attributes.bold);
        let explicit_bg =
            format!("{source}\n[roles.\"surface.{kind}.success\"]\nbackground = '#abcdef'\n");
        let explicit_bg = test_theme_from_source(&explicit_bg);
        assert_eq!(
            explicit_bg
                .semantic_style(&format!("surface.{kind}.success"))
                .background,
            Color::Rgb(0xab, 0xcd, 0xef)
        );
    }
    let explicit = source.replace(
        "name = \"Legacy Pi\"",
        "name = \"Legacy Pi\"\nprofile = \"octet\"",
    );
    let explicit = test_theme_from_source(&explicit);
    assert!(explicit.surface_for_width("user", 80).prompt_marker);
    assert!(!explicit.has_semantic_style("surface.tool.success"));
}

// Use a filled surface to exercise the SGR reset/reopen boundary around the
// header, its adjacent value, and the trailing canvas, not just role lookup.
fn inherited_style_fixture(foreground_role: &str) -> String {
    format!(
        r##"
        [colors]
        margin_markers = false
        [layout]
        transcript_inset = 0
        [surfaces.tool]
        chrome = "band"
        padding = 1
        marker_gutter = false
        [roles."surface.tool"]
        foreground = "#112233"
        background = "#334455"
        [roles.{foreground_role}]
        foreground = "#123456"
        background = "#234567"
        bold = false
        italic = true
        [roles.muted]
        foreground = "#abcdef"
        background = "#345678"
        bold = true
        italic = true
        dim = false
        "##
    )
}

fn rendered_read_header(theme: &OctetTheme) -> String {
    let args = serde_json::json!({"path":"file.txt"});
    let tool = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("style-read".into()),
        "read".into(),
        args.to_string(),
        summarize_tool("read", &args),
        "result".into(),
        true,
        false,
        None,
        None,
    )));
    let rows = render_block(
        None,
        &tool,
        theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        80,
        true,
    );
    row_containing(&rows, "Read").to_owned()
}

fn rendered_scroll_hint(theme: &OctetTheme) -> String {
    let mut shell = InteractiveShell::test_shell_with_theme(theme.clone());
    shell.set_size(80, 12);
    for index in 0..40 {
        shell.notice(format!("history {index}"));
    }
    shell.scroll(-1);
    let rows = render_shell_viewport_at(&shell.state.borrow(), 80, Instant::now());
    row_containing(&rows, "rows back").to_owned()
}

fn assert_rendered_style(
    row: &str,
    needle: &str,
    foreground: vt100::Color,
    background: vt100::Color,
    bold: bool,
    italic: bool,
) {
    let plain = strip_terminal_sequences(row);
    let prefix = plain.split_once(needle).expect("styled text is present").0;
    let column = sexy_tui_rs::visible_width(prefix) as u16;
    let parser = surface_frame::emulate_row_for_test(row, 100);
    let cell = parser.screen().cell(0, column).unwrap();
    assert_eq!(cell.fgcolor(), foreground, "{row:?}");
    assert_eq!(cell.bgcolor(), background, "{row:?}");
    assert_eq!(cell.bold(), bold, "{row:?}");
    assert_eq!(cell.italic(), italic, "{row:?}");
}

fn assert_row_does_not_bleed(row: &str) {
    let column = sexy_tui_rs::visible_width(row) as u16;
    let parser = surface_frame::emulate_row_for_test(&format!("{row}Z"), 100);
    let cell = parser.screen().cell(0, column).unwrap();
    assert_eq!(cell.fgcolor(), vt100::Color::Default, "{row:?}");
    assert_eq!(cell.bgcolor(), vt100::Color::Default, "{row:?}");
    assert!(!cell.bold(), "{row:?}");
    assert!(!cell.italic(), "{row:?}");
}

#[test]
fn legacy_foreground_and_muted_attributes_reach_the_rendered_header_and_scroll_hint() {
    for foreground_role in ["foreground", "text"] {
        let theme = test_theme_from_source(&inherited_style_fixture(foreground_role));
        let header = rendered_read_header(&theme);
        assert_rendered_style(
            &header,
            "Read",
            vt100::Color::Rgb(0x12, 0x34, 0x56),
            vt100::Color::Rgb(0x23, 0x45, 0x67),
            true,
            true,
        );
        assert_rendered_style(
            &header,
            "file.txt",
            vt100::Color::Rgb(0xab, 0xcd, 0xef),
            vt100::Color::Rgb(0x34, 0x56, 0x78),
            true,
            true,
        );
        let parser = surface_frame::emulate_row_for_test(&header, 100);
        let plain = strip_terminal_sequences(&header);
        let gap = sexy_tui_rs::visible_width(plain.split_once("Read").unwrap().0) as u16 + 4;
        let gap = parser.screen().cell(0, gap).unwrap();
        assert_eq!(gap.bgcolor(), vt100::Color::Rgb(0x33, 0x44, 0x55));
        assert!(!gap.bold());
        assert!(!gap.italic());
        assert_row_does_not_bleed(&header);

        let hint = rendered_scroll_hint(&theme);
        assert_rendered_style(
            &hint,
            "rows back",
            vt100::Color::Rgb(0xab, 0xcd, 0xef),
            vt100::Color::Rgb(0x34, 0x56, 0x78),
            true,
            true,
        );
        assert_row_does_not_bleed(&hint);
    }
}

#[test]
fn explicit_transcript_roles_override_inherited_attributes_after_aliases_compile() {
    // `text` sorts after `scroll_hint` and `tool_header` in the parsed map.
    let source = format!(
        "{}{}",
        inherited_style_fixture("text"),
        r##"
        [roles.tool_header]
        foreground = "#fedcba"
        bold = false
        [roles.scroll_hint]
        foreground = "#a1b2c3"
        background = "#456789"
        bold = false
        italic = false
        "##
    );
    let theme = test_theme_from_source(&source);
    let header = rendered_read_header(&theme);
    assert_rendered_style(
        &header,
        "Read",
        vt100::Color::Rgb(0xfe, 0xdc, 0xba),
        vt100::Color::Rgb(0x23, 0x45, 0x67),
        false,
        true,
    );
    assert_row_does_not_bleed(&header);
    let hint = rendered_scroll_hint(&theme);
    assert_rendered_style(
        &hint,
        "rows back",
        vt100::Color::Rgb(0xa1, 0xb2, 0xc3),
        vt100::Color::Rgb(0x45, 0x67, 0x89),
        false,
        false,
    );
    assert_row_does_not_bleed(&hint);
}

#[test]
fn pi_tool_title_inheritance_and_explicit_header_overrides_preserve_attributes() {
    let base = format!(
        "{}{}",
        inherited_style_fixture("foreground"),
        r##"
        [roles."extension.pi.accent"]
        foreground = "#123456"
        [roles.tool_title]
        foreground = "#789abc"
        background = "#456789"
        bold = false
        italic = true
        "##
    );
    for profile in ["", "[metadata]\nprofile = 'pi'\n"] {
        let source = format!("{profile}{base}");
        let theme = test_theme_from_source(&source);
        let header = rendered_read_header(&theme);
        assert_rendered_style(
            &header,
            "Read",
            vt100::Color::Rgb(0x78, 0x9a, 0xbc),
            vt100::Color::Rgb(0x45, 0x67, 0x89),
            true,
            true,
        );
        assert_row_does_not_bleed(&header);

        let source =
            format!("{source}\n[roles.tool_header]\nforeground = '#cba987'\nbold = false\n");
        let theme = test_theme_from_source(&source);
        let header = rendered_read_header(&theme);
        assert_rendered_style(
            &header,
            "Read",
            vt100::Color::Rgb(0xcb, 0xa9, 0x87),
            vt100::Color::Rgb(0x45, 0x67, 0x89),
            false,
            true,
        );
        assert_row_does_not_bleed(&header);
    }
}

#[test]
fn custom_tool_state_and_header_styles_are_theme_driven() {
    let theme = test_theme_from_source(
        r##"
        [metadata]
        profile = "pi"
        [roles.tool_header]
        foreground = "#123456"
        bold = false
        italic = true
        [roles."surface.tool"]
        foreground = "#abcdef"
        background = "#222222"
        [roles."surface.tool.success"]
        background = "#445566"
    "##,
    );
    let args = serde_json::json!({"path":"file.txt"});
    let tool = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("custom-read".into()),
        "read".into(),
        args.to_string(),
        summarize_tool("read", &args),
        "result".into(),
        true,
        false,
        None,
        None,
    )));
    let plan = compile_surface_plan(None, &tool, &theme, 80);
    assert_eq!(plan.content_role, "surface.tool.success");
    let state = theme.semantic_style(plan.content_role);
    assert_eq!(state.foreground, Color::Rgb(0xab, 0xcd, 0xef));
    assert_eq!(state.background, Color::Rgb(0x44, 0x55, 0x66));
    let rows = render_block(
        None,
        &tool,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        80,
        true,
    );
    let row = rows
        .iter()
        .find(|row| !strip_terminal_sequences(row).trim().is_empty())
        .unwrap();
    let parser = surface_frame::emulate_row_for_test(row, 80);
    let cell = parser.screen().cell(0, 1).unwrap();
    assert_eq!(cell.fgcolor(), vt100::Color::Rgb(0x12, 0x34, 0x56));
    assert!(!cell.bold());
    assert!(cell.italic());
}

#[test]
fn state_styles_inherit_fill_but_border_and_label_styles_remain_independent() {
    let theme = test_theme_from_source(
        r##"
        [roles."surface.tool"]
        background = "#112233"
        bold = true
        [roles."surface.tool.success"]
        italic = true
        [roles."surface.tool.border"]
        foreground = "#445566"
        [roles."surface.tool.label"]
        foreground = "#778899"
        [roles."surface.shell"]
        background = "#112233"
        bold = true
        [roles."surface.shell.error"]
        italic = true
        [roles."surface.shell.border"]
        foreground = "#445566"
        [roles."surface.shell.label"]
        foreground = "#778899"
        "##,
    );
    for state in ["surface.tool.success", "surface.shell.error"] {
        let style = theme.semantic_style(state);
        assert_eq!(style.background, Color::Rgb(0x11, 0x22, 0x33), "{state}");
        assert!(style.attributes.bold && style.attributes.italic, "{state}");
    }
    for role in [
        "surface.tool.border",
        "surface.tool.label",
        "surface.shell.border",
        "surface.shell.label",
    ] {
        let style = theme.semantic_style(role);
        assert_eq!(style.background, Color::Default, "{role}");
        assert!(!style.attributes.bold, "{role}");
    }
    let cards = compiled_theme_for_selector(
        "Cards",
        TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
        TerminalBackground::Dark,
    )
    .unwrap()
    .unwrap();
    for role in ["surface.tool.border", "surface.shell.border"] {
        assert_eq!(
            cards.semantic_style(role).background,
            Color::Default,
            "{role}"
        );
    }
}

#[test]
fn disabling_marker_gutter_does_not_shift_or_clip_content() {
    let theme = test_theme_from_source(
        "[layout]\ntranscript_inset = 0\n[surfaces.notice]\nmarker_gutter = false\n",
    );
    let notice = TranscriptBlock::Notice("abcdefghij".into());
    let rows = render_block(
        None,
        &notice,
        &theme,
        &theme.rich_renderer(),
        &theme.reasoning_renderer(),
        10,
        false,
    );
    let row = strip_terminal_sequences(row_containing(&rows, "abcdefghij"));
    assert_eq!(row, "abcdefghij");
}

#[test]
fn wide_tool_indent_preserves_output_in_narrow_content() {
    let theme = test_theme_from_source(
        "[layout]\ntranscript_inset = 0\nblock_padding_x = 4\ntool_body_indent = 4\n\
         [surfaces.tool]\nmarker_gutter = false\n\
         [surfaces.shell]\nmarker_gutter = false\n",
    );
    let args = serde_json::json!({"command":"printf A"});
    let tool = TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("narrow-output".into()),
        "bash".into(),
        args.to_string(),
        summarize_tool("bash", &args),
        "A".into(),
        true,
        false,
        None,
        None,
    )));
    let shell = TranscriptBlock::Shell(Box::new(ShellOutput {
        id: "narrow-shell".into(),
        command: "printf A".into(),
        output: "A".into(),
        exit_code: 0,
        running: false,
    }));
    for block in [&tool, &shell] {
        let plan = compile_surface_plan(None, block, &theme, 12);
        assert_eq!(plan.geometry.content_width, 4);
        let rows = render_block(
            None,
            block,
            &theme,
            &theme.rich_renderer(),
            &theme.reasoning_renderer(),
            12,
            true,
        );
        assert!(
            rows.iter()
                .any(|row| strip_terminal_sequences(row).contains('A')),
            "output disappeared: {rows:?}"
        );
    }
}

#[test]
fn activity_surface_roles_override_reasoning_and_fall_back_when_unset() {
    let activity =
        TranscriptBlock::Reasoning(Box::new(AssistantBlock::streaming_reasoning("private")));
    for (extra, role, bg) in [
        ("", "surface.reasoning", vt100::Color::Rgb(0x11, 0x22, 0x33)),
        (
            "[roles.\"surface.activity\"]\nbackground = '#445566'\n",
            "surface.activity",
            vt100::Color::Rgb(0x44, 0x55, 0x66),
        ),
    ] {
        let theme = test_theme_from_source(&format!(
            "[surfaces.activity]\nchrome = 'band'\npadding = 1\n[roles.\"surface.reasoning\"]\nbackground = '#112233'\n{extra}"
        ));
        let plan = compile_surface_plan(None, &activity, &theme, 80);
        assert_eq!(plan.content_role, role);
        let rows = render_block(
            None,
            &activity,
            &theme,
            &theme.rich_renderer(),
            &theme.reasoning_renderer(),
            80,
            false,
        );
        let row = rows
            .iter()
            .find(|row| !strip_terminal_sequences(row).trim().is_empty())
            .expect("activity row");
        let parser = surface_frame::emulate_row_for_test(row, 80);
        assert_eq!(
            parser
                .screen()
                .cell(0, plan.geometry.content_left)
                .unwrap()
                .bgcolor(),
            bg
        );
    }
}

#[test]
fn standalone_pi_profile_uses_static_activity_and_inherits_tool_title_attributes() {
    let source =
        "[metadata]\nprofile = 'pi'\n[roles.tool_title]\nforeground = '#123456'\nitalic = true\n";
    let theme = test_theme_from_source(source);
    assert!(
        !theme.is_pi_theme(),
        "profile is distinct from the Pi extension palette"
    );
    assert!(theme.semantic_style("tool_header").attributes.italic);
    let mut shell = InteractiveShell::test_shell_with_theme(theme);
    shell.begin_run("test");
    assert!(!shell.state.borrow().has_active_status_shimmer());
    let override_source = format!("{source}\n[colors]\nactivity_shimmer = true\n");
    let mut shell =
        InteractiveShell::test_shell_with_theme(test_theme_from_source(&override_source));
    shell.begin_run("test");
    assert!(shell.state.borrow().has_active_status_shimmer());
}

#[test]
fn explicit_tool_header_resets_override_surface_without_bleeding() {
    let theme = test_theme_from_source(
        r##"
        [colors]
        margin_markers = false
        [layout]
        transcript_inset = 0
        [surfaces.tool]
        chrome = "band"
        padding = 1
        marker_gutter = false
        [roles."surface.tool"]
        foreground = "#112233"
        background = "#334455"
        bold = true
        italic = true
        [roles.tool_header]
        foreground = "default"
        background = "default"
        bold = false
        italic = false
        "##,
    );
    let header = rendered_read_header(&theme);
    assert_rendered_style(
        &header,
        "Read",
        vt100::Color::Default,
        vt100::Color::Default,
        false,
        false,
    );
    let plain = strip_terminal_sequences(&header);
    let gap = sexy_tui_rs::visible_width(plain.split_once("Read").unwrap().0) as u16 + 4;
    let parser = surface_frame::emulate_row_for_test(&header, 100);
    let cell = parser.screen().cell(0, gap).unwrap();
    assert_eq!(cell.bgcolor(), vt100::Color::Rgb(0x33, 0x44, 0x55));
    assert!(cell.bold());
    assert!(cell.italic());
    assert_row_does_not_bleed(&header);
}
