//! Headless native service peer for adapter tests. Compiles the production
//! service source, not a reference editor or a JavaScript editing fallback.
//! Real frontend owner/mount admission is qualified separately by App tests.
// Frontend-only lease observation/write methods are exercised by App tests.
#[allow(dead_code)]
#[path = "../src/native_editor.rs"]
mod native_editor;

use std::collections::BTreeMap;
use std::io::{self, BufRead, Read, Write};

use serde_json::{json, Value};

fn main() -> anyhow::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut mounts = BTreeMap::new();
    loop {
        let mut line = String::new();
        if input.by_ref().take(1_048_577).read_line(&mut line)? == 0 {
            break;
        }
        anyhow::ensure!(
            line.len() <= 1_048_576 && line.ends_with('\n'),
            "fixture request exceeds frame bound"
        );
        let request: Value = serde_json::from_str(&line)?;
        let params = &request["params"];
        let result = match request["method"].as_str() {
            Some("ui/open") => {
                let id = params["surface_id"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("fixture surface id required"))?;
                mounts.insert(
                    id.to_owned(),
                    native_editor::EditorService::for_mount(id.to_owned()),
                );
                Ok(json!({}))
            }
            Some("ui/close") => {
                mounts.remove(params["surface_id"].as_str().unwrap_or(""));
                Ok(json!({}))
            }
            Some("ui/chrome") if params["chrome"]["kind"] == "editor" => {
                let chrome = &params["chrome"];
                let surface = chrome["surface_id"].as_str().unwrap_or("");
                if let Some(service) = mounts.get_mut(surface) {
                    service.request(chrome["editor_id"].as_str(), chrome["operation"].clone())
                } else {
                    Err((
                        octet_agent::extension_process::ExtensionRequestFailure::NotForegroundOwner,
                        "fixture surface retired".into(),
                    ))
                }
            }
            _ => anyhow::bail!("unsupported fixture operation"),
        };
        let reply = match result {
            Ok(value) => json!({"result":value}),
            Err((failure, detail)) => json!({"error":{"code":failure.code(),"message":detail}}),
        };
        serde_json::to_writer(&mut output, &reply)?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}
