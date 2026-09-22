use super::super::{InteractiveShell, TranscriptBlock};
use super::*;

struct RecordingTerminal(Arc<Mutex<String>>);

impl sexy_tui_rs::Terminal for RecordingTerminal {
    fn start_events(&mut self, _: Box<dyn FnMut(sexy_tui_rs::TerminalInput)>, _: Box<dyn FnMut()>) {
    }
    fn stop(&mut self) {
        self.0.lock().unwrap().push_str("TERMINAL-STOP");
    }
    fn write(&mut self, data: &str) {
        let mut bytes = self.0.lock().unwrap();
        assert!(!bytes.contains("TERMINAL-STOP"), "write after restoration");
        bytes.push_str(data);
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
    fn capabilities(&self) -> sexy_tui_rs::TerminalCapabilities {
        sexy_tui_rs::TerminalCapabilities::interactive(sexy_tui_rs::ColorDepth::TrueColor, true)
    }
}

#[test]
fn renderer_shutdown_flushes_final_notice_once_before_restoration() {
    for queued_render in [false, true] {
        for suspend in [false, true] {
            let mut shell = InteractiveShell::test_shell();
            shell.set_size(80, 24);
            shell.tui.take().unwrap().stop();
            shell
                .state
                .borrow_mut()
                .push_block(TranscriptBlock::Notice("INITIAL-NOTICE".into()));
            let bytes = Arc::new(Mutex::new(String::new()));
            let (tx, rx) = mpsc::channel();
            let (ack_tx, ack_rx) = mpsc::channel();
            render_loop_with_terminal(
                RecordingTerminal(bytes.clone()),
                shell.state.clone(),
                shell.size.clone(),
                rx,
                false,
                false,
                false,
                |state, _| {
                    // Idle resize polling happens after the initial paint. Queue
                    // both commands here, without a thread/sleep race, so Stop
                    // is consumed by the coalescer when Render precedes it.
                    assert!(bytes.lock().unwrap().contains("INITIAL-NOTICE"));
                    state
                        .borrow_mut()
                        .push_block(TranscriptBlock::Notice("FINAL-COMPLETED-NOTICE".into()));
                    if queued_render {
                        tx.send(RenderCommand::Render).unwrap();
                    }
                    tx.send(if suspend {
                        RenderCommand::Suspend(ack_tx.clone())
                    } else {
                        RenderCommand::Stop
                    })
                    .unwrap();
                    false
                },
            );
            let output = bytes.lock().unwrap();
            assert_eq!(
                output.matches("FINAL-COMPLETED-NOTICE").count(),
                1,
                "{output}"
            );
            assert_eq!(output.matches("TERMINAL-STOP").count(), 1, "{output}");
            assert!(output.find("FINAL-COMPLETED-NOTICE") < output.find("TERMINAL-STOP"));
            if suspend {
                ack_rx.try_recv().expect("suspend acknowledgement");
                assert!(ack_rx.try_recv().is_err());
            }
        }
    }
}
