//! Runs the published Pi `SessionManager` as the differential oracle.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::Value;

use super::Case;

/// One oracle step: the state after an operation, its rejection note and the
/// state of the same file reopened from disk.
pub(super) struct PiStep {
    pub(super) state: Value,
    pub(super) rejected: bool,
    pub(super) disk: Option<Value>,
}

/// Where the oracle wrote its Pi session files for this run.
pub(super) fn session_file(case_root: &Path, step: usize) -> PathBuf {
    case_root.join("checkpoints").join(format!("{step}.jsonl"))
}

/// Replay every case through one published Pi version.
pub(super) fn run(
    repo: &Path,
    version: &str,
    directory: &Path,
    cases: &[Case],
) -> anyhow::Result<HashMap<String, Vec<PiStep>>> {
    let driver = repo
        .join("scripts/fixtures/pi-reference")
        .join("session-driver.mjs");
    anyhow::ensure!(
        driver.is_file(),
        "the Pi session oracle is missing: {} (run `npm ci --prefix scripts/fixtures/pi-reference --ignore-scripts`)",
        driver.display()
    );
    std::fs::create_dir_all(directory)?;
    let input = directory.join("input.json");
    let mut file = std::fs::File::create(&input)?;
    let payload = serde_json::json!({
        "cases": cases.iter().map(|case| serde_json::json!({
            "name": case.name,
            "operations": case.operations,
        })).collect::<Vec<_>>(),
        "directory": directory,
    });
    file.write_all(serde_json::to_vec(&payload)?.as_slice())?;
    drop(file);
    let stdin = std::fs::File::open(&input)?;
    let output = Command::new("node")
        .arg(&driver)
        .arg(version)
        .current_dir(repo.join("scripts/fixtures/pi-reference"))
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| {
            anyhow::anyhow!(
                "cannot run the Pi session oracle with node ({error}); the differential harness needs node 22.19+ on PATH"
            )
        })?;
    anyhow::ensure!(
        output.status.success(),
        "Pi {version} oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(
        parsed["version"].as_str() == Some(version),
        "oracle reported version {} for {version}",
        parsed["version"]
    );
    let mut results = HashMap::new();
    for result in parsed["results"].as_array().into_iter().flatten() {
        let name = result["name"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("oracle result without a name"))?
            .to_owned();
        let mut steps = Vec::new();
        for step in result["steps"].as_array().into_iter().flatten() {
            let note = step["error"].as_str();
            anyhow::ensure!(
                matches!(note, None | Some("rejected" | "unpersisted")),
                "oracle reported an unknown step note {note:?}"
            );
            steps.push(PiStep {
                state: step["state"].clone(),
                rejected: note == Some("rejected"),
                disk: step
                    .get("disk")
                    .filter(|disk| !disk.is_null())
                    .map(|disk| disk["state"].clone()),
            });
        }
        results.insert(name, steps);
    }
    Ok(results)
}
