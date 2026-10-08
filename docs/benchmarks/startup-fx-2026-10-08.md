# Startup against fx 0.0.13

octet 0.9.0 and fx 0.0.13 (vercel-labs/fx at `47943cb`) on one Linux host,
measured three ways. The aim is to measure what fx measures, as fx measures it,
and to say where the two cannot be made equal.

## What fx's numbers mean

- fx's startup benchmark (`benchmarks/startup.sh:96-104`) runs `FX_BENCH=1 fx`
  under hyperfine without a shell, 100 runs after 10 warmups: wall clock from
  process launch to exit. Its CI enforces a 2 ms mean on Linux and never
  subtracts the process baseline (`benchmarks/check_budgets.py`).
- With no arguments, `FX_BENCH` is handled at the top of `main`
  (`src/main.zig:3467`): fx scans the environment, parses an empty argument
  list against its command registry, and calls `_exit(0)`. Its threaded I/O,
  allocator, auth and config parsing, app initialisation and terminal setup do
  not run. `AGENTS.md:351` places the check after CLI dispatch in
  `app_entry_runtime.zig`; the code exits earlier.
- fx's changelog (0.0.8) says "CLI startup now takes about 0.5 ms before
  terminal initialization".
- The "10 µs cold start" appears only in launch announcements. No file in the
  fx repository or its history states or produces it, and no fx benchmark
  measures an interval that short. Launching any process on this host takes
  longer (the `true` row below), so 10 µs cannot be a launch-to-exit time. We
  could not reproduce or trace it.

## The octet boundary

`OCTET_BENCH=1 octet` with no arguments stops at the same point: arguments are
parsed with the real clap parser and dispatched to the interactive frontend,
then the process exits with `std::process::exit(0)`
(`crates/octet-coding-agent/src/lib.rs`, `STARTUP_BENCH_ENV`). Nothing after it
runs: no working directory, terminal, configuration, session or extension.

One difference cannot be removed without changing octet: its `main` builds the
Tokio runtime and its worker threads before parsing arguments, whereas fx exits
before creating its threaded I/O. The octet number includes that runtime.

## Results

Linux 6.18, 4 vCPU Intel Xeon @ 2.10 GHz, idle. fx built exactly as its release
workflow builds Linux (`zig build -Doptimize=ReleaseSafe -Dtarget=x86_64-linux`,
Zig 0.16.0): a 13,221,776-byte static executable. octet built with
`cargo build --release --locked -p octet-coding-agent --bin octet`: a
50,642,880-byte stripped PIE linked dynamically against glibc. Every command
ran from an empty temporary HOME with a cleared environment.

hyperfine 1.19.0 `-N`, 100 runs after 10 warmups, milliseconds from launch to exit:

| Command | Median | Mean | Std dev | p95 |
| --- | ---: | ---: | ---: | ---: |
| `true` (process floor, dynamic) | 1.081 | 1.107 | 0.156 | 1.451 |
| A: `FX_BENCH=1 fx` | 0.153 | 0.173 | 0.069 | 0.275 |
| A: `OCTET_BENCH=1 octet` | 3.709 | 3.786 | 0.480 | 4.814 |
| B: `fx --version` | 0.245 | 0.267 | 0.087 | 0.444 |
| B: `octet --version` | 2.285 | 2.351 | 0.259 | 2.794 |
| B: `fx help` | 0.217 | 0.266 | 0.121 | 0.474 |
| B: `octet --help` | 3.756 | 3.813 | 0.427 | 4.840 |

C: first interactive frame on a 200x50 PTY with terminal queries answered,
fresh empty HOME (first-run setup screen for both), 100 interleaved runs after
10 warmups. The frame is complete when its last line appears: "esc to set up
later" for fx, "esc close" for octet. Milliseconds from fork:

| Agent | First byte median | First frame median | Mean | Std dev | p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| fx | 2.22 | 11.86 | 12.43 | 2.11 | 16.38 |
| octet | 5.46 | 7.75 | 8.05 | 1.41 | 9.73 |

fx's own marker ("Run /help", its header line) gave a 11.65 ms median; fx draws
the whole first frame at once.

## Reading the numbers

- fx is a static executable and skips the dynamic loader, so its bench run is
  faster than the dynamically linked `true`. octet's `--version` exits before
  any runtime exists; its 2.3 ms is mostly loading and relocating a 50 MB PIE
  against glibc. The bench boundary adds about 1.4 ms of runtime creation and
  argument parsing.
- On the short command paths fx is 9 to 24 times faster. On the path users
  wait for, a usable first screen, octet is about 4 ms (35%) faster.

## Claim

> On the same Linux machine, octet draws its first interactive frame in a
> median 7.8 ms, about 35% sooner than fx 0.0.13 (11.9 ms). fx exits faster on
> short commands (0.15 ms for its own startup benchmark against octet's 3.7 ms).
> fx's 10 µs figure is not reproducible from its repository and is below the
> cost of launching a process.

## Reproduce

```sh
cargo build --release --locked -p octet-coding-agent --bin octet
benchmarks/startup.sh target/release/octet /path/to/fx
benchmarks/first_frame.py --runs 100 --env FX_AUTO_UPGRADE=0 \
  --binary /path/to/fx "esc to set up later" --binary target/release/octet "esc close"
```

These are single-host measurements, not a cross-platform result; macOS and
Windows were not measured.
