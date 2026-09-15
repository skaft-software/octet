//! Script-free, self-contained HTML projection of the redacted portable export.
use base64::Engine;
use serde_json::Value;

fn escape(text: &str) -> String {
    let safe = sexy_tui_rs::sanitize_text(text, sexy_tui_rs::SanitizeOptions::default());
    safe.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Only inline raster images become active markup. URLs, provider references,
/// audio, SVG and unrecognized payloads are never fetched or executed.
fn images(value: &mut Value, html: &mut String) {
    match value {
        Value::Object(object) => {
            if let Some(image) = object.get_mut("Image") {
                let mime = image["media_type"].as_str().unwrap_or("");
                if let Some(data) = image["source"]["Inline"].as_str() {
                    if data.len() <= 7 * 1024 * 1024 {
                        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
                            let valid = match mime {
                                "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
                                "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
                                "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
                                "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
                                _ => false,
                            };
                            if valid && bytes.len() <= 5 * 1024 * 1024 {
                                // Re-encoding admits only base64 alphabet bytes into attributes.
                                let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                                html.push_str(&format!("<img alt=\"Session image\" loading=\"lazy\" src=\"data:{mime};base64,{data}\">\n"));
                            }
                        }
                    }
                }
                *image = Value::String("[image: inline preview when supported; external references never fetched]".into());
            }
            if let Some(audio) = object.get_mut("Audio") {
                *audio = Value::String("[audio omitted from HTML; review the private session separately]".into());
            }
            for child in object.values_mut() { images(child, html); }
        }
        Value::Array(values) => for child in values { images(child, html); },
        _ => {}
    }
}

pub(crate) fn render(package: &Value, theme: &str) -> anyhow::Result<Vec<u8>> {
    let (scheme, background, card, foreground, muted) = match theme {
        "light" => ("light", "#f4f5f7", "#ffffff", "#17202a", "#52616b"),
        _ => ("dark", "#15171c", "#20242c", "#ecedf0", "#b1bac7"),
    };
    let title = package["metadata"]["name"].as_str().or_else(|| package["source_title"].as_str()).unwrap_or("octet session");
    let mut html = format!("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n<meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'\">\n<title>{}</title>\n<style>:root{{color-scheme:{scheme};background:{background};color:{foreground}}}body{{max-width:72rem;margin:2rem auto;padding:0 1rem;font:16px system-ui}}article{{background:{card};padding:1rem;margin:1rem 0;border-radius:.5rem}}pre{{white-space:pre-wrap;overflow-wrap:anywhere}}.notice{{color:{muted}}}img{{max-width:100%;height:auto}}</style></head><body>\n<h1>{}</h1>\n<p class=\"notice\">Private session export. Redaction is not proof of secret-free content. Images may contain sensitive information. Markup, links and terminal controls are inert; audio and remote media are omitted. Accounting values are known subtotals if any usage_uncertainty record exists.</p>\n", escape(title), escape(title));
    let mut metadata = package.clone();
    metadata.as_object_mut().ok_or_else(|| anyhow::anyhow!("export package must be an object"))?.remove("records");
    html.push_str(&format!("<details><summary>Export metadata</summary><pre>{}</pre></details>\n", escape(&serde_json::to_string_pretty(&metadata)?)));
    for record in package["records"].as_array().ok_or_else(|| anyhow::anyhow!("export records must be an array"))? {
        let mut record = record.clone();
        let mut previews = String::new();
        images(&mut record, &mut previews);
        html.push_str(&format!("<article><pre>{}</pre>\n{previews}</article>\n", escape(&serde_json::to_string_pretty(&record)?)));
    }
    html.push_str("</body></html>\n");
    Ok(html.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn safe_markup_ansi_media_and_both_themes_have_stable_goldens() {
        let fixture = json!({"source_title":"<script>alert('x')</script>\u{1b}]52;c;secret\u{7}","metadata":{},"records":[{"type":"usage_uncertainty"},{"Image":{"source":{"Url":"https://private.example/secret"},"media_type":"image/svg+xml"}},{"text":"<img src=x onerror=evil> & **literal markdown**"}]});
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
    fn only_bounded_inline_raster_payloads_are_rendered() {
        let image = octet_ai::Media::image_bytes(bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\n"), mime::IMAGE_PNG);
        let fixture = json!({"metadata":{}, "records":[image]});
        let html = String::from_utf8(render(&fixture, "dark").unwrap()).unwrap();
        assert!(html.contains("src=\"data:image/png;base64,iVBORw0KGgo=\""));
        assert!(!html.contains("\"Inline\""));
    }
}
