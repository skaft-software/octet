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
