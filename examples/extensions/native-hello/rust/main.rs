use octet_extension::{Deserialize, Extension, JsonSchema, ToolResult};
use std::time::Duration;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Greeting {
    #[schemars(length(max = 256))]
    name: String,
    #[serde(default)]
    #[schemars(range(min = 0, max = 5000))]
    delay_ms: u32,
}

fn main() -> Result<(), octet_extension::Error> {
    let mut extension = Extension::new();
    extension.tool(
        "hello",
        "Return a local greeting",
        |args: Greeting, call| {
            call.wait(Duration::from_millis(args.delay_ms.into()))?;
            Ok(ToolResult::text(format!("Hello, {}!", args.name)))
        },
    )?;
    extension.run()
}
