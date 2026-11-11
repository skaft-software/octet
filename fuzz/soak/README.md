# Long-session soak harness

Runs the real `octet` binary with the bundled `octet-pi-compat` bridge and a
deterministic **local loopback fake provider** for N randomized turns, mixing
prompt submissions, tool calls, compaction, model switches, thinking-level
cycles, `reload`, `new_session`, abort cancellations and Pi UI updates. Every
turn is recorded, RSS is sampled per turn, and runtime invariants are checked
from the RPC state and process logs.

## Layout

- `soak.sh` — entrypoint (build discovery, env isolation, TMPDIR).
- `driver.py` — the seeded turn driver and invariant checker.
- `fake_provider.py` — loopback OpenAI-Responses SSE fake. Responses are a
  pure function of the request; no network, no credentials.
- `fixture/soak.ts` — the Pi fixture: registers a host-delegated provider
  (`soak` / `soak-model`), a `soak_probe` tool, a `soak_ui` tool and a
  `session_start` footer mount (deliberately unawaited, like real Pi
  extensions).

## Usage

```sh
# build once (debug is fine for correctness; release for lower overhead)
CARGO_TARGET_DIR=/path/to/target-fuzz cargo build -p octet-coding-agent --bin octet

fuzz/soak/soak.sh            # 300 turns, seed 0
SOAK_TURNS=60 fuzz/soak/soak.sh --seed 7
```

Deterministic: the seed drives every randomized decision and the fake
provider derives each reply from the request body, so a failing run
reproduces with the same seed. Failure details live in
`.tmp-fuzz/soak-<seed>/` (`octet.log`, `provider.log`, `summary.json`);
`--keep` preserves the directory (otherwise it is removed on failure).

## Invariants

Hard failures (exit 1):

1. **Fail closed on providers.** `set_model` switches to the extension
   provider (`m-` wire id of `soak-model`) *before* any prompt, re-pins it
   after every `reload`/`new_session`, and every assistant reply must carry
   the fake provider's signature (`ack:`/`probe turn`) — a live provider
   answering would fail the run.
2. **Process liveness.** Every turn is guarded by `process.poll()`.
3. **Typed terminal outcomes.** Every command must answer with an
   id-matched `response` (success or typed error); a lost response or an
   RPC `error` fails the run.
4. **Reload survival.** After `reload`, the extension provider must still be
   selectable and the next prompt must complete.
5. **RSS slope.** After the warmup fraction of samples (default 25%), the
   least-squares slope of summed tree RSS must stay at or below
   `--rss-slope-limit` MiB/turn (default 0.20).

Recorded, not failing:

6. **Mount-cancellation signature (known main bug).** On main the native
   reader cancels the Pi footer mount's `ui/open` when the `session_start`
   hook replies, so the bridge logs `-32800`/`request cancelled` diagnostics.
   The driver counts those signatures and reports
   `mount_cancel_signatures`; it is expected to be nonzero until the
   surface-lease fix lands and must become zero afterwards.

Known on main / unverified here:

- **Resolved, not a product bug: the apparent headless prompt stall.**
  `OctetRpc.wait_turn_end` held the condition (a non-reentrant lock) and
  called `events_since()`, which took the same lock: the driver deadlocked
  itself, and the reader thread blocked before logging, so `octet.log` looked
  like a stall after `agent_start`. A wire tap showed octet and the bridge
  completing the whole turn. The lock is now an `RLock`. After that fix, a
  `SOAK_TOOL` turn after `new_session` fails with "extension provider
  reported an error" because the driver re-selects the extension model
  without re-pinning the native route: a driver sequencing issue, open.
- **Bridge-delegated extension-model inference** (`soak/m-…`) resolves and
  registers, but its delegated `baseUrl` is not reproducible in this shared
  environment (a stale value was observed coming from shared mirror-cache
  state). The driver verifies registration/reselection and pins the native
  custom route for inference; extension-route inference stays unverified.
- Provider *streaming quality*, prompt latency, and terminal rendering are
  deliberately out of scope — this harness checks stability and invariant
  hygiene, not performance or visual output.
