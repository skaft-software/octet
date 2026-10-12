//! The model-visible context keeps only the newest tool-result screenshots.
//!
//! Part of the `session::tests` suite; the shared builders and
//! `use super::*;` preamble live in `session/tests.rs`.

use super::*;

use octet_ai::ToolResultPart;

fn png(label: &str) -> octet_ai::Media {
    octet_ai::Media::image_bytes(
        bytes::Bytes::copy_from_slice(label.as_bytes()),
        "image/png".parse().unwrap(),
    )
}

fn screenshot_result(call_id: &str, label: &str) -> EntryValue {
    EntryValue::Message(Message::User(UserMessage {
        content: vec![UserPart::ToolResult(ToolResult {
            tool_call_id: ToolCallId(call_id.to_string()),
            content: vec![
                ToolResultPart::Text(format!("window state {label}")),
                ToolResultPart::Media(png(label)),
            ],
            is_error: false,
            added_tool_names: None,
        })],
    }))
}

fn inline_label(media: &octet_ai::Media) -> String {
    let octet_ai::Media::Image(image) = media else {
        panic!("expected an image");
    };
    let octet_ai::ImageSource::Inline(data) = &image.source else {
        panic!("expected inline image bytes");
    };
    String::from_utf8(data.to_vec()).unwrap()
}

/// Every model-visible image label, oldest first, and the number of
/// placeholders standing in for omitted ones.
fn visible_images(context: &[Message]) -> (Vec<String>, usize) {
    let mut labels = Vec::new();
    let mut placeholders = 0;
    for message in context {
        let Message::User(user) = message else {
            continue;
        };
        for part in &user.content {
            match part {
                UserPart::Media(media) => labels.push(inline_label(media)),
                UserPart::ToolResult(result) => {
                    for item in &result.content {
                        match item {
                            ToolResultPart::Media(media) => labels.push(inline_label(media)),
                            ToolResultPart::Text(text) if text == TOOL_IMAGE_PLACEHOLDER => {
                                placeholders += 1;
                            }
                            ToolResultPart::Text(_) => {}
                        }
                    }
                }
                UserPart::Text(_) => {}
            }
        }
    }
    (labels, placeholders)
}

fn expected_newest(total: usize) -> Vec<String> {
    (total - MODEL_VISIBLE_TOOL_IMAGE_LIMIT..total)
        .map(|index| format!("shot-{index}"))
        .collect()
}

fn session_with_screenshots(total: usize, materialize_early: bool) -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    session.append(user("drive the desktop")).unwrap();
    if materialize_early {
        // Exercise the incremental cache path rather than a cold rebuild.
        session.context().unwrap();
    }
    for index in 0..total {
        session.append(assistant("next action")).unwrap();
        session
            .append(screenshot_result(
                &format!("call_{index}"),
                &format!("shot-{index}"),
            ))
            .unwrap();
    }
    (dir, session)
}

#[test]
fn only_the_newest_tool_screenshots_are_model_visible() {
    let total = MODEL_VISIBLE_TOOL_IMAGE_LIMIT + 5;
    for materialize_early in [true, false] {
        let (_dir, session) = session_with_screenshots(total, materialize_early);
        let (labels, placeholders) = visible_images(&session.context().unwrap());
        assert_eq!(labels, expected_newest(total), "early={materialize_early}");
        assert_eq!(placeholders, 5, "early={materialize_early}");
    }
}

#[test]
fn omitted_screenshots_keep_their_text_and_leave_the_journal_whole() {
    let total = MODEL_VISIBLE_TOOL_IMAGE_LIMIT + 2;
    let (dir, session) = session_with_screenshots(total, true);
    let context = session.context().unwrap();
    // The text beside an omitted screenshot is still sent.
    let texts: Vec<_> = context
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => Some(user),
            Message::Assistant(_) => None,
        })
        .flat_map(|user| &user.content)
        .filter_map(|part| match part {
            UserPart::ToolResult(result) => Some(result),
            _ => None,
        })
        .flat_map(|result| &result.content)
        .filter_map(|item| match item {
            ToolResultPart::Text(text) if text.starts_with("window state") => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts.len(), total);

    // The journal is unchanged, in memory and after reopening from disk.
    let journal_images = |session: &Session| {
        session
            .entries()
            .iter()
            .filter_map(|entry| match &entry.value {
                EntryValue::Message(Message::User(user)) => Some(user),
                _ => None,
            })
            .flat_map(|user| &user.content)
            .filter_map(|part| match part {
                UserPart::ToolResult(result) => Some(result),
                _ => None,
            })
            .flat_map(|result| &result.content)
            .filter(|item| matches!(item, ToolResultPart::Media(_)))
            .count()
    };
    assert_eq!(journal_images(&session), total);
    let path = temp_path(&dir);
    drop(session);
    let reopened = Session::open(path).unwrap();
    assert_eq!(journal_images(&reopened), total);
    let (labels, _) = visible_images(&reopened.context().unwrap());
    assert_eq!(labels, expected_newest(total));
}

#[test]
fn coalesced_parallel_results_and_adjacent_media_share_one_window() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    session.append(assistant("calling tools")).unwrap();
    // Older turn: result plus protocol-adjacent media.
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![
                UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId("old".into()),
                    content: vec![ToolResultPart::Text("old result".into())],
                    is_error: false,
                    added_tool_names: None,
                }),
                UserPart::Media(png("adjacent-old")),
            ],
        })))
        .unwrap();
    for index in 0..MODEL_VISIBLE_TOOL_IMAGE_LIMIT {
        session.append(assistant("again")).unwrap();
        session
            .append(screenshot_result(
                &format!("call_{index}"),
                &format!("shot-{index}"),
            ))
            .unwrap();
    }
    let (labels, placeholders) = visible_images(&session.context().unwrap());
    assert_eq!(labels, expected_newest(MODEL_VISIBLE_TOOL_IMAGE_LIMIT));
    assert_eq!(placeholders, 1);
    // The tool-result turn keeps its shape so later results still coalesce.
    let context = session.context().unwrap();
    let Message::User(first) = &context[1] else {
        panic!("expected the older tool-result turn");
    };
    assert!(first
        .content
        .iter()
        .all(|part| matches!(part, UserPart::ToolResult(_) | UserPart::Media(_))));
    assert!(first
        .content
        .iter()
        .any(|part| matches!(part, UserPart::ToolResult(_))));
    assert!(!first
        .content
        .iter()
        .any(|part| matches!(part, UserPart::Media(_))));
}

#[test]
fn user_attached_images_are_never_windowed() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp_path(&dir)).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![
                UserPart::Text("look at this".into()),
                UserPart::Media(png("user-image")),
            ],
        })))
        .unwrap();
    for index in 0..MODEL_VISIBLE_TOOL_IMAGE_LIMIT + 3 {
        session.append(assistant("act")).unwrap();
        session
            .append(screenshot_result(
                &format!("call_{index}"),
                &format!("shot-{index}"),
            ))
            .unwrap();
    }
    let (labels, placeholders) = visible_images(&session.context().unwrap());
    assert_eq!(labels.first().map(String::as_str), Some("user-image"));
    assert_eq!(labels.len(), 1 + MODEL_VISIBLE_TOOL_IMAGE_LIMIT);
    assert_eq!(placeholders, 3);
}
