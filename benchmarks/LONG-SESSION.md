# Long-session budget protocol

`long_session_bench.py` drives a **real octet binary** through a long TUI
session against a deterministic fake provider on `127.0.0.1`, and produces a
JSON record of how per-turn latency, memory and persistence behave as the
session grows. `budget_gate.py` runs the short scale and fails on a budget
breach in `performance_budget.json`.

Everything is offline and deterministic: disposable HOME/workspace, no
extensions, no network beyond the loopback provider, no real credentials, and
a fixed streamed-answer schedule. There is no comparison against any other
binary input; every budget value is octet's own measured behaviour plus
headroom.

## What it measures

Each turn submits `LONG_PROMPT_%05d` into the PTY composer and ends when the
turn's checkpoint is *durable* in the session file (the answer being painted
is not the turn boundary; submitting the next turn early would overlap runs
and lose checkpoints). Every 25th turn (`LARGE_EVERY`) issues a `bash` tool
call whose output is 128 KiB, so durable bytes, request bytes and the tool
capture path all carry real weight.

Scales (`--scales`) are run continuously in one session: 100, 1,000 and 5,000
turns. At every scale the run records:

- **turn_latency_ms** (p50/p95/max) from input submission until the durable
  checkpoint, minus a fixed 40 ms completion drain; early vs late small-turn
  p50 medians expose linear growth.
- **reply_frame_ms**: submission until the answer's first PTY frame.
- **settled_ms**: submission until the durable checkpoint excludes the drain.
- **peak/end RSS** (sampled every 2 ms), **RSS growth per 100 turns**
  (least-squares slope), measured on the octet process alone.
- **session file size**, **append bytes per turn** and the traced
  `Session::persist` wall time.
- **CPU time** (`/proc` ticks, process alone).
- **render cost** while a response streams: the traced `render.frame` wall
  time of paints issued during the turn.

At each scale the freshly saved session is also **resumed** into a new process
(`resume` cell: spawn to the synchronized fullscreen frame with history and
model present), and the durable ledger is independently re-verified
(`check_session`: prompts, answers, tool bytes, parent/head links and
checkpoint counts must all match the fixture exactly — not just the latest
turn).

A correct run also validates the provider wire: every request must replay the
previously accepted history exactly (structural comparison of the parsed
message list against everything prior — a dropped, reordered or rewritten turn
fails on the request that carries it), and tool call/result pairing plus the
complete 128 KiB tool output are checked byte-for-byte. Fixtures that shrunk
silently would show as a floor breach in the budget, per-turn `append_bytes`.

## Running it

```sh
export CARGO_TARGET_DIR=/absolute/path/to/target
cargo build --release --locked -p octet-coding-agent --bin octet
python3 benchmarks/long_session_bench.py \
  --binary "$CARGO_TARGET_DIR/release/octet" \
  --scales 100 1000 5000 --timeout 30 \
  --output /tmp/octet-long-session.json
```

`python3 scripts/tests/test_long_session_bench.py` exercises the
fixture/ledger/trace contracts without a binary. Use `--work-dir` to keep
sessions and diagnostics for a run you inspect. `--strace` is a diagnostic
mode only: it wraps the binary with a syscall observer and its overhead is not
comparable with ordinary results.

Measure on a quiet machine only. Report the 1-minute load average beside every
number; load above single digits is not a calibration source on this machine.
Compare same-profile builds, same viewport, same fixture constants, same
Python version. Never add timing headroom to make a noisy run pass: rerun, or
mark the run provisional.

## The budget gate

`budget_gate.py` runs the **short scale (100 turns)** and asserts
`performance_budget.json`:

```sh
python3 benchmarks/budget_gate.py --binary "$CARGO_TARGET_DIR/release/octet"
```

- **Caps** are absolute ceilings with headroom over a calibrated quiet-machine
  value. They fail when a per-turn cost inflates.
- **Relative checks** (`turn_latency_growth_ratio`: late small-turn p50 vs
  early small-turn p50) scale with the machine so runner noise cancels.
- **Floors** pin fixture-driven metrics (append bytes per turn, session bytes
  per turn) from below so a silently shrinking fixture can never mask a
  regression.
- A result that did **not pass**, or whose largest cell is not the budget's
  short scale, is not checkable and fails.

`--calibrate` prints the measured values beside the committed caps for a
covered run. The committed file is edited by hand after review;
`--calibrate` never writes it.

Wiring the gate into CI is a maintainer step and is intentionally left to the
landing review: the script itself carries no CI assumptions, and a contended
runner shows up in `turn_latency_growth_ratio` first. Keep the gate job
unparallelized and compare buffers (build logs) when diagnosing a flake.

