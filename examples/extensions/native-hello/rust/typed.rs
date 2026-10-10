//! One declaration per input/output; no resource or bulk setup for an ordinary tool.
use octet_extension::{
    Deserialize, Diagnostic, Error, Extension, JsonSchema, Serialize, Severity, ToolResult,
};

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Samples {
    #[schemars(length(max = 128))]
    label: String,
    #[schemars(length(max = 4096))]
    samples: Vec<f64>,
    #[serde(default)]
    unit: Option<String>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Summary {
    label: String,
    count: u64,
    mean: f64,
    unit: Option<String>,
}

fn main() -> Result<(), Error> {
    let mut extension = Extension::new();
    extension.typed_tool::<Samples, Summary, _>(
        "summarize",
        "Summarize finite samples with typed output",
        |input, call| {
            call.check_cancelled()?;
            if input.samples.is_empty() {
                return ToolResult::error("At least one sample is required").with_diagnostics(
                    vec![Diagnostic::new(
                        Severity::Error,
                        "samples.empty",
                        "Provide at least one finite sample.",
                    )],
                );
            }
            if call.supports_progress() {
                call.progress("Summarizing samples")?;
            }
            let count = input.samples.len() as u64;
            let mut mean = 0.0;
            for sample in input.samples {
                call.check_cancelled()?;
                mean += sample / count as f64;
            }
            if !mean.is_finite() {
                return Err(Error::tool("Sample mean exceeds the finite numeric range"));
            }
            ToolResult::structured(
                Summary {
                    label: input.label,
                    count,
                    mean,
                    unit: input.unit,
                },
                "Sample summary ready",
            )
        },
    )?;
    extension.run()
}
