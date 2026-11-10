# Local process benchmark

`session_bench.py` runs a release binary against a deterministic HTTP server bound
only to `127.0.0.1`. It supplies disposable HOME, XDG directories, credentials,
workspace and sessions, disables remote discovery with `--offline`, and never
uses real credentials. It requires Linux `/proc`, Python 3 and a PTY. The
provider implements the OpenAI Chat Completions streaming protocol. Use a quiet
machine and compare the same binary profile, kernel, terminal size and fixture;
this is not a measurement of a real provider or a typical model response.

```sh
export CARGO_TARGET_DIR=/absolute/path/to/target
cargo build --release --locked -p octet-coding-agent --bin octet
python3 benchmarks/test_session_bench.py
python3 benchmarks/session_bench.py --binary "$CARGO_TARGET_DIR/release/octet" \
  --runs 5 --warmup 1 --long-prompts 75 --output /tmp/octet-session-bench.json
```

`--scenarios` selects any of `startup tools interactive long write stream`.
`--timeout` is the per-frame or per-process timeout in seconds. Every sample
uses a new HOME/workspace; one warmup is discarded by default. The JSON includes
raw samples, median/min/max, a binary SHA-256, platform and metric definitions.
It fails on missing, duplicate or out-of-order requests, missing tool rounds,
unsuccessful or incorrect shell output, incorrect streamed output, incorrect file
contents, failure to edit the PTY
composer, abnormal shutdown or failure to restore terminal modes.

* **startup**: process spawn to the first completed PTY frame containing the
  selected model, followed by an editability check (120x40 terminal).
* **tools**: one headless prompt, four sequential `bash` tool calls and a
  final response. `wall_ms` is process spawn through exit. `cpu_ms` comes from
  `wait4` (user + system CPU including children waited for by the process).
* **interactive**: five sequential TUI prompts; **long**: 75 by default. Each
  response is approximately 2 KB of repeated Markdown. Each per-prompt wall
  interval runs from input submission until the answer is drawn and 30 ms of
  PTY quiet have elapsed, including sampling overhead. CPU is the change in
  the process's `/proc/PID/stat` user + system ticks; tick granularity is in
  the JSON. Each prompt includes current RSS and private resident bytes.
* **write**: one headless prompt, a write tool call containing 200,000 ASCII
  bytes, and a final response. The file is checked byte-for-byte. Timing is
  the entire process, not only the filesystem operation.
* **stream**: one headless prompt with a 60,000-character ASCII response sent
  in 256-character streaming deltas. Output is checked byte-for-byte. This
  does **not** measure interactive rendering of that response.

Peak RSS is sampled from `/proc/PID/status` every 2 ms and may miss short peaks;
it excludes descendants. Private resident memory is the sum of `Private_Clean`,
`Private_Dirty`, and `Private_Hugetlb` from `/proc/PID/smaps_rollup` for the
agent process alone. It is not process-tree RSS, total heap allocations, PSS,
or a precise live-heap count. Long-session private-memory slope is ordinary
least squares over all sampled prompts, reported in bytes/prompt. It can be
used as a provisional local gate with `--max-long-slope-bytes`, but allocator
retention, platform and fixture size affect this measure. A gate is meaningful
only after calibrating repeated runs on the intended runner. A long scenario
with at least two prompts is required when that gate is selected.

Allocation and instruction counts are currently emitted as `null`, not zero:
this runner does not instrument the allocator or run an instruction profiler.
Where available, an external profiler must be applied and reported separately;
its overhead and whole-process scope must not be confused with these samples.
Wall time, tick CPU and RSS are noisy on shared CI; retain the raw distributions
and compare on matched runners rather than importing thresholds from other
hardware. Do not infer attribution or a regression's introducing commit solely
from these process-level measurements.

The ordinary `successful_context_hook_preserves_large_canonical_history` test
checks a no-op hook against 150 messages of 2 KiB each without timing assertions.
For end-to-end measurements and the long-session budget, see
[the long-session protocol](LONG-SESSION.md).
