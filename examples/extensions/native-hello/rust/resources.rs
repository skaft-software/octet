use octet_extension::{
    Deserialize, Error, Extension, JsonSchema, ReleaseStatus, Resource, ResourceType, Serialize,
    ToolResult,
};
use std::sync::{Arc, Mutex};

struct Counter(i64);
impl ResourceType for Counter {
    const TYPE_ID: &'static str = "hello.Counter";
    // Default disposal drops the owned value. Override dispose(self) for fallible cleanup.
}
#[derive(Deserialize, Serialize, JsonSchema)]
struct Empty {}
#[derive(Deserialize, Serialize, JsonSchema)]
struct Create {
    initial: i64,
}
#[derive(Deserialize, Serialize, JsonSchema)]
struct Created {
    counter: Resource<Counter>,
}
#[derive(Deserialize, Serialize, JsonSchema)]
struct Add {
    counter: Resource<Counter>,
    amount: i64,
}
#[derive(Deserialize, Serialize, JsonSchema)]
struct Added {
    value: i64,
}

fn main() -> Result<(), Error> {
    let mut extension = Extension::new();
    let last = Arc::new(Mutex::new(None::<Resource<Counter>>));
    let created = last.clone();
    extension.typed_tool::<Create, Created, _>(
        "counter_create",
        "Create a native counter",
        move |input, call| {
            let counter = call.export(Counter(input.initial))?;
            *created.lock().unwrap() = Some(counter.clone());
            ToolResult::structured(Created { counter }, "Counter created")
        },
    )?;
    extension.operation::<Add, Added, _>(
        "counter_add",
        "Add to a native counter",
        Some("/counter"),
        |input, call| {
            let value = call.with_resource(&input.counter, |counter| {
                counter.0 = counter
                    .0
                    .checked_add(input.amount)
                    .ok_or_else(|| Error::tool("Counter overflow"))?;
                Ok(counter.0)
            })?;
            ToolResult::structured(Added { value }, "Counter updated")
        },
    )?;
    // Release is lifecycle control, not a method on a pinned resource argument.
    // This example keeps only the most recently created identity; older counters
    // are still host-owned and may be retired by the host or session teardown.
    extension.operation::<Empty, ReleaseStatus, _>(
        "counter_release_last",
        "Retire the most recently created counter",
        None,
        move |_, call| {
            let counter = last
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| Error::tool("No saved counter"))?;
            let status = call.release(&counter)?;
            last.lock().unwrap().take();
            ToolResult::structured(status, "Counter retired; cleanup is separate")
        },
    )?;
    extension.run()
}
