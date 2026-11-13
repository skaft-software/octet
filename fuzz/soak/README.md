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
`$TMPDIR/octet-soak/soak-<seed>/` (`SOAK_RUN_ROOT` overrides the root) (`octet.log`, `provider.log`, `summary.json`);
`--keep` preserves the directory (otherwise it is removed on failure).

## Isolation guard

`guard.py` makes `soak.sh` and `driver.py` refuse to start (exit 2, with a
message) unless HOME, the Pi agent dir (`OCTET_PI_AGENT_DIR`,
`PI_CODING_AGENT_DIR`, `HOME/.pi/agent`) and the octet dirs (`OCTET_DATA_DIR`,
`HOME/.octet`) all resolve inside the run's own temporary directory, and the
run directory is outside the real home. The fixture only reads, and only from
an agent dir carrying the driver's `.soak-harness` sentinel. Test:
`python3 -m unittest test_guard` from `fuzz/soak/`.

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

6. **Mount-cancellation signature.** The driver counts `-32800`/`request
   cancelled` diagnostics on stderr and reports `mount_cancel_signatures`. The
   host used to cancel an unawaited `ui/open` when its parent hook replied; that
   is fixed and covered by `an_unawaited_ui_open_survives_the_parent_reply`
   (octet-agent). The headless RPC mode has no UI frontend that answers
   `ui/open`, so this counter is expected to stay zero and cannot by itself
   prove the mount survives.

Known on main / unverified here:

- **Resolved, not a product bug: the apparent headless prompt stall.**
  `OctetRpc.wait_turn_end` held the condition (a non-reentrant lock) and
  called `events_since()`, which took the same lock: the driver deadlocked
  itself, and the reader thread blocked before logging, so `octet.log` looked
  like a stall after `agent_start`. A wire tap showed octet and the bridge
  completing the whole turn. The lock is now an `RLock`.
- **Resolved: `SOAK_TOOL` after `new_session`.** The driver re-selected the
  extension model after `new_session` and left it selected, so the next prompt
  ran on the extension route. That failed ("extension provider reported an
  error", no request reaching the fake) for two independent reasons, both now
  fixed: (1) adapter bug: the bridge handed the extension's `streamSimple` a
  strict options object that threw `unsupported_feature` for any option Pi does
  not set (`headers`, `env`, `fetch`, `samplingParams`, ...), which the
  installed Pi's own stream helpers read; unset options now read as
  `undefined` (regression test in `provider-registration.test.mjs`). (2)
  fixture bugs: it read the provider URL from `process.env`, which the
  sandboxed runtime bridge does not have (the factory runs in the mirror
  capture and in the runtime bridge, and the runtime one registered the dummy
  `127.0.0.1:0` URL), it lacked the `apiKey` Pi requires with explicit models,
  and its `streamSimple` threw instead of streaming. The URL now comes from
  `<workspace>/.pi/soak-port`, and the stream delegates to pi-ai's OpenAI
  Responses stream.
- Both routes are covered: the default `--route native` pins the native
  custom route for inference, `--route extension` runs every inference turn on
  the extension provider (`soak/m-...`).
- Provider *streaming quality*, prompt latency, and terminal rendering are
  deliberately out of scope — this harness checks stability and invariant
  hygiene, not performance or visual output.
