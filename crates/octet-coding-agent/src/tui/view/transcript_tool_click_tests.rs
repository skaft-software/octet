use super::*;
use crate::tui::keymap::{InputAction, PointerGesture};
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use octet_agent::extension_process::{
    ExtensionResourceOwner, TranscriptRenderContent, TranscriptRenderResponse,
};

const DOWN: MouseEventKind = MouseEventKind::Down(MouseButton::Left);
const UP: MouseEventKind = MouseEventKind::Up(MouseButton::Left);
const DRAG: MouseEventKind = MouseEventKind::Drag(MouseButton::Left);

fn fixture(threaded: bool) -> InteractiveShell {
    let mut shell = InteractiveShell::test_shell();
    shell.capture_mouse = true;
    shell.set_size(96, 80);
    shell.apply_edit(EditAction::Paste("kept draft".into()));
    {
        let mut state = shell.state.borrow_mut();
        state.application_viewport_requested = true;
        for name in ["first", "second"] {
            let args = serde_json::json!({"command": format!("echo {name}"), "private": "PRIVATE"});
            let output = (0..18)
                .map(|n| format!("{name}-{n:02}"))
                .collect::<Vec<_>>()
                .join("\n");
            state.push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
                ToolCallId(name.into()),
                "bash".into(),
                args.to_string(),
                crate::presentation::summarize_tool("bash", &args),
                output,
                true,
                false,
                None,
                None,
            ))));
        }
        state.push_block(TranscriptBlock::Reasoning(Box::new(
            AssistantBlock::finalized_reasoning("unrelated trace".into()),
        )));
    }
    if threaded {
        shell.isolate_native_test_renderer();
    }
    shell
}

fn paint(shell: &mut InteractiveShell) -> Vec<String> {
    shell.render_written_test_frame();
    shell
        .tui
        .as_ref()
        .unwrap()
        .rendered_frame()
        .iter()
        .map(|s| strip_terminal_sequences(s))
        .collect()
}

fn cell(rows: &[String], text: &str) -> (u16, u16) {
    rows.iter()
        .enumerate()
        .find_map(|(row, line)| {
            let i = line.find(text)?;
            Some((row as u16, visible_width(&line[..i]) as u16))
        })
        .unwrap_or_else(|| panic!("missing {text:?} in {rows:?}"))
}

fn dispatch(
    shell: &mut InteractiveShell,
    kind: MouseEventKind,
    at: (u16, u16),
    modifiers: KeyModifiers,
) {
    dispatch_with_opener(shell, kind, at, modifiers, |url| {
        panic!("unexpected browser handoff: {url}")
    });
}

fn dispatch_with_opener(
    shell: &mut InteractiveShell,
    kind: MouseEventKind,
    (row, column): (u16, u16),
    modifiers: KeyModifiers,
    open: impl FnOnce(&str),
) {
    match shell.translate_input(
        Some(Event::Mouse(MouseEvent {
            kind,
            row,
            column,
            modifiers,
        })),
        false,
    ) {
        InputAction::TranscriptPointer(PointerGesture::Begin { row, col, extend }) => {
            shell.begin_transcript_selection(row, col, extend)
        }
        InputAction::TranscriptPointer(PointerGesture::Extend { row, col }) => {
            shell.extend_transcript_selection(row, col)
        }
        InputAction::TranscriptPointer(PointerGesture::End { row, col }) => {
            shell.end_transcript_selection_with(row, col, open)
        }
        InputAction::ScrollLines(lines) => shell.scroll_lines(lines),
        InputAction::Ignore => (),
        other => panic!("unexpected action {other:?}"),
    }
}

#[test]
fn tool_card_click_expands_two_independent_cards_and_collapses_again() {
    for threaded in [false, true] {
        let mut shell = fixture(threaded);
        let mut rows = paint(&mut shell);
        for (target, first, second) in [
            ("first", true, false),
            ("second", true, true),
            ("first", false, true),
        ] {
            let hit = cell(&rows, &format!("echo {target}"));
            dispatch(&mut shell, DOWN, hit, KeyModifiers::NONE);
            assert_eq!(paint(&mut shell), rows, "press alone must not disclose");
            dispatch(&mut shell, UP, hit, KeyModifiers::NONE);
            rows = paint(&mut shell);
            let text = rows.join("\n");
            assert_eq!(text.contains("first-09"), first, "{text}");
            assert_eq!(text.contains("second-09"), second, "{text}");
            shell.select_all_transcript();
            let copy = shell.selected_plain_text().unwrap();
            assert_eq!(copy.contains("first-09"), first, "{copy}");
            assert_eq!(copy.contains("second-09"), second, "{copy}");
            assert!(!copy.contains("PRIVATE"));
            assert!(!shell.verbose_tools());
            assert!(!shell.thinking_expanded());
            assert_eq!(shell.pending(), "kept draft");
        }
        let mut state = shell.state.borrow_mut();
        let trace = state.transcript_commit_ids[2];
        state.extension_transcript.expanded.insert(trace, true);
        drop(state);
        shell.toggle_disclosure();
        let state = shell.state.borrow();
        assert_eq!(state.extension_transcript.expanded.len(), 1);
        assert_eq!(state.extension_transcript.expanded.get(&trace), Some(&true));
    }
}

#[test]
fn extension_tool_header_discloses_even_when_the_card_has_no_copy_text() {
    const HEADER: &str = "Adapter-only 界 heading";
    for threaded in [false, true] {
        for render_shell in ["default", "self"] {
            let mut shell = fixture(false);
            // In a quiet theme, a collapsed group child's native copy projection is empty.
            let theme = crate::tui::theme::test_theme_from_source(
                "[colors]\nquiet_tool_summaries = true\n[surfaces.tool]\nchrome = \"plain\"",
            );
            shell.set_theme(theme);
            {
                let mut state = shell.state.borrow_mut();
                let TranscriptBlock::Tool(panel) = &mut state.transcript[0] else {
                    unreachable!()
                };
                panel.grouped_child = true;
                state.touch_block(0);
            }
            if threaded {
                shell.isolate_native_test_renderer();
            }
            let owner = ExtensionResourceOwner {
                session_id: "session".into(),
                extension_instance_id: "renderer".into(),
                process_generation: 1,
            };
            shell.set_transcript_render_owners(vec![owner.clone()]);
            let candidate = shell.transcript_render_candidates().into_iter().find(|c| {
                matches!(&c.render, TranscriptRenderContent::Tool { tool_call_id, .. } if tool_call_id == "first")
            }).unwrap();
            let response = |body: &str| TranscriptRenderResponse {
                registered: true,
                lines: Some(vec![HEADER.into(), body.into()]),
                markdown: None,
                render_shell: Some(render_shell.into()),
            };
            assert!(shell.accept_transcript_render(
                &candidate,
                owner.clone(),
                response("Adapter body")
            ));
            let rows = paint(&mut shell);
            shell.select_all_transcript();
            let copy = shell.selected_plain_text().unwrap();
            assert!(
                copy.contains("echo second") && !copy.contains("echo first"),
                "{copy}"
            );
            assert!(!copy.contains("Adapter"), "{copy}");

            let body = cell(&rows, "Adapter body");
            dispatch(&mut shell, DOWN, body, KeyModifiers::NONE);
            dispatch(&mut shell, UP, body, KeyModifiers::NONE);
            let header = cell(&rows, "界 heading");
            dispatch(&mut shell, DOWN, header, KeyModifiers::NONE);
            // Both cells of the wide glyph map to the same empty copy range.
            dispatch(
                &mut shell,
                DRAG,
                (header.0, header.1 + 1),
                KeyModifiers::NONE,
            );
            dispatch(&mut shell, DRAG, header, KeyModifiers::NONE);
            dispatch(&mut shell, UP, header, KeyModifiers::NONE);
            assert!(!shell.state.borrow().show_tool_details(0));
            assert!(paint(&mut shell).iter().any(|line| line.contains(HEADER)));

            dispatch(&mut shell, DOWN, header, KeyModifiers::NONE);
            dispatch(&mut shell, UP, header, KeyModifiers::NONE);
            let expanded = shell
                .transcript_render_candidates()
                .into_iter()
                .find(|c| c.key.id == candidate.key.id)
                .expect("request a new frame for the clicked card");
            assert!(matches!(
                expanded.render,
                TranscriptRenderContent::Tool { expanded: true, .. }
            ));
            assert!(!shell.accept_transcript_render(
                &candidate,
                owner.clone(),
                response("Stale compact body")
            ));
            assert!(shell.accept_transcript_render(
                &expanded,
                owner,
                response("Expanded adapter body")
            ));
            assert!(paint(&mut shell)
                .iter()
                .any(|line| line.contains("Expanded adapter body")));
            assert!(!shell.verbose_tools());
        }
    }
}

#[test]
fn body_and_header_padding_do_not_disclose_and_transcript_links_still_open_on_release() {
    const TARGET: &str = "https://example.test/tool-help";
    for threaded in [false, true] {
        let mut shell = fixture(threaded);
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Assistant(Box::new(
                AssistantBlock::finalized(format!("See [tool reference]({TARGET})")),
            )));
        let rows = paint(&mut shell);
        let header = cell(&rows, "echo first");
        let padding = (header.0, header.1 + visible_width("echo first") as u16 + 1);
        assert!(sexy_tui_rs::slice_by_column(
            &rows[usize::from(padding.0)],
            usize::from(padding.1),
            1,
            false
        )
        .trim()
        .is_empty());
        for at in [cell(&rows, "first-17"), padding] {
            dispatch(&mut shell, DOWN, at, KeyModifiers::NONE);
            dispatch(&mut shell, UP, at, KeyModifiers::NONE);
            assert!(!shell.state.borrow().show_tool_details(0));
        }
        let link = cell(&rows, "tool reference");
        assert_eq!(
            shell
                .transcript_link_at_screen_cell(link.0, link.1)
                .as_deref(),
            Some(TARGET)
        );
        dispatch(&mut shell, DOWN, link, KeyModifiers::NONE);
        let mut opened = Vec::new();
        for _ in 0..2 {
            dispatch_with_opener(&mut shell, UP, link, KeyModifiers::NONE, |url| {
                opened.push(url.to_owned())
            });
        }
        assert_eq!(opened, [TARGET]);
        assert!(shell
            .state
            .borrow()
            .extension_transcript
            .expanded
            .is_empty());
        assert_eq!(shell.pending(), "kept draft");
    }
}

#[test]
fn dragging_the_header_copies_even_on_a_coalesced_release_and_never_discloses() {
    for threaded in [false, true] {
        for (drag, back) in [(true, false), (false, false), (true, true)] {
            let mut shell = fixture(threaded);
            let start = cell(&paint(&mut shell), "echo first");
            let end = (start.0, start.1 + 4);
            dispatch(&mut shell, DOWN, start, KeyModifiers::NONE);
            if drag {
                dispatch(&mut shell, DRAG, end, KeyModifiers::NONE);
            }
            if back {
                dispatch(&mut shell, MouseEventKind::Moved, start, KeyModifiers::NONE);
            }
            dispatch(
                &mut shell,
                UP,
                if back { start } else { end },
                KeyModifiers::NONE,
            );
            if !back {
                assert_eq!(shell.state.borrow().copy_buffer.as_deref(), Some("echo"));
            }
            assert!(
                !shell.state.borrow().show_tool_details(0),
                "drag={drag} back={back}"
            );
        }
    }
}

#[test]
fn wheel_or_modifiers_on_either_edge_cancel_header_disclosure() {
    for threaded in [false, true] {
        let mut shell = fixture(threaded);
        shell
            .state
            .borrow_mut()
            .insert_block(0, TranscriptBlock::Notice("earlier output\n".repeat(120)));
        let header = cell(&paint(&mut shell), "echo first");
        for modifier in [
            KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
        ] {
            for (down, up) in [
                (modifier, KeyModifiers::NONE),
                (KeyModifiers::NONE, modifier),
            ] {
                dispatch(&mut shell, DOWN, header, down);
                dispatch(&mut shell, UP, header, up);
                assert!(
                    shell
                        .state
                        .borrow()
                        .extension_transcript
                        .expanded
                        .is_empty(),
                    "down={down:?}, up={up:?}"
                );
            }
        }
        dispatch(&mut shell, DOWN, header, KeyModifiers::NONE);
        dispatch(
            &mut shell,
            MouseEventKind::ScrollUp,
            header,
            KeyModifiers::NONE,
        );
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), 1);
        dispatch(
            &mut shell,
            MouseEventKind::ScrollDown,
            header,
            KeyModifiers::NONE,
        );
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), 0);
        assert_eq!(cell(&paint(&mut shell), "echo first"), header);
        dispatch(&mut shell, UP, header, KeyModifiers::NONE);
        assert!(shell
            .state
            .borrow()
            .extension_transcript
            .expanded
            .is_empty());
    }
}

#[test]
fn a_prepared_tool_frame_cannot_authorize_a_click_after_its_write_completes() {
    let mut shell = fixture(true);
    // Request layout directly, holding back the terminal writer's receipt.
    shell.tui.as_mut().unwrap().request_render();
    let prepared = shell.state.prepared_geometry().expect("layout prepared");
    let rows: Vec<_> = prepared
        .visible_lines
        .iter()
        .map(|s| strip_terminal_sequences(s))
        .collect();
    let header = cell(&rows, "echo first");
    assert!(shell.state.borrow().pointer_render_geometry().is_none());
    dispatch(&mut shell, DOWN, header, KeyModifiers::NONE);
    shell.state.frame_written_at(None);
    dispatch(&mut shell, UP, header, KeyModifiers::NONE);
    assert!(!shell.state.borrow().show_tool_details(0));
    dispatch(&mut shell, DOWN, header, KeyModifiers::NONE);
    dispatch(&mut shell, UP, header, KeyModifiers::NONE);
    assert!(
        shell.state.borrow().show_tool_details(0),
        "a fresh click on the written frame must work"
    );
}

#[test]
fn revision_changes_before_press_or_release_fence_otherwise_identical_header_cells() {
    for threaded in [false, true] {
        for changed_before_press in [false, true] {
            let mut shell = fixture(threaded);
            let header = cell(&paint(&mut shell), "echo first");
            if changed_before_press {
                shell.state.borrow_mut().touch_block(0);
            }
            dispatch(&mut shell, DOWN, header, KeyModifiers::NONE);
            if !changed_before_press {
                shell.state.borrow_mut().touch_block(0);
            }
            assert_eq!(cell(&paint(&mut shell), "echo first"), header);
            dispatch(&mut shell, UP, header, KeyModifiers::NONE);
            assert!(
                !shell.state.borrow().show_tool_details(0),
                "stale before press = {changed_before_press}"
            );
        }
    }
}
