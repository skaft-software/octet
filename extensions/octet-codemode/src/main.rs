//! One Rust executable with two modes: the API 0.4 extension adapter
//! (`serve`) and the disposable warm JavaScript runner (`runner`, invoked as a
//! bare engine name). JavaScript and WASM are embedded at compile time; no
//! Node/Python runtime and no runtime files are read from the checkout.
use anyhow::Result;

/// Bounded JSON-lines frames on both the host protocol and the runner IPC.
pub(crate) const IPC_BYTES: usize = 20 * 1024 * 1024;

mod extension;
mod guest;
mod native;
mod runner;
mod wasi;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        [engine] if ["native", "wasi"].contains(engine) => runner::serve(engine),
        ["serve"] => serve("wasi"),
        ["serve", "--engine", engine] if ["native", "wasi"].contains(engine) => serve(engine),
        [flag] if ["--help", "-h"].contains(flag) => {
            println!(
                "octet-codemode: serve [--engine wasi|native]\n\
                 Internal disposable runner: wasi|native\n\
                 WASI (Wasmi + vendored QuickJS-WASI) is the default engine; the native\n\
                 quickjs-ng lane is opt-in. No Node/Python runtime is required."
            );
            Ok(())
        }
        _ => Err(anyhow::anyhow!(
            "usage: octet-codemode serve [--engine wasi|native]"
        )),
    };
    if let Err(error) = result {
        let message: String = format!("{error:#}").chars().take(2048).collect();
        eprintln!("octet-codemode: {message}");
        std::process::exit(1);
    }
}

fn serve(engine: &str) -> Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(anyhow::Error::from)
        .and_then(|runtime| runtime.block_on(extension::run(engine.to_owned())))
}
