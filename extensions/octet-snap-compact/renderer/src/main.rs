use base64::Engine as _;
use oxi_snapcompact::{compact, CompactOptions, CompactPreparation};
use serde_json::{json, Value};
use std::io::{self, Read};

fn render(value: &Value) -> Result<Value, &'static str> {
    let model_id = value
        .get("model_id")
        .and_then(Value::as_str)
        .ok_or("missing model_id")?;
    let text = value
        .get("text")
        .and_then(Value::as_str)
        .ok_or("missing text")?;
    if model_id.len() > 256 || text.is_empty() || text.len() > 16 * 1024 {
        return Err("input exceeds renderer bounds");
    }
    // The host already serializes the conversation. `prepare` reserializes and
    // can drop blank lines; supply the exact chunk to the rasterizer instead.
    let prep = CompactPreparation {
        text: text.to_owned(),
        bounded_text: text.to_owned(),
        remaining_text: String::new(),
    };
    let result = compact(
        &prep,
        &CompactOptions {
            model_id: model_id.to_owned(),
            max_frames: 32,
            shape: None,
        },
    );
    let mut covered = 0;
    for frame in &result.frames {
        if frame.source_start != covered
            || frame.source_end <= covered
            || !frame.bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        {
            return Err("renderer did not cover all source characters with PNG frames");
        }
        covered = frame.source_end;
    }
    if result.frames.is_empty()
        || result.frames.len() > 32
        || result.source_text != text
        || covered != text.chars().count()
    {
        return Err("renderer did not cover all source characters with PNG frames");
    }
    let frames: Vec<_> = result
        .frames
        .iter()
        .map(|frame| base64::engine::general_purpose::STANDARD.encode(&frame.bytes))
        .collect();
    if frames.iter().any(|frame| frame.len() > 512 * 1024)
        || frames.iter().map(String::len).sum::<usize>() > 768 * 1024
    {
        return Err("rendered output exceeds protocol limits");
    }
    Ok(json!({ "compaction_frames": frames }))
}

fn main() {
    let mut input = String::new();
    let outcome = io::stdin()
        .take(16 * 1024 + 1025)
        .read_to_string(&mut input)
        .map_err(|_| "unable to read input")
        .and_then(|_| {
            if input.len() > 16 * 1024 + 1024 {
                Err("input exceeds renderer bounds")
            } else {
                serde_json::from_str::<Value>(&input).map_err(|_| "invalid input JSON")
            }
        })
        .and_then(|value| render(&value));
    match outcome {
        Ok(value) => println!("{value}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_complete_png_frames_and_refuses_truncation() {
        let value =
            render(&json!({"model_id":"claude-sonnet", "text":"User: hello\nAssistant: hi"}))
                .unwrap();
        let frames = value["compaction_frames"].as_array().unwrap();
        assert!(!frames.is_empty());
        assert!(frames[0].as_str().unwrap().starts_with("iVBOR"));
        assert!(render(&json!({"model_id":"claude", "text":"x".repeat(10_000)})).is_err());
    }
}
