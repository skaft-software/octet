//! Interactive, print, and RPC octet terminal application.

fn main() -> std::process::ExitCode {
    if is_version_only_invocation() {
        println!("octet {}", env!("CARGO_PKG_VERSION"));
        return std::process::ExitCode::SUCCESS;
    }

    octet_sdk::build_runtime()
        .expect("build the octet runtime")
        .block_on(octet_sdk::run_cli())
}

fn is_version_only_invocation() -> bool {
    let mut args = std::env::args_os().skip(1);
    matches!(args.next().as_deref(), Some(arg) if arg == "--version" || arg == "-V")
        && args.next().is_none()
}
