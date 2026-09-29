//! Interactive, print, and RPC octet terminal application.

fn main() -> std::process::ExitCode {
    octet_sdk::build_runtime()
        .expect("build the octet runtime")
        .block_on(octet_sdk::run_cli())
}
