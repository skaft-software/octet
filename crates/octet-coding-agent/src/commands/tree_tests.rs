use super::*;

#[test]
fn tree_is_a_discoverable_exact_and_unique_prefix_command() {
    assert_eq!(parse("/tree"), Command::Tree);
    assert_eq!(parse("/tr"), Command::Tree);
    assert!(matches!(parse("/tree extra"), Command::Unknown(_)));
    assert_eq!(slash_suggestions("/tr")[0].name, "tree");
    let help = help_text(Path::new("."), Some("tree"));
    assert!(help.contains("/tree — navigate branches in this session"));
    assert!(!help.contains("/checkout"));
    let fork_help = help_text(Path::new("."), Some("fork"));
    assert!(fork_help.contains("new session"));
    assert_eq!(parse("/fork"), Command::Fork);
}

#[test]
fn portability_commands_parse_literal_quoted_paths_and_require_attended_share() {
    assert_eq!(
        parse("/import session.jsonl"),
        Command::Import("session.jsonl".into())
    );
    assert_eq!(
        parse("/import \"session with spaces.json\""),
        Command::Import("session with spaces.json".into())
    );
    assert_eq!(
        parse("/import 'session with spaces.jsonl'"),
        Command::Import("session with spaces.jsonl".into())
    );
    for invalid in [
        "/import",
        "/import a b",
        "/import \"unfinished",
        "/import \"a\" extra",
        "/import a\nb",
        "/share --yes",
        "/share private",
    ] {
        assert!(matches!(parse(invalid), Command::Unknown(_)), "{invalid}");
    }
    assert_eq!(parse("/share"), Command::Share);
    assert_eq!(slash_suggestions("/imp")[0].name, "import");
    assert_eq!(slash_suggestions("/sha")[0].name, "share");
    assert!(help_text(Path::new("."), Some("import")).contains("new session"));
    assert!(help_text(Path::new("."), Some("share")).contains("unlisted"));
    assert!(help_text(Path::new("."), Some("export")).contains("HTML"));
}

#[test]
fn frontend_export_defaults_to_html_but_explicit_data_suffixes_keep_the_whole_graph() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("portable.jsonl");
    let mut session = Session::create(&path).unwrap();
    let root = session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![octet_ai::UserPart::Text("shared root".into())],
        })))
        .unwrap();
    let abandoned = session
        .append(EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap();
    session.checkout(root).unwrap();
    let before = std::fs::read(&path).unwrap();
    let store =
        crate::session_store::SessionStore::for_directory(directory.path(), directory.path());
    let html = export_for_frontend(&store, "portable", None, directory.path(), "light").unwrap();
    assert_eq!(html.destination, directory.path().join("portable.html"));
    let text = std::fs::read_to_string(&html.destination).unwrap();
    assert!(text.starts_with("<!doctype html>"));
    assert!(text.contains("color-scheme:light"));
    assert!(
        export_for_frontend(&store, "portable", None, directory.path(), "light").is_err(),
        "existing exports must not be overwritten"
    );
    let json = export_for_frontend(
        &store,
        "portable",
        Some("'local data.json'".into()),
        directory.path(),
        "dark",
    )
    .unwrap();
    let package: serde_json::Value =
        serde_json::from_slice(&std::fs::read(json.destination).unwrap()).unwrap();
    assert_eq!(package["format"], "octet-session-export");
    assert_eq!(package["redacted"], true);
    assert!(package["records"]
        .as_array()
        .unwrap()
        .iter()
        .any(|record| record["id"] == abandoned.0));
    let jsonl = export_for_frontend(
        &store,
        "portable",
        Some("local.jsonl".into()),
        directory.path(),
        "dark",
    )
    .unwrap();
    let reopened = Session::open_read_only(jsonl.destination).unwrap();
    assert_eq!(reopened.head(), session.head());
    assert_eq!(reopened.entries().len(), session.entries().len());
    assert!(reopened.entry(&abandoned).is_some());
    assert_eq!(std::fs::read(path).unwrap(), before);
}
