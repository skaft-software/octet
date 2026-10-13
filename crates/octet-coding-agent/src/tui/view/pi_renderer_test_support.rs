//! Test-only projection of a real App shell through the production Tern consumer.
//! The sink is in memory: this is native protocol/tree coverage, not a PTY or GUI.
use super::*;
use crate::tui::view::InteractiveShell;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<String>>>);
impl io::Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap()
            .push(String::from_utf8(bytes.to_vec()).unwrap());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn project(shell: &InteractiveShell) -> Vec<Node> {
    let output = Output::default();
    let client =
        TernClient::with_writer("pi-renderer-acceptance", None, &["edit"], output.clone()).unwrap();
    let mut surface = TernSurface::with_client(client);
    let hello = octet_tern::frame::decode_body(
        "r",
        &json!({"r":"hello", "v":1, "term":"test", "kinds":octet_tern::wire::TSP_KINDS,
            "credits":8, "dark":true})
        .to_string(),
    )
    .unwrap();
    surface.observe(&hello).unwrap();
    surface.flush(&shell.state).unwrap();
    assert!(
        output
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|wire| { octet_tern::frame::split(wire).is_some_and(|raw| raw.verb == "f") }),
        "production Tern consumer must publish a native frame"
    );
    surface.sent.main.clone()
}
