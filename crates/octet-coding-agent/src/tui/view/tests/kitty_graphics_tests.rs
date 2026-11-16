//! Full terminal-byte frames, including opaque PNG expansion in OctetTerminal.
use super::*;
use crate::tui::terminal::graphics_tests::{
    self,
    model::{Kitty, Placement},
    Frames,
};
use sexy_tui_rs::{ImageAnchor, ImageProtocol, TerminalImage};

const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;
pub(crate) const PNG: &[u8] = include_bytes!("../../terminal/fixtures/screenshot.png");

pub(crate) struct Probe {
    frames: Frames,
    model: Kitty,
    pub(crate) top_crop: bool,
    pub(crate) bottom_crop: bool,
    image_frames: usize,
}

pub(crate) fn attach(shell: &mut InteractiveShell) -> Probe {
    if let Some(mut tui) = shell.tui.take() {
        tui.stop();
    }
    shell.set_size(WIDTH, HEIGHT);
    shell.set_tui_fullscreen(true).unwrap();
    shell.capture_mouse = true;
    shell.state.borrow_mut().application_viewport_requested = true;
    let (terminal, frames) = graphics_tests::terminal(
        shell.size.clone(),
        shell.state.borrow().terminal_images.clone(),
    );
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_alternate_screen(true);
    tui.set_show_hardware_cursor(true);
    tui.add_child(Box::new(ShellComponent::new(shell.state.clone(), true)));
    tui.start();
    shell.tui = Some(tui);
    let mut probe = Probe {
        frames,
        model: Kitty::new(HEIGHT, WIDTH),
        top_crop: false,
        bottom_crop: false,
        image_frames: 0,
    };
    probe.check(shell);
    probe
}

impl Probe {
    fn check(&mut self, shell: &InteractiveShell) {
        let mut expected = Vec::new();
        let lines = shell.tui.as_ref().unwrap().rendered_frame();
        let top = lines.len().saturating_sub(usize::from(HEIGHT));
        for (row, line) in lines[top..].iter().enumerate() {
            let mut offset = 0;
            for anchor in ImageAnchor::parse_all(line) {
                assert_eq!(anchor.protocol(), ImageProtocol::Kitty);
                let marker = anchor.marker();
                let start = offset + line[offset..].find(&marker).unwrap();
                offset = start + marker.len();
                let column = anchor
                    .column()
                    .unwrap_or_else(|| visible_width(&line[..start]) as u16);
                if let Some(crop) = anchor.crop() {
                    self.top_crop |= crop.skip_rows() > 0;
                    self.bottom_crop |=
                        crop.skip_rows() + crop.shown_rows() < anchor.layout().rows();
                }
                expected.push(Placement {
                    image: anchor.id().get(),
                    placement: None,
                    row: row as u16,
                    column,
                    rows: anchor.placed_rows(),
                    columns: anchor.layout().columns(),
                });
            }
        }
        self.image_frames += usize::from(!expected.is_empty());
        // Each call follows exactly one paint. Lifecycle entry is also fed to
        // the model; unlike vt100 alone it retains the separate graphics plane.
        let mut frames = self.frames.lock().unwrap();
        assert!(
            !frames.is_empty(),
            "paint must reach the real output adapter"
        );
        for frame in frames.drain(..) {
            self.model.feed(&frame);
            self.model.assert_placements(&expected);
        }
    }

    pub(crate) fn render(&mut self, shell: &mut InteractiveShell) {
        shell.render();
        self.check(shell);
    }
}

pub(crate) fn seed(shell: &mut InteractiveShell) {
    let mut state = shell.state.borrow_mut();
    state.update_image_rendering(
        true,
        ImageCapabilities::forced(Some(ImageProtocol::Kitty), None),
    );
    for number in 0..4 {
        state.push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized(format!(
                "before screenshot {number}\n\nRead rows remain readable"
            )),
        )));
        let images = state.register_tool_images(vec![ToolResultImage::Ready {
            image: Arc::new(TerminalImage::from_bytes(PNG.to_vec()).unwrap()),
            id: None,
        }]);
        let args = serde_json::json!({"path": format!("screenshot-{number}.png")});
        let mut panel = ToolPanel::new(
            ToolCallId(format!("screenshot-{number}")),
            "read".into(),
            args.to_string(),
            summarize_tool("read", &args),
            String::new(),
            true,
            false,
            None,
            None,
        );
        panel.images = images;
        panel.image_rendering = state.image_rendering;
        state.push_block(TranscriptBlock::Tool(Box::new(panel)));
    }
}

pub(crate) fn exercise(shell: &mut InteractiveShell, probe: &mut Probe, streaming: bool) {
    let run = streaming.then(|| shell.begin_run("offline-fixture"));
    probe.render(shell);
    // Row steps expose every top/bottom crop; page and coalesced wheel bursts
    // cross multiple images. No scroll policy is reimplemented in this test.
    for index in 0..160 {
        if let Some(run) = run {
            shell.on_run_event(
                run,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: format!("stream row {index}\n\n"),
                },
            );
            probe.render(shell);
            shell.state.borrow_mut().advance_status_shimmer();
            probe.render(shell);
        }
        shell.scroll_lines(if index < 80 { -1 } else { 1 });
        probe.render(shell);
    }
    use crate::tui::keymap::InputAction;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    for key in [
        KeyCode::PageUp,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::PageDown,
    ] {
        let action = shell.translate_input(
            Some(Event::Key(KeyEvent::new(key, KeyModifiers::NONE))),
            streaming,
        );
        // Page bindings apply semantic navigation and paint inside dispatch.
        // Check that very frame rather than adding a second render afterward.
        assert_eq!(action, InputAction::Ignore);
        probe.check(shell);
    }
    // Decoded SGR button 64/65 wheel packets, coalesced before painting.
    for kind in [
        MouseEventKind::ScrollUp,
        MouseEventKind::ScrollDown,
        MouseEventKind::ScrollUp,
        MouseEventKind::ScrollDown,
    ] {
        for _ in 0..16 {
            let action = shell.translate_input(
                Some(Event::Mouse(MouseEvent {
                    kind,
                    column: 10,
                    row: 5,
                    modifiers: KeyModifiers::NONE,
                })),
                streaming,
            );
            let InputAction::ScrollLines(direction) = action else {
                panic!("unexpected wheel action: {action:?}");
            };
            shell.scroll_lines(direction);
        }
        probe.render(shell);
    }
    assert!(probe.image_frames > 10, "fixture never displayed images");
    assert!(
        probe.top_crop && probe.bottom_crop,
        "fixture missed a viewport edge"
    );
}

#[test]
fn kitty_graphics_native_idle_scroll_and_fast_wheel_bursts() {
    let mut shell = InteractiveShell::test_shell();
    let mut probe = attach(&mut shell);
    seed(&mut shell);
    exercise(&mut shell, &mut probe, false);
}

#[test]
fn kitty_graphics_native_streaming_scroll_and_status_ticks() {
    let mut shell = InteractiveShell::test_shell();
    let mut probe = attach(&mut shell);
    seed(&mut shell);
    exercise(&mut shell, &mut probe, true);
}
