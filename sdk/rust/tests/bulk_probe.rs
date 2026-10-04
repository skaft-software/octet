use octet_extension::{
    BlobRef, Deserialize, Error, Extension, JsonSchema, Resource, ResourceType, Serialize,
    ToolResult,
};
use serde_json::{json, Value};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn log(path: &Path, mut event: Value) {
    event["pid"] = json!(std::process::id());
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
struct ReadInput {
    data: BlobRef,
    #[serde(default)]
    mode: String,
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
#[derive(Serialize, Deserialize, JsonSchema)]
struct Empty {}
fn write(
    call: &octet_extension::CallContext,
    input: &Create,
    transfers: Option<&Path>,
    saved: &Mutex<Option<std::fs::File>>,
    workspace: &Path,
) -> Result<BlobRef, Error> {
    // Host-selected scratch path is supplied only to hostile real-file fixtures;
    // the SDK author API still exposes only bounded Read/Write callbacks.
    let mut retained = None;
    let capacity = if input.mode == "long" {
        input.bytes + 1
    } else {
        input.bytes
    };
    let blob = call.write_blob(capacity, "application/octet-stream", |writer| {
        let buffer = [0xa5; 64 * 1024];
        let mut left = input.bytes;
        while left != 0 {
            let size = left.min(buffer.len() as u64) as usize;
            writer.write_all(&buffer[..size])?;
            left -= size as u64;
        }
        if input.mode == "ticket-quota" {
            let error = call
                .write_blob(1, "application/octet-stream", |w| w.write_all(&[1]))
                .unwrap_err();
            assert_eq!(error.to_string(), "quota_exceeded");
            log(
                workspace,
                json!({"kind":"quota","what":"ticket","message":error.to_string()}),
            );
        }
        if input.mode == "overflow" {
            writer.write_all(b"overflow")?;
        }
        if input.mode == "abandon" {
            return Err(io::Error::other("fixture abandons scratch before commit"));
        }
        if matches!(
            input.mode.as_str(),
            "short" | "long" | "wrong-digest" | "immutable"
        ) {
            writer.flush()?;
            let files: Vec<_> = std::fs::read_dir(transfers.expect("fixture transfer directory"))?
                .map(|entry| entry.unwrap().path())
                .collect();
            assert_eq!(files.len(), 1, "one active ticket in private transfer area");
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&files[0])?;
            match input.mode.as_str() {
                "short" => file.set_len(input.bytes - 1)?,
                "long" => file.set_len(input.bytes + 1)?,
                "wrong-digest" => file.write_all(&[0x5a])?,
                "immutable" => retained = Some(file),
                _ => unreachable!(),
            }
        }
        Ok(())
    })?;
    if retained.is_some() {
        // Preserve the producer FD across successful parent admission. A later
        // ordinary SDK call rewrites it, not just the provisional commit window.
        *saved.lock().unwrap() = retained;
    }
    Ok(blob)
}
fn main() -> Result<(), Error> {
    let workspace = PathBuf::from(std::env::args_os().nth(1).expect("private workspace"));
    let transfers = std::env::args_os().nth(2).map(PathBuf::from);
    let mut extension = Extension::new();
    let path = workspace.clone();
    let write_transfers = transfers.clone();
    let saved = Arc::new(Mutex::new(None));
    let write_saved = saved.clone();
    extension.typed_tool::<Create, Blob, _>(
        "write",
        "Write native binary data without JSON bytes",
        move |input, call| {
            log(
                &path,
                json!({"kind":"call","name":"write","pid":std::process::id()}),
            );
            let data = write(
                &call,
                &input,
                write_transfers.as_deref(),
                &write_saved,
                &path,
            )?;
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
    extension.typed_tool::<ReadInput, Count, _>(
        "read",
        "Read and verify immutable binary data",
        move |input, call| {
            log(
                &path,
                json!({"kind":"call","name":"read","pid":std::process::id()}),
            );
            let bytes = call.read_blob(&input.data, |reader| {
                if input.mode == "lease-quota" {
                    let error = call
                        .read_blob(&input.data, |r| std::io::copy(r, &mut std::io::sink()))
                        .unwrap_err();
                    assert_eq!(error.to_string(), "quota_exceeded");
                    log(
                        &path,
                        json!({"kind":"quota","what":"lease","message":error.to_string()}),
                    );
                }
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
    let path = workspace.clone();
    let rewrite_saved = saved.clone();
    extension.typed_tool::<Empty, Count, _>(
        "rewrite_saved",
        "Rewrite the producer scratch inode after publication",
        move |_, _| {
            log(
                &path,
                json!({"kind":"call","name":"rewrite_saved","pid":std::process::id()}),
            );
            let mut file = rewrite_saved
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| Error::tool("no saved producer FD"))?;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| Error::tool("fixture seek failed"))?;
            file.write_all(&[0x5a])
                .map_err(|_| Error::tool("fixture rewrite failed"))?;
            file.sync_all()
                .map_err(|_| Error::tool("fixture sync failed"))?;
            ToolResult::structured(Count { bytes: 1 }, "Producer scratch rewritten and closed")
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
            let data = write(&call, &input, transfers.as_deref(), &saved, &workspace)?;
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
