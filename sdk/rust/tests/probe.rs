use octet_extension::{Deserialize, Error, Extension, JsonSchema, ToolResult};
use std::time::Duration;
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Input {
    mode: String,
    #[serde(default)]
    text: String,
}
fn main() -> Result<(), Error> {
    let mut ext = Extension::new();
    ext.tool(
        "probe",
        "Exercise the native runtime",
        |input: Input, call| match input.mode.as_str() {
            "cancel" => {
                call.wait(Duration::from_secs(5))?;
                Ok("not cancelled".into())
            }
            "uncooperative" => {
                std::thread::sleep(Duration::from_secs(5));
                Ok("late".into())
            }
            "panic" => panic!("deliberate handler panic"),
            "error" => Err(Error::tool("domain failure")),
            "oversized" => Ok(ToolResult::text(
                "x".repeat(octet_extension::MAX_TEXT_BYTES + 1),
            )),
            "max-result" => Ok(ToolResult::text(
                "\0".repeat(octet_extension::MAX_TEXT_BYTES),
            )),
            "context" => Ok(ToolResult::text(call.host_context().to_string())),
            _ => Ok(ToolResult::text(input.text)),
        },
    )?;
    ext.run()
}
