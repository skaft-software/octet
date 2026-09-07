use super::*;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

fn press(shell: &mut InteractiveShell, code: KeyCode, active: bool) {
    let action = crate::tui::keymap::translate_with_popup(
        Some(Event::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        active,
        &shell.pending(),
        shell.slash_popup_open(),
    );
    match action {
        crate::tui::keymap::InputAction::Edit(action) => shell.apply_edit(action),
        crate::tui::keymap::InputAction::CompletePath => shell.complete_path(),
        other => panic!("unexpected completion action: {other:?}"),
    }
}

fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..10 {
        std::fs::write(dir.path().join(format!("item{index}.rs")), "text").unwrap();
    }
    dir
}

fn composer(root: &std::path::Path, draft: &str) -> InteractiveShell {
    let mut shell = InteractiveShell::test_shell();
    shell.set_workspace(root.to_path_buf());
    shell.apply_edit(EditAction::Paste(draft.to_owned()));
    shell
}

#[test]
fn arrows_select_and_tab_accepts_paths_and_mentions_while_idle_or_active() {
    let dir = workspace();
    for active in [false, true] {
        for (draft, completed) in [
            ("inspect ./item", "inspect ./item6.rs "),
            ("inspect @item", "inspect @item6.rs "),
            ("inspect @./item", "inspect @./item6.rs "),
        ] {
            let mut shell = composer(dir.path(), draft);
            let cursor = shell.state.borrow().editor.cursor();
            press(&mut shell, KeyCode::Up, active);
            assert_eq!(shell.state.borrow().path_selection, 0);
            for _ in 0..7 {
                press(&mut shell, KeyCode::Down, active);
            }
            press(&mut shell, KeyCode::Up, active);
            assert_eq!(shell.state.borrow().path_selection, 6);
            assert_eq!(shell.pending(), draft);
            assert_eq!(shell.state.borrow().editor.cursor(), cursor);

            // The highlighted result stays visible beyond the first five rows,
            // including after the available popup height contracts.
            for rows in [6, 3, 2] {
                let state = shell.state.borrow();
                let lines = input_overlays::render_input_suggestions(&state, 80, rows);
                let selected = lines.iter().find(|line| line.contains("item6.rs")).unwrap();
                let selected = sexy_tui_rs::strip_terminal_sequences(selected);
                assert!(selected
                    .trim_start()
                    .starts_with(state.theme.glyph("prompt")));
                assert_eq!(lines.len(), rows);
                assert!(lines.iter().all(|line| !line.contains("item7.rs")));
            }
            press(&mut shell, KeyCode::Tab, active);
            assert_eq!(shell.pending(), completed);
            assert_eq!(shell.state.borrow().path_selection, 0);
        }
    }
}

#[test]
fn path_selection_clamps_and_resets_when_query_or_workspace_changes() {
    let dir = workspace();
    let mut shell = composer(dir.path(), "@item");
    for _ in 0..15 {
        press(&mut shell, KeyCode::Down, false);
    }
    assert_eq!(shell.state.borrow().path_selection, 9);
    shell.apply_edit(EditAction::Char('9'));
    assert_eq!(shell.state.borrow().path_selection, 0);
    press(&mut shell, KeyCode::Tab, false);
    assert_eq!(shell.pending(), "@item9.rs ");

    let mut shell = composer(dir.path(), "./item");
    press(&mut shell, KeyCode::Down, false);
    shell.invalidate_file_index();
    assert_eq!(shell.state.borrow().path_selection, 0);
    press(&mut shell, KeyCode::Down, false);
    let other = tempfile::tempdir().unwrap();
    std::fs::write(other.path().join("item-new.rs"), "text").unwrap();
    shell.set_workspace(other.path().to_path_buf());
    assert_eq!(shell.state.borrow().path_selection, 0);
    press(&mut shell, KeyCode::Tab, false);
    assert_eq!(shell.pending(), "./item-new.rs ");
}

#[test]
fn arrows_remain_visual_editor_navigation_without_a_visible_path_menu() {
    let dir = workspace();
    for draft in [
        "first line\nno matches",
        "first line\n@missing",
        "first line\n./missing",
    ] {
        let mut shell = composer(dir.path(), draft);
        let cursor = shell.state.borrow().editor.cursor();
        press(&mut shell, KeyCode::Up, false);
        assert!(shell.state.borrow().editor.cursor() < cursor);
        assert_eq!(shell.pending(), draft);
        assert_eq!(shell.state.borrow().path_selection, 0);
    }

    let mut shell = composer(dir.path(), "first line\n@item");
    press(&mut shell, KeyCode::Left, false);
    let cursor = shell.state.borrow().editor.cursor();
    press(&mut shell, KeyCode::Up, false);
    assert!(shell.state.borrow().editor.cursor() < cursor);
    assert_eq!(shell.state.borrow().path_selection, 0);

    let mut shell = composer(dir.path(), "first line\n@item");
    shell.set_size(80, 1);
    let cursor = shell.state.borrow().editor.cursor();
    press(&mut shell, KeyCode::Up, false);
    assert!(shell.state.borrow().editor.cursor() < cursor);
    assert_eq!(shell.state.borrow().path_selection, 0);
}

#[test]
fn selected_directory_descends_and_selected_media_still_becomes_an_attachment() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["folder0", "folder1"] {
        std::fs::create_dir(dir.path().join(name)).unwrap();
        std::fs::write(dir.path().join(name).join("nested.txt"), "text").unwrap();
    }
    let mut shell = composer(dir.path(), "@./folder");
    press(&mut shell, KeyCode::Down, false);
    press(&mut shell, KeyCode::Tab, false);
    assert_eq!(shell.pending(), "@./folder1/");
    press(&mut shell, KeyCode::Tab, false);
    assert_eq!(shell.pending(), "@./folder1/nested.txt ");

    for name in ["shot0.png", "shot1.png"] {
        std::fs::write(dir.path().join(name), "png").unwrap();
    }
    let mut shell = composer(dir.path(), "@shot");
    shell.set_input_modalities(octet_ai::ModalitySet::none().with(octet_ai::Modality::Image));
    press(&mut shell, KeyCode::Down, false);
    press(&mut shell, KeyCode::Tab, false);
    assert_eq!(shell.pending(), "[Image #1]");
    let composed = shell.drain_composed();
    assert!(composed
        .parts
        .iter()
        .any(|part| matches!(part, octet_agent::InputPart::Media(_))));
}
