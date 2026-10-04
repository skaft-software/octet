use octet_extension::{
    BlobRef, Deserialize, Error, Extension, JsonSchema, Resource, ResourceType, Serialize,
    ToolResult,
};
use serde_json::{json, Value};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn log(path: &Path, event: Value) {
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path.join("bulk.jsonl"))
        .unwrap()
        .write_all(format!("{event}\n").as_bytes())
        .unwrap();
}
struct Native(PathBuf);
impl ResourceType for Native {
    const TYPE_ID: &'static str = "fixture.BulkNative";
    fn dispose(self) -> Result<(), Error> {
        log(&self.0, json!({"kind":"dispose"}));
        Ok(())
    }
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Create {
    bytes: u64,
    #[serde(default)]
    mode: String,
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Blob {
    data: BlobRef,
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Joint {
    data: BlobRef,
    native: Resource<Native>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Count {
    bytes: u64,
}
fn write(call: &octet_extension::CallContext, input: &Create) -> Result<BlobRef, Error> {
    call.write_blob(input.bytes, "application/octet-stream", |writer| {
        let buffer = [0xa5; 64 * 1024];
        let mut left = input.bytes;
        while left != 0 {
            let size = left.min(buffer.len() as u64) as usize;
            writer.write_all(&buffer[..size])?;
            left -= size as u64;
        }
        if input.mode == "overflow" {
            writer.write_all(b"overflow")?;
        }
        Ok(())
    })
}
fn main() -> Result<(), Error> {
    let workspace = PathBuf::from(std::env::args_os().nth(1).expect("private workspace"));
    let mut extension = Extension::new();
    let path = workspace.clone();
    extension.typed_tool::<Create, Blob, _>(
        "write",
        "Write native binary data without JSON bytes",
        move |input, call| {
            log(
                &path,
                json!({"kind":"call","name":"write","pid":std::process::id()}),
            );
            let data = write(&call, &input)?;
            log(&path, json!({"kind":"committed","blob":data}));
            if input.mode == "cancel" {
                call.wait(Duration::from_secs(30))?;
            }
            if input.mode == "error" {
                return Err(Error::tool("fixture error after commit"));
            }
            if input.mode == "invalid-output" {
                return ToolResult::structured(
                    Count { bytes: input.bytes },
                    "Invalid fixture result",
                );
            }
            ToolResult::structured(Blob { data }, "Binary data committed")
        },
    )?;
    let path = workspace.clone();
    extension.typed_tool::<Blob, Count, _>(
        "read",
        "Read and verify immutable binary data",
        move |input, call| {
            log(
                &path,
                json!({"kind":"call","name":"read","pid":std::process::id()}),
            );
            let bytes = call.read_blob(&input.data, |reader| {
                let mut bytes = 0;
                let mut buffer = [0; 64 * 1024];
                loop {
                    let n = reader.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    if !buffer[..n].iter().all(|b| *b == 0xa5) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "fixture bytes changed",
                        ));
                    }
                    bytes += n as u64;
                }
                Ok(bytes)
            })?;
            ToolResult::structured(Count { bytes }, "Binary data verified")
        },
    )?;
    extension.typed_tool::<Create, Joint, _>(
        "joint",
        "Export a native object and blob in one result",
        move |input, call| {
            log(
                &workspace,
                json!({"kind":"call","name":"joint","pid":std::process::id()}),
            );
            let native = call.export(Native(workspace.clone()))?;
            let data = write(&call, &input)?;
            log(
                &workspace,
                json!({"kind":"joint","native":native,"blob":data}),
            );
            if input.mode == "error" {
                return Err(Error::tool("fixture joint publication error"));
            }
            ToolResult::structured(
                Joint { data, native },
                "Native object and immutable data returned",
            )
        },
    )?;
    extension.run()
}
