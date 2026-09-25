//! Script-free, self-contained HTML projection of the redacted portable export.
use base64::Engine;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use serde_json::Value;

/// Rich text is parsed only inside a bounded envelope; large/deep messages keep
/// their complete literal representation rather than being truncated.
fn markdown(text: &str, html: &mut String) {
    let text = sexy_tui_rs::sanitize_text(text, sexy_tui_rs::SanitizeOptions::default());
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut depth = 0usize;
    let too_deep = text.len() > 512 * 1024
        || Parser::new_ext(&text, options).any(|event| {
            match event {
                Event::Start(_) => depth += 1,
                Event::End(_) => depth = depth.saturating_sub(1),
                _ => {}
            }
            depth > 64
        });
    if too_deep {
        html.push_str(&format!("<pre>{}</pre>", escape(&text)));
        return;
    }
    // Author markup and destinations are data, never executable HTML or URLs.
    // Only our structured Media path below may produce an image element.
    let mut code: Option<(String, String)> = None;
    let events = Parser::new_ext(&text, options).filter_map(|event| match event {
        Event::Start(Tag::CodeBlock(kind)) => {
            let language = match kind {
                pulldown_cmark::CodeBlockKind::Fenced(label) => {
                    label.split_whitespace().next().unwrap_or("").to_owned()
                }
                pulldown_cmark::CodeBlockKind::Indented => String::new(),
            };
            code = Some((language, String::new()));
            None
        }
        Event::Text(text) if code.is_some() => {
            code.as_mut().unwrap().1.push_str(&text);
            None
        }
        Event::End(TagEnd::CodeBlock) => {
            let (language, source) = code.take().expect("balanced Markdown code block");
            Some(Event::Html(highlighted_code(&source, &language).into()))
        }
        Event::Html(text) | Event::InlineHtml(text) => Some(Event::Text(text)),
        Event::Start(Tag::Link { .. } | Tag::Image { .. })
        | Event::End(TagEnd::Link | TagEnd::Image) => None,
        Event::TaskListMarker(checked) => {
            Some(Event::Text(if checked { "[x] " } else { "[ ] " }.into()))
        }
        other => Some(other),
    });
    pulldown_cmark::html::push_html(html, events);
}

fn highlighted_code(source: &str, language: &str) -> String {
    let mut html = String::from("<pre><code>");
    if let Some(lines) = sexy_tui_rs::rich_text::highlight_code(source, language) {
        for (index, line) in lines.into_iter().enumerate() {
            if index != 0 {
                html.push('\n');
            }
            for region in line {
                use sexy_tui_rs::TextRole;
                let class = match region.role {
                    Some(TextRole::SyntaxComment) => "syntax-comment",
                    Some(TextRole::SyntaxString) => "syntax-string",
                    Some(TextRole::SyntaxNumber) => "syntax-number",
                    Some(TextRole::SyntaxFunction) => "syntax-function",
                    Some(TextRole::SyntaxKeyword) => "syntax-keyword",
                    Some(TextRole::SyntaxType) => "syntax-type",
                    Some(TextRole::SyntaxOperator) => "syntax-operator",
                    _ => "",
                };
                let text = escape(&region.text);
                if class.is_empty() {
                    html.push_str(&text);
                } else {
                    html.push_str(&format!("<span class=\"{class}\">{text}</span>"));
                }
            }
        }
    } else {
        html.push_str(&escape(source));
    }
    html.push_str("</code></pre>\n");
    html
}

fn details(label: &str, value: &Value, html: &mut String) -> anyhow::Result<()> {
    html.push_str(&format!(
        "<details><summary>{}</summary><pre>{}</pre></details>\n",
        escape(label),
        escape(&serde_json::to_string_pretty(value)?)
    ));
    Ok(())
}

fn message(record: &Value) -> Option<(&str, &Value)> {
    if record["type"] != "entry" || record["value"]["type"] != "message" {
        return None;
    }
    for (key, label) in [("User", "User"), ("Assistant", "Assistant")] {
        if let Some(parts) = record["value"][key]["content"].as_array() {
            let label =
                if key == "User" && parts.iter().all(|part| part.get("ToolResult").is_some()) {
                    "Tool result"
                } else {
                    label
                };
            return Some((label, &record["value"][key]));
        }
    }
    None
}

fn message_parts(message: &Value, html: &mut String) -> anyhow::Result<()> {
    if let Some(parts) = message["content"].as_array() {
        for part in parts {
            if let Some(text) = part["Text"].as_str() {
                markdown(text, html);
            } else if let Some(call) = part.get("ToolCall") {
                html.push_str(&format!(
                    "<details open><summary>Tool call: {}</summary><pre>{}</pre></details>\n",
                    escape(call["name"].as_str().unwrap_or("unknown")),
                    escape(call["arguments_json"].as_str().unwrap_or(""))
                ));
            } else if let Some(result) = part.get("ToolResult") {
                html.push_str(&format!(
                    "<details open><summary>Tool result{}: {}</summary>\n",
                    if result["is_error"] == true {
                        " (error)"
                    } else {
                        ""
                    },
                    escape(result["tool_call_id"].as_str().unwrap_or("unknown"))
                ));
                if let Some(content) = result["content"].as_array() {
                    for item in content {
                        if let Some(text) = item["Text"].as_str() {
                            html.push_str(&format!("<pre>{}</pre>\n", escape(text)));
                        }
                    }
                }
                html.push_str("</details>\n");
            } else if let Some(text) = part["Reasoning"]["text"].as_str() {
                html.push_str("<details><summary>Reasoning</summary>\n");
                markdown(text, html);
                html.push_str("</details>\n");
            }
        }
    }
    Ok(())
}

fn escape(text: &str) -> String {
    let safe = sexy_tui_rs::sanitize_text(text, sexy_tui_rs::SanitizeOptions::default());
    safe.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Only inline raster images become active markup. URLs, provider references,
/// audio, SVG and unrecognized payloads are never fetched or executed.
fn media_preview(value: &mut Value, html: &mut String) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(image) = object.get_mut("Image") {
        let mime = image["media_type"].as_str().unwrap_or("");
        if let Some(data) = image["source"]["Inline"].as_str() {
            if data.len() <= 7 * 1024 * 1024 {
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                    let valid = bytes.len() <= 5 * 1024 * 1024
                        && sexy_tui_rs::TerminalImage::from_slice(&bytes).is_ok_and(|image| {
                            use sexy_tui_rs::ImageFormat;
                            mime == match image.format() {
                                ImageFormat::Png => "image/png",
                                ImageFormat::Jpeg => "image/jpeg",
                                ImageFormat::Gif => "image/gif",
                                ImageFormat::Webp => "image/webp",
                            }
                        });
                    if valid {
                        // Re-encoding admits only base64 alphabet bytes into attributes.
                        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                        html.push_str(&format!("<img alt=\"Session image\" loading=\"lazy\" src=\"data:{mime};base64,{data}\">\n"));
                    }
                }
            }
        }
        *image = Value::String(
            "[image: inline preview when supported; external references never fetched]".into(),
        );
    }
    if let Some(audio) = object.get_mut("Audio") {
        *audio = Value::String(
            "[audio omitted from HTML; review the private session separately]".into(),
        );
    }
}

fn images(record: &mut Value, html: &mut String) {
    if record["type"] != "entry" || record["value"]["type"] != "message" {
        return;
    }
    // Image/Audio keys inside arbitrary tool or extension metadata are data,
    // not typed Media. Preserve those values and never activate their payloads.
    for role in ["User", "Assistant"] {
        let Some(parts) = record
            .get_mut("value")
            .and_then(|value| value.get_mut(role))
            .and_then(|message| message.get_mut("content"))
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        for part in parts {
            if let Some(media) = part.get_mut("Media") {
                media_preview(media, html);
            } else if let Some(results) = part
                .get_mut("ToolResult")
                .and_then(|result| result.get_mut("content"))
                .and_then(Value::as_array_mut)
            {
                for result in results {
                    if let Some(media) = result.get_mut("Media") {
                        media_preview(media, html);
                    }
                }
            }
        }
    }
}

pub(crate) fn render(package: &Value, theme: &str) -> anyhow::Result<Vec<u8>> {
    let (scheme, background, card, foreground, muted) = match theme {
        "light" => ("light", "#f4f5f7", "#ffffff", "#17202a", "#52616b"),
        _ => ("dark", "#15171c", "#20242c", "#ecedf0", "#b1bac7"),
    };
    let (keyword, string, number, function, ty) = match theme {
        "light" => ("#cf222e", "#0a3069", "#0550ae", "#8250df", "#953800"),
        _ => ("#ff7b72", "#a5d6ff", "#79c0ff", "#d2a8ff", "#ffa657"),
    };
    let title = package["metadata"]["name"]
        .as_str()
        .or_else(|| package["source_title"].as_str())
        .unwrap_or("octet session");
    let mut html = format!("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\">\n<title>{}</title>\n<style>:root{{color-scheme:{scheme};background:{background};color:{foreground}}}body{{max-width:72rem;margin:2rem auto;padding:0 1rem;font:16px system-ui}}article{{background:{card};padding:1rem;margin:1rem 0;border-radius:.5rem}}pre{{white-space:pre-wrap;overflow-wrap:anywhere}}code{{font-family:ui-monospace,monospace}}blockquote{{border-left:.2rem solid {muted};margin-left:0;padding-left:1rem}}table{{border-collapse:collapse;max-width:100%}}th,td{{border:1px solid {muted};padding:.35rem}}a{{color:inherit}}summary{{cursor:pointer}}.notice{{color:{muted}}}img{{max-width:100%;height:auto}}.syntax-comment{{color:{muted}}}.syntax-keyword,.syntax-operator{{color:{keyword}}}.syntax-string{{color:{string}}}.syntax-number{{color:{number}}}.syntax-function{{color:{function}}}.syntax-type{{color:{ty}}}</style></head><body>\n<h1>{}</h1>\n<p class=\"notice\">Private session export. Redaction is not proof of secret-free content. Images may contain sensitive information. Raw HTML, external links and terminal controls are inert; Markdown is formatted, while audio and remote media are omitted. Accounting values are known subtotals if any usage_uncertainty record exists or a usage record has no price.</p>\n", escape(title), escape(title));
    // Project metadata directly: cloning the package first also duplicates
    // every transcript/media record only to immediately discard that copy.
    let metadata = Value::Object(
        package
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("export package must be an object"))?
            .iter()
            .filter(|(key, _)| key.as_str() != "records")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    details("Export metadata", &metadata, &mut html)?;
    let records = package["records"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("export records must be an array"))?;
    let anchors: std::collections::HashMap<_, _> = records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| {
            (record["type"] == "entry")
                .then(|| record["id"].as_str().map(|id| (id, index)))
                .flatten()
        })
        .collect();
    html.push_str("<nav aria-label=\"Conversation\"><details><summary>Conversation index (first 256 messages)</summary><ol>\n");
    for (index, record, label) in records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| message(record).map(|(label, _)| (index, record, label)))
        .take(256)
    {
        html.push_str(&format!(
            "<li><a href=\"#entry-{index}\">{}: {}</a></li>\n",
            label,
            escape(record["id"].as_str().unwrap_or("message"))
        ));
    }
    html.push_str("</ol></details></nav>\n");
    for (index, record) in records.iter().enumerate() {
        let mut record = record.clone();
        let mut previews = String::new();
        images(&mut record, &mut previews);
        html.push_str(&format!("<article id=\"entry-{index}\">\n"));
        if let Some((label, content)) = message(&record) {
            html.push_str(&format!("<h2>{label}</h2>\n"));
            if let Some(parent) = record["parent"].as_str().and_then(|id| anchors.get(id)) {
                html.push_str(&format!(
                    "<p class=\"notice\"><a href=\"#entry-{parent}\">Parent entry</a></p>\n"
                ));
            }
            message_parts(content, &mut html)?;
        } else if record["type"] == "usage_uncertainty" {
            html.push_str("<h2>Usage uncertain</h2><p>Recorded costs and tokens are known subtotals, not exact totals.</p>\n");
        }
        html.push_str(&previews);
        details("Session record", &record, &mut html)?;
        html.push_str("</article>\n");
    }
    html.push_str("</body></html>\n");
    Ok(html.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn media_record(media: octet_ai::Media) -> Value {
        json!({"type": "entry", "id": "media-fixture", "parent": null,
        "value": octet_agent::EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage { content: vec![octet_ai::UserPart::Media(media)] }
        ))})
    }

    #[test]
    fn metadata_projection_preserves_all_non_record_fields() {
        let package = json!({
            "metadata": {"name": "fixture"}, "source_title": "original",
            "future_metadata": {"nested": ["retained", 42]},
            "records": [{"type": "config", "sentinel": "record-only-content"}],
        });
        let mut expected = package.clone();
        expected.as_object_mut().unwrap().remove("records");
        let mut expected_details = String::new();
        details("Export metadata", &expected, &mut expected_details).unwrap();
        let html = String::from_utf8(render(&package, "light").unwrap()).unwrap();
        assert!(html.contains(&expected_details));
        assert_eq!(html.matches("record-only-content").count(), 1);
        assert_eq!(package["records"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn rich_markdown_matches_reviewable_static_golden() {
        let mut html = String::new();
        markdown("# Report\n\nA **bold** and *clear* answer with `code`.\n\n- first\n- second\n\n```rust\nfn main() {}\n```\n", &mut html);
        assert_eq!(
            html,
            include_str!("../../tests/fixtures/export_html/rich-markdown.html")
        );
    }

    #[test]
    fn rich_export_uses_real_session_record_shape_and_preserves_branches_and_tools() {
        let directory = tempfile::tempdir().unwrap();
        let mut session =
            octet_agent::Session::create(directory.path().join("rich.jsonl")).unwrap();
        let user = session
            .append(octet_agent::EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text("## Please explain".into())],
                },
            )))
            .unwrap();
        session
            .append(octet_agent::EntryValue::Message(
                octet_ai::Message::Assistant(octet_ai::AssistantMessage {
                    content: vec![
                        octet_ai::AssistantPart::Text("**A clear answer**".into()),
                        octet_ai::AssistantPart::ToolCall(octet_ai::ToolCall {
                            async_execution: false,
                            id: octet_ai::ToolCallId("call-1".into()),
                            name: "read".into(),
                            arguments_json: "{\"path\":\"README.md\"}".into(),
                            argument_error: None,
                        }),
                    ],
                    model: octet_ai::ModelId("fixture".into()),
                    protocol: octet_ai::Protocol::OpenAiChat,
                }),
            ))
            .unwrap();
        session
            .append(octet_agent::EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::ToolResult(octet_ai::ToolResult {
                        tool_call_id: octet_ai::ToolCallId("call-1".into()),
                        content: vec![octet_ai::ToolResultPart::Text(
                            "<script>tool output</script>".into(),
                        )],
                        is_error: true,
                        added_tool_names: None,
                    })],
                },
            )))
            .unwrap();
        session.checkout(user).unwrap();
        session
            .append(octet_agent::EntryValue::Message(
                octet_ai::Message::Assistant(octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::Text("Alternative branch".into())],
                    model: octet_ai::ModelId("fixture".into()),
                    protocol: octet_ai::Protocol::OpenAiChat,
                }),
            ))
            .unwrap();
        let records: Vec<Value> = std::fs::read_to_string(session.path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let root_index = records
            .iter()
            .position(|record| record["type"] == "entry" && record["parent"].is_null())
            .unwrap();
        let package = json!({"metadata": {}, "records": records});
        for theme in ["light", "dark"] {
            let html = String::from_utf8(render(&package, theme).unwrap()).unwrap();
            assert!(html.contains("<h2>User</h2>"));
            assert!(html.contains("<h2>Assistant</h2>"));
            assert!(html.contains("<h2>Please explain</h2>"));
            assert!(html.contains("<strong>A clear answer</strong>"));
            assert!(html.contains("Tool call: read"));
            assert!(html.contains("Tool result (error): call-1"));
            assert!(html.contains("Alternative branch"));
            assert_eq!(html.matches(">Parent entry</a>").count(), 3);
            assert_eq!(
                html.matches(&format!("href=\"#entry-{root_index}\">Parent entry</a>"))
                    .count(),
                2
            );
            assert!(!html.contains("<script>"));
            assert!(html.contains("&lt;script&gt;tool output&lt;/script&gt;"));
        }
    }

    #[test]
    fn markdown_cannot_introduce_external_resources_or_active_markup() {
        let mut html = String::new();
        markdown("[danger](javascript:alert%281%29) ![alt](https://example.invalid/a.png)\n\n<script>alert(1)</script>\n\n<svg onload=evil></svg>\n\n| a | b |\n| - | - |\n| **x** | y |\n", &mut html);
        assert!(html.contains("danger"));
        assert!(html.contains("alt"));
        assert!(html.contains("<table>"));
        assert!(html.contains("<strong>x</strong>"));
        for active in ["<script", "<svg", "<img", "<a ", "javascript:", "https://"] {
            assert!(!html.contains(active), "active fragment {active}: {html}");
        }
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn highlighted_source_is_escaped_and_bounded_with_literal_fallback() {
        let source = "fn main() { println!(\"<script>evil</script>\"); }\n";
        let html = highlighted_code(source, "rust");
        assert!(html.contains("class=\"syntax-"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.ends_with("\n</code></pre>\n"));
        for (source, language) in [
            (source.to_owned(), "unknown-language"),
            ("x".repeat(8193), "rust"),
        ] {
            assert_eq!(
                highlighted_code(&source, language),
                format!("<pre><code>{}</code></pre>\n", escape(&source))
            );
        }
        let mut html = String::new();
        markdown("```rust\" onmouseover=\"evil\n<script>\n```", &mut html);
        assert!(!html.contains("onmouseover"));
        assert!(!html.contains("<script>"));
    }

    #[test]
    fn oversized_or_deep_markdown_keeps_literal_content_without_parsing() {
        for text in [
            format!("{}**tail**", "x".repeat(512 * 1024)),
            format!("{}deep", "> ".repeat(70)),
        ] {
            let mut html = String::new();
            markdown(&text, &mut html);
            assert_eq!(html, format!("<pre>{}</pre>", escape(&text)));
        }
    }

    #[test]
    fn safe_markup_ansi_media_and_both_themes_are_deterministic() {
        let image = octet_ai::Media::image_url(
            "https://private.example/secret".parse().unwrap(),
            Some("image/svg+xml".parse().unwrap()),
        );
        let fixture = json!({"source_title":"<script>alert('x')</script>\u{1b}]52;c;secret\u{7}","metadata":{},"records":[{"type":"usage_uncertainty"},media_record(image),{"text":"<img src=x onerror=evil> & **literal markdown**"}]});
        for theme in ["light", "dark"] {
            let bytes = render(&fixture, theme).unwrap();
            let html = String::from_utf8(bytes).unwrap();
            assert!(!html.contains("<script>"));
            assert!(!html.contains("\u{1b}"));
            assert!(!html.contains("private.example"));
            assert!(html.contains("&lt;img src=x onerror=evil&gt;"));
            assert!(html.contains(&format!("color-scheme:{theme}")));
            assert_eq!(render(&fixture, theme).unwrap(), html.as_bytes());
        }
    }
    #[test]
    fn previews_only_consume_typed_media_and_preserve_arbitrary_metadata() {
        use octet_ai::{
            AssistantMessage, AssistantPart, Message, ToolResult, ToolResultPart, UserMessage,
            UserPart,
        };
        let image = octet_ai::Media::image_bytes(
            bytes::Bytes::from_static(include_bytes!(
                "../../tests/fixtures/export_html/one-pixel.png"
            )),
            mime::IMAGE_PNG,
        );
        let mut user = media_record(image.clone());
        user["metadata"] = json!({
            "Image": "asset classification", "Audio": "description",
            "nested": {"Media": image.clone()}
        });
        let assistant = json!({"type": "entry", "value": octet_agent::EntryValue::Message(
            Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Media(image.clone())],
                model: octet_ai::ModelId("fixture".into()),
                protocol: octet_ai::Protocol::OpenAiChat,
            })
        )});
        let tool = json!({"type": "entry", "value": octet_agent::EntryValue::Message(
            Message::User(UserMessage { content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: octet_ai::ToolCallId("image-tool".into()),
                content: vec![ToolResultPart::Media(image)],
                is_error: false,
                added_tool_names: None,
            })] })
        )});
        let package = json!({"metadata": {}, "records": [user, assistant, tool]});
        let before = package.clone();
        let html = String::from_utf8(render(&package, "dark").unwrap()).unwrap();
        assert_eq!(html.matches("<img ").count(), 3);
        assert!(html.contains("asset classification"));
        assert!(html.contains("description"));
        // The metadata's Image-shaped value remains literal raw JSON, not a
        // fourth preview or an omission marker. The source package is untouched.
        assert!(html.contains("&quot;Inline&quot;"));
        assert_eq!(package, before);
    }

    #[test]
    fn bounded_index_does_not_truncate_later_messages() {
        let records: Vec<_> = (0..257).map(|index| json!({
            "type": "entry", "id": format!("message-{index}"),
            "value": octet_agent::EntryValue::Message(octet_ai::Message::User(
                octet_ai::UserMessage { content: vec![octet_ai::UserPart::Text(format!("tail-{index}"))] }
            ))
        })).collect();
        let html = String::from_utf8(
            render(&json!({"metadata": {}, "records": records}), "dark").unwrap(),
        )
        .unwrap();
        assert_eq!(html.matches("<li><a href=\"#entry-").count(), 256);
        assert!(html.contains("<article id=\"entry-256\">"));
        assert!(html.contains("<p>tail-256</p>"));
    }

    #[test]
    fn invalid_or_mislabeled_images_never_become_active_markup() {
        let png = include_bytes!("../../tests/fixtures/export_html/one-pixel.png");
        for (bytes, mime) in [
            (b"\x89PNG\r\n\x1a\n".as_slice(), mime::IMAGE_PNG),
            (png.as_slice(), mime::IMAGE_JPEG),
        ] {
            let image = octet_ai::Media::image_bytes(bytes::Bytes::copy_from_slice(bytes), mime);
            let fixture = json!({"metadata": {}, "records": [media_record(image)]});
            let html = String::from_utf8(render(&fixture, "dark").unwrap()).unwrap();
            assert!(!html.contains("<img "));
        }
    }

    #[test]
    fn only_bounded_inline_raster_payloads_are_rendered() {
        let image = octet_ai::Media::image_bytes(
            bytes::Bytes::from_static(include_bytes!(
                "../../tests/fixtures/export_html/one-pixel.png"
            )),
            mime::IMAGE_PNG,
        );
        let fixture = json!({"metadata":{}, "records":[media_record(image)]});
        let html = String::from_utf8(render(&fixture, "dark").unwrap()).unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(include_bytes!(
            "../../tests/fixtures/export_html/one-pixel.png"
        ));
        assert!(html.contains(&format!("src=\"data:image/png;base64,{data}\"")));
        assert!(!html.contains("\"Inline\""));
    }
}
