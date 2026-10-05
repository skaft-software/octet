use octet_extension::{BlobRef, Deserialize, Extension, JsonSchema, Serialize, ToolResult};

#[derive(Deserialize, Serialize, JsonSchema)]
struct Empty {}
#[derive(Deserialize, Serialize, JsonSchema)]
struct Saved {
    data: BlobRef,
}
#[derive(Deserialize, Serialize, JsonSchema)]
struct Size {
    bytes: u64,
}

fn main() -> Result<(), octet_extension::Error> {
    let mut extension = Extension::new();
    extension.typed_tool::<Empty, Saved, _>(
        "blob_save",
        "Save immutable binary data",
        |_, call| {
            let bytes = b"Native binary data, not JSON payload";
            let data =
                call.write_blob(bytes.len() as u64, "application/octet-stream", |writer| {
                    writer.write_all(bytes)
                })?;
            ToolResult::structured(Saved { data }, "Binary data saved")
        },
    )?;
    extension.typed_tool::<Saved, Size, _>(
        "blob_size",
        "Read immutable data through a fresh lease",
        |input, call| {
            let bytes = call.read_blob(&input.data, |reader| {
                std::io::copy(reader, &mut std::io::sink())
            })?;
            ToolResult::structured(Size { bytes }, "Binary data verified; read lease released")
        },
    )?;
    extension.run()
}
