//! Deterministic work at the isolated fullscreen input/publication/layout boundary.
use super::renderer_runtime::ShellComponent;
use super::*;
use sexy_tui_rs::Component;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Work {
    pub(super) messages_visited: usize,
    pub(super) rows_laid_out: usize,
    pub(super) rows_mapped: usize,
    pub(super) cells_rendered: usize,
    // String payload cloned by viewport/receipt slices and painted_copy_rows.
    // Excludes allocator overhead, semantic projection and CopyRow internals.
    pub(super) bytes_cloned: usize,
    pub(super) copy_text_bytes: usize,
}
thread_local! {
    static WORK: std::cell::Cell<Work> = std::cell::Cell::new(Work::default());
}
pub(super) fn record(update: impl FnOnce(&mut Work)) {
    WORK.with(|slot| {
        let mut work = slot.get();
        update(&mut work);
        slot.set(work);
    });
}
fn measured(action: impl FnOnce()) -> Work {
    WORK.with(|slot| slot.set(Work::default()));
    action();
    WORK.with(std::cell::Cell::get)
}
fn fixture(messages: usize) -> (InteractiveShell, ShellComponent) {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    {
        let mut state = shell.state.borrow_mut();
        state.application_viewport_requested = true;
        state.render_threaded = true;
        for index in 0..messages {
            state.push_block(TranscriptBlock::Notice(format!("message-{index:04} 界é")));
        }
    }
    let component = ShellComponent::isolated(shell.state.clone(), true);
    paint(&shell, &component);
    (shell, component)
}
fn paint(shell: &InteractiveShell, component: &ShellComponent) {
    let update = component.render_update(80).unwrap();
    assert!(update.replacement.len() <= 24);
    assert!(!update.rebuild_scrollback);
    shell.state.frame_written_at(None);
}

#[test]
fn fullscreen_scroll_and_selection_work_is_viewport_bounded() {
    let mut samples = Vec::new();
    for messages in [100, 1_000] {
        let (mut shell, component) = fixture(messages);
        // Warm the reader anchor, then measure another ordinary wheel step.
        shell.scroll_lines(-3);
        paint(&shell, &component);
        let scroll = measured(|| {
            shell.scroll_lines(-3);
            paint(&shell, &component);
        });
        let down = measured(|| {
            shell.scroll_lines(3);
            paint(&shell, &component);
        });
        let page = measured(|| {
            shell.scroll(-1);
            paint(&shell, &component);
        });
        shell.select_all_transcript();
        // Extend an offscreen anchor: highlighting must not visit that prefix.
        let row = {
            let state = shell.state.borrow();
            let geometry = state.pointer_render_geometry().unwrap();
            geometry
                .blocks
                .iter()
                .find_map(|block| {
                    let row =
                        (block.start + block.surface.transition_rows + block.surface.leading_rows)
                            .checked_sub(geometry.visible_start)?;
                    (row > 0 && row < geometry.visible_lines.len()).then_some(row as u16)
                })
                .unwrap()
        };
        shell.begin_transcript_selection(row, 2, true);
        assert!(shell.state.borrow().selection_dragging);
        let drag = measured(|| {
            shell.extend_transcript_selection(row, 12);
            paint(&shell, &component);
        });
        let edge = measured(|| {
            shell.extend_transcript_selection(0, 12);
            paint(&shell, &component);
        });
        let copy = shell.selected_plain_text().unwrap();
        assert!(copy.starts_with("message-0000 界é\n\nmessage-0001 界é"));
        assert!(!copy.contains('\x1b'));
        let focus = shell
            .state
            .borrow()
            .transcript_selection
            .as_ref()
            .unwrap()
            .focus;
        let mut expected = (0..=focus.block)
            .map(|index| format!("message-{index:04} 界é"))
            .collect::<Vec<_>>();
        expected.last_mut().unwrap().truncate(focus.offset);
        assert_eq!(copy, expected.join("\n\n"));
        // Copy may visit the complete selection, including offscreen messages.
        let copied = measured(|| assert_eq!(shell.selected_plain_text().unwrap(), copy));
        eprintln!("messages={messages}: scroll={scroll:?} down={down:?} page={page:?} drag={drag:?} edge={edge:?} copy={copied:?}");
        samples.push([scroll, down, page, drag, edge]);
        let mut state = shell.state.borrow_mut();
        let selection = state.transcript_selection.as_mut().unwrap();
        std::mem::swap(&mut selection.anchor, &mut selection.focus);
        drop(state);
        assert_eq!(shell.selected_plain_text().unwrap(), copy);
    }
    for (small, large) in samples[0].iter().zip(&samples[1]) {
        assert_eq!(small.rows_laid_out, 0);
        assert_eq!(large.rows_laid_out, 0);
        assert!(large.messages_visited <= 128, "{small:?} vs {large:?}");
        assert_eq!(
            small, large,
            "fixed viewport work must not grow with history"
        );
    }
}

#[test]
fn fullscreen_unpainted_scroll_burst_preserves_every_navigation_step() {
    let (mut shell, component) = fixture(1_000);
    let work = measured(|| {
        for _ in 0..64 {
            shell.scroll_lines(-3);
        }
        assert_eq!(shell.state.borrow().scroll_from_bottom.get(), 192);
        paint(&shell, &component);
    });
    assert_eq!(shell.state.borrow().scroll_from_bottom.get(), 192);
    assert_eq!(work.rows_laid_out, 0);
    assert!(work.messages_visited <= 128, "{work:?}");
}
