# Last-night integration — state and remaining failures

Written 2026-09-15 after merging every recoverable artifact from the
2026-09-14/15 swarm onto local `main`.

## Where the work is

| What | Where |
| --- | --- |
| Integration branch (47 commits) | `swarm-c88b634b2246/integration` @ `fffb5665` |
| The coordinator's assembled state (1419 files) | commit `48695e19`, branch `swarm/lastnight-assembled` |
| Union of both, on main | `6761091e` (merge) + `e9692c7e` (compile fixes) + `cf4ab5cf` (exec bits) |
| Pre-merge main | tag `pre-lastnight-union` (`e2914a62`) |
| Roadmap / worklist (142 items) | `BACKLOG.md` |
| Durable backup of all swarm refs + receipts | `../.octet-swarm-backup-20260915/` |

Provenance of the assembled tree: `resume1-assembly.tar`, sha256
`bb6ab660fc41da33fb94461e8f5fcd1e0a9dbf7b37e0c068b88593fd65948790`, identical
locally and on Temper. The swarm's own `resume1` verification queue ran only its
first command on Temper (2026-09-15T12:37Z, exit 101) and stopped; its 13
remaining commands were run locally here for the first time.

Per-file audit before merging: all 105 files changed by the integration branch
are byte-identical or already-applied in the assembly (0 absent), and all
main-side changes are present. Nothing was dropped in the union.

## Green

- `cargo check --locked --workspace --all-targets` — exits 0.
- 12 of the swarm's 13 queued commands pass, including
  `octet-ai --lib` (310), `octet-agent --lib` (525),
  `octet-coding-agent --lib` (1248 of 1249), `api_v03_runnable`,
  `copilot_host_current`, `copilot_current`, `mistral_current` (14 of 16),
  pi/copilot/presentation unit filters, prompt history, pinned provider fixtures.

Fixed to get here: 4 compile errors (see `e9692c7e`), 37 lost executable bits
(`cf4ab5cf`), and the two Pi fixture-path failures below.

## Remaining failures (3)

### 1–2. `crates/octet-ai/tests/mistral_current.rs` — 2 of 16

```
invalid_destinations_reject_before_credentials_without_echoing_url_secrets
  panicked at crates/octet-ai/tests/mistral_current.rs:657
  assertion failed: matches!(error, AiError::Config(ConfigError::InvalidBaseUrl(_)))

eof_is_not_native_done_even_after_valid_arguments
  panicked at crates/octet-ai/tests/mistral_current.rs:444
  assertion failed: matches!(root_error(&error.unwrap()),
      AiError::StreamProtocol(StreamProtocolError::MissingFinish))
```

The Mistral codec does not classify these two cases as the tests require. This
is the boundary the swarm recorded as "partial Mistral declarations remain
held": the codec test suite was written against the intended contract, and the
imported implementation is incomplete at these two points. Behavioural decision
required, not a mechanical fix.

### 3. `crates/octet-coding-agent/src/pi.rs` — 1 of 1249

```
pi::tests::generated_link_negotiates_runtime_commands_with_the_real_octet_host
  panicked at crates/octet-coding-agent/src/pi.rs:3374
  called `Result::unwrap()` on an `Err` value: Closed("extension stdout closed")
```

The generated Pi extension link starts (`ExtensionProcess::start` with a
canonical fixture root) but the spawned node bridge closes stdout before
negotiation. The earlier failure in this test was a macOS
`/var` -> `/private/var` fixture-path problem, now fixed; this is the next layer
and needs the bridge/`fake-pi` environment looked at directly.

## Flaky, not broken

`update::progress::tests::actual_updater_progress_pty_and_plain_streams` re-runs
its own test binary with `communicate(timeout=2)`. Under the full 1249-test run
it exceeded that budget; run alone it passes in 8.07s. Widen the timeout or mark
it serial before treating it as a regression.

## Known noise

- `cargo check` emits ~26 warnings in `octet-coding-agent`, mostly unused
  imports and dead code in `presentation/`, `tui/composer/`, `pi/package.rs` and
  `auth/copilot/` — facades left re-exporting items no longer referenced after
  the module splits.
- This repo has `core.fileMode=false`, which is why the archive's executable bits
  were silently dropped on import. Anything transferring trees here must set
  modes via `git update-index --chmod=+x`.

## Reproduce

```sh
cargo check --locked --workspace --all-targets
while read -r c; do bash -c "$c"; done < ../.octet-swarm-backup-20260915/resume1-queue.txt
```

Per-command logs and receipts: `../.octet-swarm-backup-20260915/queue/`.
