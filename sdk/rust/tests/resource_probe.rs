use octet_extension::{
    Deserialize, Error, Extension, JsonSchema, Resource, ResourceType, Serialize, ToolResult,
};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn log(path: &Path, kind: &str, name: &str) {
    let event = json!({"pid":std::process::id(),"kind":kind,"name":name,"thread":std::thread::current().name()});
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path.join("resources.jsonl"))
        .unwrap()
        .write_all(format!("{event}\n").as_bytes())
        .unwrap();
}
struct Counter {
    value: i64,
    mode: String,
    path: PathBuf,
}
impl ResourceType for Counter {
    const TYPE_ID: &'static str = "fixture.Counter";
    fn dispose(self) -> Result<(), Error> {
        log(&self.path, "dispose", &self.mode);
        if self.mode == "panic-dispose" {
            panic!("fixture disposal panic");
        }
        if self.mode == "fail-dispose" {
            return Err(Error::tool("fixture disposal failure"));
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Create {
    name: String,
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Created {
    counter: Resource<Counter>,
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Input {
    counter: Resource<Counter>,
    delta: i64,
    #[serde(default)]
    mode: String,
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Combine {
    first: Resource<Counter>,
    second: Created,
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Count {
    value: i64,
}
#[derive(Serialize, Deserialize, JsonSchema)]
struct Empty {}

fn main() -> Result<(), Error> {
    let workspace = PathBuf::from(std::env::args_os().nth(1).expect("private workspace"));
    let saved = Arc::new(Mutex::new(None::<Resource<Counter>>));
    let mut extension = Extension::new();
    let path = workspace.clone();
    let keep = saved.clone();
    extension.typed_tool::<Create, Created, _>(
        "create",
        "Create native counter",
        move |input, call| {
            log(&path, "call", "create");
            let counter = call.export(Counter {
                value: 0,
                mode: input.name.clone(),
                path: path.clone(),
            })?;
            *keep.lock().unwrap() = Some(counter.clone());
            log(&path, "exported", &input.name);
            if input.name == "cancel" {
                call.wait(Duration::from_secs(30))?;
            }
            if input.name == "invalid-output" {
                return ToolResult::structured(Count { value: 1 }, "Invalid fixture output");
            }
            if input.name == "error" {
                return Err(Error::tool("Fixture failed after registration"));
            }
            ToolResult::structured(Created { counter }, "Counter created")
        },
    )?;
    let path = workspace.clone();
    extension.operation::<Input, Count, _>(
        "add",
        "Mutate native counter",
        Some("/counter"),
        move |input, call| {
            log(&path, "call", "add");
            if input.mode == "release" {
                call.release(&input.counter)?;
            }
            let value = call.with_resource(&input.counter, |native| {
                if input.mode == "cancel" {
                    log(&path, "holding", "add");
                    while !call.is_cancelled() {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    // Explicit barrier proves cancellation alone doesn't free a live native borrow.
                    while !path.join("settle").exists() {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    log(&path, "settled", "add");
                    return Err(Error::cancelled());
                }
                native.value += input.delta;
                Ok(native.value)
            })?;
            ToolResult::structured(Count { value }, "Counter updated")
        },
    )?;
    let path = workspace.clone();
    extension.typed_tool::<Combine, Count, _>(
        "combine",
        "Use every declared resource slot",
        move |input, call| {
            log(&path, "call", "combine");
            let first = call.with_resource(&input.first, |counter| Ok(counter.value))?;
            let second = call.with_resource(&input.second.counter, |counter| Ok(counter.value))?;
            ToolResult::structured(
                Count {
                    value: first + second,
                },
                "Counters summed",
            )
        },
    )?;
    extension.operation::<Empty, Count, _>(
        "release_saved",
        "Release saved unpinned counter",
        None,
        move |_, call| {
            log(&workspace, "call", "release_saved");
            let counter = saved
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| Error::tool("No saved counter"))?;
            let status = call.release(&counter)?;
            ToolResult::structured(
                Count {
                    value: i64::from(status.retired),
                },
                "Counter retired; cleanup is separate",
            )
        },
    )?;
    extension.run()
}
