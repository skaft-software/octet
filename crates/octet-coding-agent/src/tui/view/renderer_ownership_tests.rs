use super::super::{
    InteractiveShell, OutputChannel, Panel, PanelAction, PanelResult, TranscriptBlock,
};
use super::*;
use crate::tui::keymap::{EditAction, InputAction, SlashMenuAction};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

// NativeReplay uses the same immutable-publication boundary as the renderer
// thread, while retaining its deterministic in-process Pi/VT terminal.
impl InteractiveShell {
    pub(in crate::tui::view) fn isolate_native_test_renderer(&mut self) {
        self.state.borrow_mut().render_threaded = true;
        let tui = self.tui.as_mut().unwrap();
        tui.remove_child(0);
        tui.add_child(Box::new(ShellComponent::isolated(self.state.clone(), false)));
    }
}

#[test]
fn isolated_structural_rebuild_cannot_reuse_the_retained_frame_generation() {
    for insert in [false, true] {
        let state = SharedState::new(ShellState {
            theme: crate::tui::theme::test_theme(),
            size: (80, 8),
            follow_tail: true,
            ..Default::default()
        });
        for index in 0..60 {
            state
                .borrow_mut()
                .push_block(TranscriptBlock::Notice(format!("RETAINED-{index:02}")));
        }
        let component = ShellComponent::isolated(state.clone(), false);
        let mut retained = component.render(80);
        assert_eq!(component.frame.borrow().transcript_generation, 1);
        let epoch = state.borrow().transcript_epoch;
        if insert {
            state
                .borrow_mut()
                .insert_block(2, TranscriptBlock::Notice("INSERTED".into()));
        } else {
            state.borrow_mut().remove_transient_activity_block(2);
        }
        assert_eq!(state.borrow().transcript_epoch, epoch);
        let update = component.render_update(80).unwrap();
        assert!(component.frame.borrow().transcript_generation > 1);
        assert_eq!(update.stable_prefix, 0, "a new root cannot certify old rows");
        retained.truncate(update.stable_prefix);
        retained.extend(update.replacement);
        let owner = component.owner.as_ref().unwrap().borrow();
        assert_eq!(retained, render_shell(&owner.state, 80));
    }
}

struct ReleaseGate(Arc<RenderGate>);
impl Drop for ReleaseGate {
    fn drop(&mut self) {
        self.0.release();
    }
}
fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn exercise_control(shell: &mut InteractiveShell) {
    shell.apply_edit(EditAction::Char('a'));
    shell.apply_edit(EditAction::Paste("βc".into()));
    shell.apply_edit(EditAction::Backspace);
    assert_eq!(shell.state.borrow().editor.text(), "aβ");
    assert_eq!(
        shell.translate_input(Some(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), true),
        InputAction::ClearEditor
    );
    shell.clear_editor();
    assert_eq!(
        shell.translate_input(Some(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), true),
        InputAction::Abort
    );
    shell.apply_edit(EditAction::Paste("/he".into()));
    assert_eq!(
        shell.translate_input(Some(key(KeyCode::Esc, KeyModifiers::NONE)), true),
        InputAction::SlashMenu(SlashMenuAction::Close)
    );
    shell.slash_menu(SlashMenuAction::Close);
    shell.clear_editor();
    shell.open_panel(Panel::ReadOnlyDocument {
        title: "modal".into(),
        text: "huge document row with wrapping text "
            .repeat(100_000)
            .into(),
        styled: false,
        scroll_from_bottom: 0,
    });
    super::super::panel_render::panel_render_test_hook::take_document_layouts();
    shell.panel_input(&key(KeyCode::Home, KeyModifiers::NONE));
    shell.update_read_only_document("replacement document row ".repeat(100_000));
    assert_eq!(
        shell.translate_input(Some(key(KeyCode::Char('d'), KeyModifiers::CONTROL)), true),
        InputAction::Closed
    );
    assert!(matches!(
        shell.panel_input(&key(KeyCode::Esc, KeyModifiers::NONE)),
        Some((PanelResult::Cancel, PanelAction::ReadOnlyDocument))
    ));
    assert_eq!(
        super::super::panel_render::panel_render_test_hook::take_document_layouts(),
        0,
        "document navigation, refresh and cancellation cannot layout on input"
    );
    shell.close_panel();
    shell.request_close();
    assert!(shell.close_requested());
    shell.apply_edit(EditAction::Paste("retained draft".into()));
}

#[test]
fn actual_thread_blocked_layout_accepts_control_and_recovers_every_character() {
    let (started, observe_gate) = mpsc::channel();
    let (admitted, admission) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let observer = std::thread::spawn(move || {
        let (mut shell, gate) = InteractiveShell::test_blocked_renderer();
        // This guard drops before the shell even when any assertion panics.
        let _release = ReleaseGate(gate.clone());
        started.send(gate.clone()).unwrap();
        assert!(gate.wait_until_entered(Duration::from_secs(3)));
        exercise_control(&mut shell);
        let run = shell.begin_run("fixture");
        let source = "accepted αβ ".repeat(512);
        for chunk in source.split_inclusive(' ') {
            shell
                .state
                .borrow_mut()
                .append_text_block(OutputChannel::Text, chunk);
            shell.render(); // Capacity one; acceptance never waits for paint.
        }
        shell.interrupt_run(run);
        assert!(!shell.state.borrow().run.is_active());
        let retained = shell
            .state
            .borrow()
            .transcript
            .iter()
            .find_map(|block| match block {
                TranscriptBlock::Assistant(block) => Some(block.text.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(retained, source);
        admitted.send(()).unwrap();
        // Wait until the independent watchdog has observed admission, then
        // release layout. This handshake cannot be satisfied by inline layout.
        while !gate.state.lock().unwrap().1 {
            std::thread::sleep(Duration::from_millis(1));
        }
        shell.render();
        // DumpFrame is deliberately a read of the last painted frame, not a
        // render barrier. Wait for the current publication's paint receipt;
        // otherwise a coalesced Render can be overtaken by this diagnostic.
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let state = shell.state.borrow();
            if state
                .render_geometry
                .as_ref()
                .is_some_and(|geometry| geometry.is_current(&state))
            {
                break;
            }
            drop(state);
            assert!(Instant::now() < deadline, "latest publication was not painted");
            std::thread::sleep(Duration::from_millis(1));
        }
        let (reply, receive) = mpsc::channel();
        shell
            .render_tx
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .send(RenderCommand::DumpFrame(reply))
            .unwrap();
        let frame = receive.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(frame.iter().any(|line| line.contains("accepted")));
        assert!(frame.iter().any(|line| line.contains("retained draft")));
        done.send(()).unwrap();
    });
    let gate = observe_gate.recv_timeout(Duration::from_secs(3)).unwrap();
    let release = ReleaseGate(gate);
    let accepted = admission.recv_timeout(Duration::from_secs(3));
    drop(release); // Always unblock the real renderer before reporting failure.
    let recovered = completed.recv_timeout(Duration::from_secs(5));
    observer.join().unwrap();
    accepted.expect("control admission waited behind actual threaded layout");
    recovered.expect("renderer did not recover the latest semantic root");
}

struct GatedTerminal {
    gate: Arc<RenderGate>,
}
impl sexy_tui_rs::Terminal for GatedTerminal {
    fn start_events(&mut self, _: Box<dyn FnMut(sexy_tui_rs::TerminalInput)>, _: Box<dyn FnMut()>) {
    }
    fn stop(&mut self) {}
    fn write(&mut self, _: &str) {
        self.gate.wait();
    }
    fn columns(&self) -> u16 {
        80
    }
    fn rows(&self) -> u16 {
        24
    }
    fn move_by(&mut self, _: i16) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {}
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {}
    fn clear_screen(&mut self) {}
}

#[test]
fn actual_thread_blocked_terminal_write_does_not_own_input() {
    let gate = Arc::new(RenderGate::default());
    let release = ReleaseGate(gate.clone());
    let (admitted, admission) = mpsc::channel();
    let observer = std::thread::spawn(move || {
        let mut shell = InteractiveShell::test_shell();
        shell.tui.take();
        let _release = ReleaseGate(gate.clone());
        let state = shell.state.clone();
        let size = shell.size.clone();
        let terminal = GatedTerminal { gate: gate.clone() };
        let (tx, rx) = mpsc::sync_channel(1);
        *shell.render_tx.lock().unwrap() = Some(tx);
        shell.render_thread = Some(std::thread::spawn(move || {
            render_loop_with_terminal(terminal, state, size, rx, false, false, false, |_, _| false);
        }));
        assert!(gate.wait_until_entered(Duration::from_secs(3)));
        exercise_control(&mut shell);
        admitted.send(()).unwrap();
    });
    let accepted = admission.recv_timeout(Duration::from_secs(3));
    drop(release);
    observer.join().unwrap();
    accepted.expect("blocked terminal I/O retained semantic ownership");
}

#[test]
fn stale_layout_cannot_overwrite_new_viewport_intent() {
    let state = SharedState::new(ShellState {
        theme: crate::tui::theme::test_theme(),
        size: (80, 24),
        ..Default::default()
    });
    let component = ShellComponent::isolated(state.clone(), false);
    let rendered = component.borrow_for_render();
    {
        let mut semantic = state.borrow_mut();
        semantic.editor.set_text("new draft");
        semantic.scroll_from_bottom.set(37);
        semantic.application_viewport_requested = true;
    }
    rendered.scroll_from_bottom.set(1);
    drop(rendered);
    assert_eq!(state.borrow().scroll_from_bottom.get(), 37);
    assert_eq!(state.borrow().editor.text(), "new draft");
}

#[test]
fn threaded_approval_requires_completed_write_and_current_geometry() {
    use super::super::ordinary_surface::OrdinarySurfaceMetadata;
    let mut shell = InteractiveShell::test_shell();
    shell.tui.take();
    shell.state.borrow_mut().render_threaded = true;
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Approve?"),
        items: vec!["Deny".into(), "Approve".into()],
        descriptions: vec![Some("writes a file".into()); 2],
        selected: 1,
        filter: String::new(),
        action: PanelAction::Confirmation,
    });
    let component = ShellComponent::isolated(shell.state.clone(), false);
    let enter = key(KeyCode::Enter, KeyModifiers::NONE);
    component.render(120);
    assert!(
        shell.panel_input(&enter).is_none(),
        "layout alone is not an emitted approval"
    );
    // Transcript/control revisions cannot starve the already visible consent.
    shell
        .state
        .borrow_mut()
        .push_block(TranscriptBlock::Notice("concurrent tool result".into()));
    shell.state.frame_written();
    assert!(shell.state.borrow().painted_panel.is_some());
    assert!(matches!(
        shell.panel_input(&enter),
        Some((PanelResult::Confirm(1), PanelAction::Confirmation))
    ));
}

#[test]
fn threaded_document_navigation_and_refresh_use_emitted_geometry() {
    use super::super::panel_render::panel_render_test_hook::take_document_layouts;
    let mut shell = InteractiveShell::test_shell();
    shell.tui.take();
    shell.set_size(60, 20);
    shell.state.borrow_mut().render_threaded = true;
    shell.open_panel(Panel::ReadOnlyDocument {
        title: "document".into(),
        text: "row\n".repeat(100).into(),
        styled: false,
        scroll_from_bottom: 0,
    });
    let component = ShellComponent::isolated(shell.state.clone(), false);
    component.render(60);
    shell.state.frame_written();
    take_document_layouts();
    shell.panel_input(&key(KeyCode::Home, KeyModifiers::NONE));
    shell.update_read_only_document("row\n".repeat(130));
    assert_eq!(
        take_document_layouts(),
        0,
        "input must not wrap either document"
    );
    assert_eq!(shell.state.borrow().pending_panel_document_top, Some(0));
    component.render(60);
    shell.state.frame_written();
    let state = shell.state.borrow();
    let receipt = state.painted_panel.as_ref().unwrap();
    let Some(Panel::ReadOnlyDocument {
        scroll_from_bottom, ..
    }) = &state.panel
    else {
        panic!()
    };
    assert_eq!(
        *scroll_from_bottom,
        receipt.document_rows - receipt.document_body_rows
    );
    assert_eq!(state.pending_panel_document_top, None);
    drop(state);
    shell.set_size(35, 12);
    take_document_layouts();
    shell.panel_input(&key(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(take_document_layouts(), 0);
    let state = shell.state.borrow();
    let Some(Panel::ReadOnlyDocument {
        scroll_from_bottom, ..
    }) = &state.panel
    else {
        panic!()
    };
    assert!(
        *scroll_from_bottom > 0,
        "stale geometry must not move the document"
    );
}

#[test]
fn threaded_approval_rejects_changed_selected_action_before_write() {
    use super::super::ordinary_surface::OrdinarySurfaceMetadata;
    for change_label in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.tui.take();
        shell.state.borrow_mut().render_threaded = true;
        shell.open_panel(Panel::SelectList {
            surface: OrdinarySurfaceMetadata::new("Approve?"),
            items: vec!["Deny".into(), "Approve".into()],
            descriptions: vec![None; 2],
            selected: 1,
            filter: String::new(),
            action: PanelAction::Confirmation,
        });
        let component = ShellComponent::isolated(shell.state.clone(), false);
        component.render(120);
        shell.state.frame_written();
        assert!(shell.state.borrow().painted_panel.is_some());
        component.render(120);
        if change_label {
            let mut state = shell.state.borrow_mut();
            let Some(Panel::SelectList { items, .. }) = state.panel.as_mut() else {
                panic!()
            };
            items[1] = "Approve different action".into();
        } else {
            shell.panel_input(&key(KeyCode::Up, KeyModifiers::NONE));
        }
        shell.state.frame_written();
        assert!(shell.state.borrow().painted_panel.is_none());
        assert!(shell
            .panel_input(&key(KeyCode::Enter, KeyModifiers::NONE))
            .is_none());
        component.render(120);
        shell.state.frame_written();
        assert!(matches!(
            shell.panel_input(&key(KeyCode::Enter, KeyModifiers::NONE)),
            Some((PanelResult::Confirm(_), PanelAction::Confirmation))
        ));
    }
}

#[test]
fn published_panels_and_reports_share_immutable_bodies() {
    use super::super::renderer_model::{RenderModel, RenderOwner};
    use super::super::{
        ordinary_surface::OrdinarySurfaceMetadata, ReportBody, ReportOverlay, ShellOverlay,
    };
    let text: Arc<str> = "large report body\n".repeat(100_000).into();
    let mut state = ShellState::default();
    state.panel = Some(Panel::ReadOnlyDocument {
        title: "shared document".into(),
        text: text.clone(),
        styled: false,
        scroll_from_bottom: 0,
    });
    state.overlay = Some(ShellOverlay::Report(ReportOverlay {
        surface: OrdinarySurfaceMetadata::new("report"),
        body: ReportBody::Text {
            text: text.clone(),
            styled: false,
        },
        scroll_from_top: 0,
    }));
    let mut owner = RenderOwner::default();
    owner.accept(RenderModel::capture(&mut state));
    let Some(Panel::ReadOnlyDocument {
        text: published, ..
    }) = &owner.state.panel
    else {
        panic!()
    };
    assert!(Arc::ptr_eq(&text, published));
    let Some(ShellOverlay::Report(ReportOverlay {
        body: ReportBody::Text {
            text: published, ..
        },
        ..
    })) = &owner.state.overlay
    else {
        panic!()
    };
    assert!(Arc::ptr_eq(&text, published));
    let document = Arc::new(sexy_tui_rs::parse_markdown("# shared markdown\n\ncontent"));
    let Some(ShellOverlay::Report(report)) = state.overlay.as_mut() else {
        panic!()
    };
    report.body = ReportBody::Markdown(document.clone());
    owner.accept(RenderModel::capture(&mut state));
    let Some(ShellOverlay::Report(ReportOverlay {
        body: ReportBody::Markdown(published),
        ..
    })) = &owner.state.overlay
    else {
        panic!()
    };
    assert!(Arc::ptr_eq(&document, published));
}
