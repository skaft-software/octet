use super::*;
use std::sync::Arc;

use crate::tui::extension_components::ExtensionComponentSurface;
use crate::tui::view::{OrdinarySurfaceMetadata, Panel, PanelAction};

fn projection(size: (u16, u16), entries: &[(Placement, &str)]) -> Projection {
    let mut components = ExtensionComponentSurface::default();
    let mut mounts = Vec::new();
    for (index, (placement, line)) in entries.iter().enumerate() {
        let id = format!("region.{index}");
        components.register_component(&id).unwrap();
        components
            .store_render(&id, size.0, vec![(*line).into()])
            .unwrap();
        mounts.push(MountView {
            surface_id: "test-surface".into(),
            id,
            title: "x".repeat(256),
            placement: *placement,
            columns: size.0,
            rows: size.1,
            mouse_capture: false,
            native_editor: false,
        });
    }
    Projection {
        components: Arc::new(components),
        mounts,
        chrome: None,
    }
}

#[test]
fn remote_ui_composes_editor_widgets_and_replacing_footer_without_rpc() {
    for size in [(30, 10), (80, 24), (120, 40)] {
        let mut state = ShellState {
            size,
            ..ShellState::default()
        };
        state.extension_ui.remote = projection(
            size,
            &[
                (Placement::AboveEditor, "above"),
                (Placement::BelowEditor, "below"),
                (Placement::Footer, "\x1b[31mfooter\x1b[0m"),
                (Placement::Editor, "remote editor"),
            ],
        );
        let chrome = shell_chrome(&state, size.0, Instant::now());
        assert!(chrome
            .composer
            .iter()
            .any(|line| line.contains("remote editor")));
        assert_eq!(chrome.composer, vec!["remote editor\x1b[0m"]);
        assert!(chrome
            .extension_above
            .iter()
            .any(|line| line.contains("above")));
        assert!(chrome
            .extension_below
            .iter()
            .any(|line| line.contains("below")));
        assert!(chrome
            .extension_below
            .last()
            .unwrap()
            .contains("\x1b[31mfooter"));
        for line in chrome
            .composer
            .iter()
            .chain(&chrome.extension_above)
            .chain(&chrome.extension_below)
        {
            assert!(sexy_tui_rs::visible_width(line) <= usize::from(size.0));
        }
        assert!(chrome.transcript_rows > 0);
    }
}

#[test]
fn cached_component_styles_and_title_survive_native_theme_and_model_switches() {
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use crate::tui::theme::{compiled_theme_for_selector, TerminalBackground};

    let mut shell = super::super::InteractiveShell::test_shell();
    let size = shell.terminal_dimensions();
    let entries = [
        (
            Placement::Header,
            "\x1b[38;2;1;2;3;48;2;4;5;6mheader\x1b[0m",
        ),
        (
            Placement::Editor,
            "\x1b[38;2;7;8;9;48;2;10;11;12meditor\x1b[0m",
        ),
        (Placement::AboveEditor, "\x1b[35;44mabove\x1b[0m"),
        (Placement::BelowEditor, "\x1b[32;45mbelow\x1b[0m"),
        (Placement::Footer, "\x1b[1;33;46mfooter\x1b[0m"),
    ];
    let mut remote = projection(size, &entries);
    remote.chrome = Some(crate::extensions::remote_ui::ChromeState {
        title: Some("Pi-owned title".into()),
        ..Default::default()
    });
    shell.prefill_editor("kept native draft".into());
    shell.set_remote_ui(remote);
    for selector in ["dark", "pi", "Cards", "Still", "dark"] {
        let theme = compiled_theme_for_selector(
            selector,
            TerminalCapabilities::test(true, true, ColorDepth::TrueColor),
            TerminalBackground::Dark,
        )
        .unwrap()
        .unwrap();
        shell.set_theme(theme);
        for (provider, model) in [("anthropic", "claude-sonnet-4"), ("openai", "gpt-5")] {
            shell.set_identity(provider, model, "max");
            let state = shell.state.borrow();
            let chrome = shell_chrome(&state, size.0, Instant::now());
            assert_eq!(chrome.composer, vec![format!("{}\x1b[0m", entries[1].1)]);
            assert_eq!(
                chrome.extension_above,
                vec![format!("{}\x1b[0m", entries[2].1)]
            );
            assert_eq!(
                chrome.extension_below,
                vec![
                    format!("{}\x1b[0m", entries[3].1),
                    format!("{}\x1b[0m", entries[4].1)
                ]
            );
            assert!(
                render_welcome_card(&state, size.0, 8, Instant::now())[0].starts_with(entries[0].1)
            );
            assert_eq!(window_title(&state), "Pi-owned title");
            assert_eq!(state.editor.text(), "kept native draft");
        }
    }
    shell.set_remote_ui(Projection::default());
    let state = shell.state.borrow();
    assert_eq!(window_title(&state), "octet");
    assert!(!shell_chrome(&state, size.0, Instant::now())
        .composer
        .join("\n")
        .contains("editor\x1b"));
    assert_eq!(state.editor.text(), "kept native draft");
}

#[test]
fn explicit_toml_keeps_native_rules_tool_fill_and_gutter_with_remote_chrome() {
    use crate::tui::theme::{compiled_theme_for_selector, TerminalBackground};
    let source = r##"
[colors]
accent = "#44aa88"
composer = "framed"
composer_border = "#6688aa"
prompt_wash = false
margin_markers = false
event_marker_gutter = 4
[model]
use_lab_color = false
[roles."surface.tool"]
background = "#203040"
[surfaces.tool]
chrome = "band"
padding = 2
narrow_padding = 2
[layout]
transcript_inset = 1
composer_padding = 1
"##;
    let mut shell = super::super::InteractiveShell::test_shell();
    shell.prefill_editor("native draft".into());
    for size in [(80, 24), (30, 10), (80, 24)] {
        shell.set_size(size.0, size.1);
        shell.set_remote_ui(projection(
            size,
            &[
                (Placement::Footer, "Pi footer"),
                (Placement::AboveEditor, "Pi widget"),
            ],
        ));
        // Extension presence and a previous Pi selection cannot permanently
        // opt an explicitly selected TOML theme into Pi presentation policy.
        let pi = compiled_theme_for_selector(
            "pi",
            shell.theme().capabilities(),
            TerminalBackground::Dark,
        )
        .unwrap()
        .unwrap();
        shell.set_theme(pi);
        shell.set_theme(crate::tui::theme::test_theme_from_source(source));
        for (provider, model) in [("anthropic", "claude-sonnet-4"), ("openai", "gpt-5")] {
            shell.set_identity(provider, model, "max");
            let state = shell.state.borrow();
            assert!(!state.theme.is_pi_theme());
            let chrome = shell_chrome(&state, size.0, Instant::now());
            let composer = chrome.composer.join("\n");
            assert!(
                composer.contains("╭") && composer.contains("╰"),
                "{composer:?}"
            );
            assert!(
                composer.contains("38;2;102;136;170"),
                "explicit border survives model switch"
            );
            assert_eq!(chrome.extension_below, vec!["Pi footer\x1b[0m"]);
            assert_eq!(chrome.extension_above, vec!["Pi widget\x1b[0m"]);
            let block =
                super::super::TranscriptBlock::Tool(Box::new(super::super::ToolPanel::new(
                    octet_ai::ToolCallId("custom-theme-tool".into()),
                    "read".into(),
                    "{}".into(),
                    crate::presentation::summarize_tool("read", &serde_json::json!({})),
                    "tool output".into(),
                    true,
                    false,
                    None,
                    None,
                )));
            let plan = super::super::surface_layout::compile_surface_plan(
                None,
                &block,
                &state.theme,
                size.0,
            );
            assert_eq!(plan.padding, 2);
            // transcript_inset includes the baseline gutter; inset 1 normalizes
            // to zero, leaving the explicitly requested four cells plus padding.
            assert_eq!(plan.geometry.content_left, 6);
            assert!(super::super::surface_frame::event_margin_marker_with_frame(
                &block,
                &state.theme,
                0,
                None,
                0,
                false
            )
            .is_none());
            let renderer = state.theme.rich_renderer();
            let rows = super::super::transcript_render::render_block(
                None,
                &block,
                &state.theme,
                &renderer,
                &renderer,
                size.0,
                true,
            );
            assert!(
                rows.iter().any(|row| row.contains("48;2;32;48;64")),
                "custom tool fill: {rows:?}"
            );
            assert!(rows
                .iter()
                .all(|row| sexy_tui_rs::visible_width(row) <= usize::from(size.0)));
            assert_eq!(state.editor.text(), "native draft");
        }
    }
}

#[test]
fn remote_ui_empty_editor_does_not_add_a_restore_hint_row() {
    let mut state = ShellState {
        size: (80, 24),
        ..ShellState::default()
    };
    let mut remote = projection(
        state.size,
        &[
            (Placement::Editor, ""),
            (Placement::Footer, "remote footer"),
        ],
    );
    Arc::make_mut(&mut remote.components)
        .store_render(&remote.mounts[0].id, state.size.0, Vec::new())
        .unwrap();
    state.extension_ui.remote = remote;
    let chrome = shell_chrome(&state, state.size.0, Instant::now());
    assert!(chrome.composer.is_empty(), "{:?}", chrome.composer);
    assert_eq!(chrome.extension_below, vec!["remote footer\x1b[0m"]);
}

#[test]
fn remote_ui_header_replaces_welcome_and_hides_wrong_geometry() {
    let mut state = ShellState {
        size: (80, 24),
        ..ShellState::default()
    };
    state.extension_ui.remote = projection(state.size, &[(Placement::Header, "remote header")]);
    assert!(render_welcome_card(&state, 79, 20, Instant::now())
        .join("\n")
        .contains("remote header"));
    state.size = (81, 24);
    assert!(render_welcome_card(&state, 80, 20, Instant::now()).is_empty());
}

#[tokio::test]
async fn remote_ui_fullscreen_keeps_host_picker_priority_and_restores_draft() {
    let (mut shell, _) =
        crate::tui::view::tests::emulated_shell(crate::tui::theme::test_theme(), 120, 40);
    shell.prefill_editor("saved draft".into());
    let size = shell.terminal_dimensions();
    let cached = projection(
        size,
        &[(Placement::Fullscreen, "\x1b[32mgame frame\x1b[0m")],
    );
    shell.set_remote_ui(cached);
    shell.render();
    let frame = shell.dump_rendered_frame().await.unwrap();
    assert!(
        frame.iter().any(|line| line.contains("\x1b[32mgame frame")),
        "remote rows: {frame:?}"
    );
    assert!(!shell.debug_snapshot().contains("game frame"));
    assert!(shell.state.borrow().overlay.is_some());
    let rows = fullscreen_overlay(&shell.state.borrow().extension_ui.remote, 8, size.1).unwrap();
    assert!(
        rows.lines().last().unwrap().starts_with("Ctrl+G"),
        "a long title cannot hide rescue"
    );
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::with_purpose("Host picker", "Host owns input"),
        items: vec!["first".into()],
        descriptions: vec![None],
        selected: 0,
        filter: String::new(),
        action: PanelAction::SelectModel(Vec::new()),
    });
    assert!(shell.remote_ui_input_blocked());
    shell.render();
    let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
    assert!(frame.contains("Host picker"));
    shell.close_panel();
    shell.show_overlay_text("Host report".into());
    assert!(shell.remote_ui_input_blocked());
    shell.close_overlay();
    assert!(!shell.remote_ui_input_blocked());
    shell.set_remote_ui(Projection::default());
    shell.render();
    assert!(!shell.has_overlay());
    assert_eq!(shell.extension_editor_snapshot().text, "saved draft");
}
