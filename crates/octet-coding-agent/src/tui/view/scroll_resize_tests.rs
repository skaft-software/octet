//! Regressions across the immutable input -> private renderer -> written-frame boundary.
use super::super::{AssistantBlock, InteractiveShell, TranscriptBlock};
use super::*;
use sexy_tui_rs::strip_terminal_sequences;

fn component(shell: &InteractiveShell, application: bool) -> ShellComponent {
    shell.state.borrow_mut().render_threaded = true;
    ShellComponent::isolated(shell.state.clone(), application)
}

fn update(
    shell: &InteractiveShell,
    component: &ShellComponent,
    width: u16,
    rows: &mut Vec<String>,
) {
    let frame = component.render_update(width).unwrap();
    rows.truncate(frame.stable_prefix);
    rows.extend(frame.replacement);
    shell.state.frame_written();
}

#[test]
fn threaded_visual_anchor_is_promoted_before_width_reflow_in_both_mouse_modes() {
    for application in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Assistant(Box::new(
                AssistantBlock::finalized(
                    (0..256)
                        .map(|i| {
                            format!(
                "MARK_{i:03} alpha beta gamma delta epsilon zeta theta Unicode 界 👩‍💻 "
            )
                        })
                        .collect::<String>(),
                ),
            )));
        let component = component(&shell, application);
        let mut rows = component.render(80);
        shell.state.frame_written();
        shell.scroll_lines(-40);
        let fallback = shell.state.borrow().viewport_anchor.get().unwrap();
        assert!(!fallback.semantic, "input must not lay out/copy the source");
        let expected = {
            let owner = component.owner.as_ref().unwrap().borrow();
            super::super::transcript_selection::selection_position_for_visual_cell(
                &owner.state,
                fallback.fallback_visual_row,
                0,
            )
            .expect("old renderer layout must map the receipt to source text")
        };
        // Resize can overtake the pending PageUp paint. Promotion must use the
        // old renderer's layout, not the receipt's row at the new width.
        shell.set_size(64, 24);
        update(&shell, &component, 64, &mut rows);
        let first = shell.state.borrow().viewport_anchor.get().unwrap();
        assert!(
            first.semantic,
            "threaded fallback never became a text anchor"
        );
        assert_eq!(
            first.text_offset, expected.offset,
            "anchor must use the old width"
        );
        assert!(first.text_offset > 0);
        for (width, height) in [(42, 18), (80, 26), (64, 8), (64, 24)] {
            shell.set_size(width, height);
            update(&shell, &component, width, &mut rows);
            let after = shell.state.borrow().viewport_anchor.get().unwrap();
            assert_eq!(after.commit_id, first.commit_id);
            assert_eq!(after.text_offset, first.text_offset);
            assert!(after.semantic);
            assert!(strip_terminal_sequences(&rows.join("\n")).contains("MARK_"));
        }
    }
}

#[test]
fn threaded_pageup_outside_the_visible_block_receipt_resolves_before_resize() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    for i in 0..256 {
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Notice(format!("row-{i:03}")));
    }
    let component = component(&shell, false);
    let mut rows = component.render(80);
    shell.state.frame_written();
    shell.scroll_lines(-80);
    assert_eq!(
        shell
            .state
            .borrow()
            .viewport_anchor
            .get()
            .unwrap()
            .block_hint,
        usize::MAX
    );
    shell.set_size(64, 18);
    update(&shell, &component, 64, &mut rows);
    let after = shell.state.borrow().viewport_anchor.get().unwrap();
    assert!(after.semantic);
    assert_ne!(after.block_hint, usize::MAX);
}

#[test]
fn threaded_height_only_resize_reuses_historical_layout_and_lazy_strings() {
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    for i in 0..4096 {
        shell
            .state
            .borrow_mut()
            .push_block(TranscriptBlock::Notice(format!("history-{i}")));
    }
    let component = component(&shell, false);
    component.render(80);
    shell.state.frame_written();
    shell.set_size(80, 18);
    let frame = component.render_update(80).unwrap();
    assert!(frame.reanchor_viewport);
    assert!(frame.stable_prefix >= 4096);
    assert!(frame.replacement.len() < 32);
    shell.state.frame_written();
    // An away-and-back resize still requires physical repair even though
    // the final row layout and dimensions are unchanged.
    shell.set_size(78, 17);
    shell.set_size(80, 18);
    let frame = component.render_update(80).unwrap();
    assert!(frame.reanchor_viewport);
    assert!(frame.stable_prefix >= 4096);
    assert!(frame.replacement.len() < 32);
}

#[test]
fn resize_settling_coalesces_bursts_without_starving_input_or_continuous_dragging() {
    let start = Instant::now();
    let mut schedule = ResizeSchedule::new(0);
    for (epoch, ms) in [(1, 0), (2, 20), (3, 40)] {
        schedule.observe(epoch, start + Duration::from_millis(ms));
    }
    assert_eq!(
        schedule.remaining(start + Duration::from_millis(114)),
        Some(Duration::from_millis(1))
    );
    assert_eq!(
        schedule.remaining(start + Duration::from_millis(115)),
        Some(Duration::ZERO)
    );
    schedule.observe(4, start + Duration::from_millis(140));
    assert_eq!(
        schedule.remaining(start + Duration::from_millis(150)),
        Some(Duration::ZERO)
    );
    // A paint forced by input consumes the pending resize too, rather than
    // leaving a redundant settled repaint queued behind the editable owner.
    schedule.painted();
    assert_eq!(schedule.remaining(start + Duration::from_millis(151)), None);
    schedule.observe(5, start + Duration::from_millis(152));
    assert_eq!(
        schedule.remaining(start + Duration::from_millis(152)),
        Some(RESIZE_SETTLE_INTERVAL)
    );
}
