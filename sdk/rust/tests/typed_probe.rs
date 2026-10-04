//! Real subprocess fixture: every domain entry is logged, never stdout.
use octet_extension::diagnostic::{Edit, Fix, Location, Source, Span};
use octet_extension::{
    Deserialize, Diagnostic, Error, Extension, JsonSchema, Serialize, Severity, ToolResult,
};
use std::{fs::OpenOptions, io::Write, path::PathBuf, time::Duration};

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Record {
    name: String,
    enabled: bool,
    samples: Vec<f64>,
    #[serde(default)]
    note: Option<String>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let workspace = PathBuf::from(std::env::args().nth(1).expect("private fixture workspace"));
    std::env::set_var("HOME", &workspace);
    std::fs::write(workspace.join("fixture.cir"), b"* fixture\nR1 a b 1k\n")?;
    let mut extension = Extension::new();
    extension.typed_tool::<Record, Record, _>("typed", "Typed conformance record", move |mut input, call| {
        let mut log = OpenOptions::new().create(true).append(true).open(workspace.join("calls.jsonl")).unwrap();
        writeln!(log, "{}", serde_json::json!({"pid":std::process::id(),"name":input.name})).unwrap();
        match input.name.as_str() {
            "invalid-output" => return ToolResult::structured(serde_json::json!({"name":3}), "invalid output"),
            "missing-output" => return Ok(ToolResult::text("missing output")),
            "nonfinite-output" => input.samples = vec![f64::NAN],
            "nonportable-output" => return ToolResult::structured(serde_json::json!({"name":"x","enabled":true,"samples":[9_007_199_254_740_992_u64]}), "invalid integer"),
            "progress" => {
                assert_eq!(call.progress("typed started")?, 1);
                assert_eq!(call.progress_status("typed finished", Some(2), Some(2), Some("steps"))?, 2);
            }
            "cancel" => {
                call.progress("entered")?;
                call.wait(Duration::from_secs(30))?;
            }
            "diagnostic" => {
                let location = Location { source: Source::Workspace { path: "fixture.cir".into(), revision: "c68a14d42f9b59f1f9b39dcded7a2e81dd9ed210c2b6ba522c4a64d2575d672a".into() }, span: Span { start_byte: 13, end_byte: 14 } };
                let mut diagnostic = Diagnostic::new(Severity::Error, "solver.nonconvergent", "Operating point did not converge.");
                diagnostic.primary = Some(location.clone());
                diagnostic.fixes.push(Fix { title: "Ground node a".into(), edits: vec![Edit { location, replacement: "0".into() }] });
                return ToolResult::error("Domain failure").with_diagnostics(vec![diagnostic]);
            }
            "invalid-diagnostic" => return ToolResult::error("Domain failure").with_diagnostics(vec![Diagnostic::new(Severity::Error, "", "Invalid code")]),
            "error" => return Err(Error::tool("typed domain failure")),
            _ => {}
        }
        ToolResult::structured(input, "typed record")
    })?;
    extension.run()?;
    Ok(())
}
