# octet versus fx

`run.py` measures octet and [fx](https://github.com/vercel-labs/fx) on the same
machine, with the same hyperfine settings fx's own `benchmarks/startup.sh` uses
(no shell, fixed runs and warmups). Each agent runs from a private `HOME` with a
cleared environment against the same loopback OpenAI Chat Completions mock
(`mock_openai.py`), so a round trip measures the agent, not a provider.

Fixtures are built by driving each agent itself: the session list is N real
saved sessions, and the long session is T real turns with 4 KiB replies, each in
the agent's own format.

| scenario | octet | fx | what it covers |
| --- | --- | --- | --- |
| `startup` | `OCTET_BENCH=1 octet` | `FX_BENCH=1 fx` | argument parsing and frontend dispatch |
| `version` | `--version` | `--version` | process floor |
| `help` | `--help` | `help` | help rendering |
| `doctor` | `doctor` | `doctor --json` | local diagnostics |
| `models` | `--list-models` | `models --json` | model picking |
| `sessions-N` | `sessions list` | `sessions --json` | session picking with N sessions |
| `ask` | `--no-session -p hi` | `ask --no-save hi` | one API round trip (the CI path) |
| `ask-saved` | `-p hi` | `ask hi` | one round trip into a new saved session |
| `resume-T` | `--continue -p hi` | `ask --resume last hi` | resuming a T-turn session |

```sh
cargo build --release --locked -p octet-coding-agent --bin octet
python3 benchmarks/versus/run.py --octet target/release/octet --fx /path/to/fx \
    --runs 30 --sessions 1000 --turns 200
```

fx builds with Zig 0.17 (`zig build -Doptimize=ReleaseSafe`, as its benchmarks
do); `pip install ziglang==0.17.0` provides it as `python -m ziglang`.
`benchmarks/first_frame.py` covers time to the first interactive frame.

The mock logs request sizes to `wire.jsonl` in the work directory. Counting
`fsync`/`fdatasync` calls (`strace -f -e trace=fsync,fdatasync`) is worth doing
beside the wall clock: on a CI runner's disk each one is a device flush, so a
local tmpfs or fast SSD can hide most of a durability cost.
