use octet_extension::{
    Deserialize, Error, Extension, JsonSchema, Resource, ResourceType, Serialize, ToolResult,
};

struct Counter(i64);
impl ResourceType for Counter {
    const TYPE_ID: &'static str = "hello.Counter";
    // Default disposal drops the owned value. Override dispose(self) for fallible cleanup.
}
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
    extension.typed_tool::<Create, Created, _>(
        "counter_create",
        "Create a native counter",
        |input, call| {
            let counter = call.export(Counter(input.initial))?;
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
    extension.run()
}
