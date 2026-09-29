//! Tests for the parity contract between the session-search picker and the
//! stored `Session` index: query dispatch, ranking, and selection mapping.
//!
//! Separate from tests.rs because this area reaches across the session store
//! and the octet-agent index rather than the picker state machine itself, and
//! its expectations are stated as index contracts rather than shell behaviour.

use super::*;
use octet_agent::{EntryValue, Session};
use octet_ai::{Message, UserMessage, UserPart};

#[test]
fn resume_transcript_search_dispatch_uses_index_and_returns_original_session() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    for id in ["one", "two"] {
        let mut session = Session::create(store.dir().join(format!("{id}.jsonl"))).unwrap();
        for text in [
            "ordinary first prompt",
            if id == "two" {
                "deep hidden needle"
            } else {
                "other later text"
            },
        ] {
            session
                .append(EntryValue::Message(Message::User(UserMessage {
                    content: vec![UserPart::Text(text.into())],
                })))
                .unwrap();
        }
    }
    let rows = store.list();
    assert_eq!(rows.len(), 2);
    let expected = store.dir().join("two.jsonl");
    let mut shell = InteractiveShell::test_shell();
    shell.open_panel(Panel::SessionPicker {
        picker: PickerState::new(rows, None),
    });
    for character in "needle".chars() {
        shell.panel_input(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::NONE,
        )));
    }
    shell.panel_input(&Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char('f'),
        KeyModifiers::CONTROL,
    )));
    let mut requests = shell.drain_panel_requests();
    assert_eq!(requests.len(), 1);
    let PanelRequest::SearchEntries { query, paths } = requests.remove(0) else {
        panic!("search request");
    };
    let hits = search_picker_entries(&store, &query, &paths).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits.get(&expected).unwrap().contains("deep hidden needle"));
    shell.set_picker_entry_search(query.clone(), hits);
    let repeated = search_picker_entries(&store, &query, &paths).unwrap();
    assert_eq!(repeated.len(), 1);
    let selected = shell.panel_input(&Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
    assert!(matches!(selected, Some((PanelResult::Select(ref id), _)) if id == "two"));
    assert_eq!(
        shell.take_picker_selection(),
        Some(("two".into(), expected))
    );
    assert!(search_picker_entries(&store, &"x".repeat(1025), &paths).is_err());
}
