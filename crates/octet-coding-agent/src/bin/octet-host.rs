//! Native, versioned NDJSON host for applications that cannot link Rust.

fn main() -> anyhow::Result<()> {
    octet_sdk::build_runtime()?.block_on(octet_sdk::host::run_stdio())
}
