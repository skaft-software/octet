# Current-release Quickstart candidate qualification

**Status: candidate documentation and source-contract record, not a release,
live-provider, human-success, issue-closure, or Windows-support claim.** This
note accompanies [Getting started](../getting-started.md). It records what was
checked in this checkout and what a separate verifier or fresh user must still
do.

## Scope and expected first success

The guide now gives one bounded journey:

1. select a published or intentional source-build installation lane;
2. run Octet from the repository being worked on;
3. choose one of Codex subscription, OpenRouter API key, or LM Studio/local
   OpenAI-compatible server;
4. inspect the route with `/status` and make a read-only inference using only
   `read` and `search`;
5. start a change-capable safe-mode session, approve effects, inspect the diff,
   and stop before commit.

The expected first outcome is an answer that names real repository paths and a
verification plan, followed by a small user-selected change whose diff is
reviewable. The guide labels shell commands separately from text entered inside
the TUI. It keeps credential values out of examples, prompts, URLs, history, and
fixtures.

This is documentation-only in this root. No new Rust fixture was added because
the existing provider-setup unit tests already exercise the isolated setup
transaction, review/cancel, offline/manual path, compare-and-swap protection,
and secret-free receipts.

## Release and platform boundary

- The workspace package version is `0.7.6` (`Cargo.toml:12-19`). The published
  native scope is macOS Apple silicon/Intel and GNU/Linux x86-64. The release
  page, not a source checkout or version string alone, establishes signed asset
  availability.
- A source build uses Rust 1.86+ and ripgrep on macOS or GNU/Linux. It may report
  `0.7.6` while containing unreleased checkout changes; it is not a signed
  replacement for the installed binary.
- The checkout contains the `octet setup` CLI contract documented below. A user
  of a published binary must confirm `octet setup --help` and `octet --help`
  before using candidate-specific forms. This note does not backdate checkout
  behavior into every published artifact.
- There is no published native Windows download in this snapshot. WSL,
  cross-compilation, macOS, and GNU/Linux evidence cannot qualify native Windows.
  Windows requires a later matching artifact/checksum and physical acceptance.

## Source contract checked

| Contract | Source evidence | Documentation consequence |
| --- | --- | --- |
| Setup requires an explicit endpoint selection | `crates/octet-coding-agent/src/cli.rs:27-86`; `provider_setup.rs:793-816` | Use `--preset lm-studio` or `--endpoint`; never imply an automatic localhost scan. |
| LM Studio uses the explicit default URL | `provider_setup.rs:182-189,809-814` | Advertise `http://localhost:1234/v1/` only after the user selects the LM Studio preset. |
| Discovery is bounded and endpoint-scoped | `provider_setup.rs:459-521,602-637` | Explain one `/models` probe to the selected endpoint, no redirect following, and no setup scan of localhost or the network. |
| Manual/offline setup avoids setup discovery | `provider_setup.rs:659-674`; `run_cli` at `:755-767` | Use `--manual-model`; pair it with `--offline` when a no-probe recovery is required. |
| Review precedes persistence | `provider_setup.rs:676-699,725-790`; `modes/interactive.rs:5055-5103,5153-5219` | Preview without `--yes`, inspect the secret-free receipt, then explicitly confirm; CLI `--cancel` returns before discovery, while an interactive cancellation after discovery cannot undo that request. |
| Registry writes are protected | `provider_setup.rs:573-599,881-941`; `auth/custom/store.rs:324-330,424-448`; `crates/octet-agent/src/secure_fs.rs:406-454` | Explain owner-private atomic storage, explicit replacement, and stale-snapshot failure. |
| Guided startup has the same local/manual/retry choices | `modes/interactive.rs:4815-5225` | Tell users that setup selection, discovery, review, and commit are separate steps. |
| Codex is a hosted device login | `auth/codex/login.rs:18-80`; `auth/codex/store.rs:1-64` | Use `octet --login codex` or `--headless`; do not ask users to paste an API key. |
| Codex inventory is account-scoped | `app/bootstrap.rs:4659-4770`; `auth/codex/mod.rs:41-52` | Treat fallback IDs and plan names as non-authoritative until listed by the account. |
| OpenRouter is a built-in environment route | `providers/declarations.json:135-169`; `app/bootstrap.rs:2341-2390` | Load `OPENROUTER_API_KEY`, select a listed `openrouter/...` model, and avoid a misleading custom route. |
| Workspace trust and effect safety are independent | `cli.rs:287-332`; `config.rs:397-499`; `docs/tools.md` | Begin without `--workspace-trusted`, use `--safe-mode`, and review project instructions before admitting them. |

The selected custom model uses the ID form `custom/<provider-id>/<model-id>`;
the CLI persists that preference only after catalog verification
(`provider_setup.rs:301-305,679-699,780-788`). `--model` and `--manual-model`
are deliberately distinct setup inputs. The provider setup receipt reports the
credential policy, not a credential value (`provider_setup.rs:86-95,308-323`).

## Proposed acceptance checks (not run here)

The following are requests for centralized verification and fresh-user QA, not
results from this source-only session.

### Static and source checks

- Check Markdown links and code-fence labels for `docs/getting-started.md` and
  this file. Confirm all shown setup flags remain in the generated help for the
  candidate binary and that the published v0.7.6 docs do not imply unpublished
  channels or native Windows support.
- Run the existing focused provider-setup tests with the verifier's run-root
  Cargo prefix and `--locked`. At minimum cover the existing tests for explicit
  endpoint selection, discovered commit, cancellation/offline, manual offline
  rebuild, replacement/CAS, redaction, and validation. No coding root may run
  Cargo; `verify-rust` is the sole current Cargo owner.
- If the coordinator requests a dedicated docs contract test, add it only under
  the explicitly owned `crates/octet-coding-agent/tests/quickstart_current.rs`;
  this root did not add a speculative test that would duplicate source tests.

### Fresh-user matrix

For each published target, use a disposable owner profile and a fresh target
repository. Record binary identity, OS/architecture, terminal, and whether the
route is release or checkout candidate:

1. macOS Apple silicon, macOS Intel, and GNU/Linux x86-64: install the pinned
   v0.7.6 asset, check `octet --version`, run from the target repository, and
   complete the read-only first request.
2. Source candidate on macOS and GNU/Linux: build the checkout separately, use
   its full binary path, and do not mix it with the installed release.
3. Codex: complete device login, exercise the browser and `--headless` paths,
   discover the account inventory, select only a listed ID, and confirm that
   login/model diagnostics do not reveal tokens or device material.
4. OpenRouter: provide the key through an environment/secret-manager boundary,
   discover a model available to that account, confirm the route in `/status`,
   and perform the read/search first request. Record account/provider/model
   identity without recording the key.
5. LM Studio: start the server and model, exercise a review-only preset setup,
   confirm cancellation leaves no registry change, then commit after review and
   run the first request. Repeat with `/models` discovery and with
   `--offline --manual-model` to prove the no-probe recovery; a manual receipt
   alone must not be called connection success.
6. Local compatible server: repeat with an explicit `/v1/` endpoint and both
   no-auth and environment-backed bearer-key policy in separate disposable
   profiles. Confirm `--no-auth` and `--api-key-env` cannot be combined.
7. Safety/trust: first run with `--safe-mode --tools read,search --no-edit
   --no-write --no-process --no-context-files`, then review project instructions
   before a later `--workspace-trusted` change-capable run. Confirm every
   mutation and shell call remains approval-gated and executable extensions stay
   stopped.

### Recovery matrix

Exercise each recovery without exposing secrets or overwriting unrelated state:

- missing Codex credential, expired/refresh-invalid credential, unavailable
  account model, and browser-open failure;
- missing OpenRouter environment value, rejected authentication, empty/changed
  model inventory, and offline startup with no fresh cache;
- stopped local server, wrong endpoint, empty `/models`, manual model fallback,
  and a manual setup followed by a real inference attempt;
- preview/cancel, `--yes` commit, existing provider requiring `--replace`, and a
  concurrent registry change;
- unavailable resumed model, print/RPC unresolved-model behavior, and a
  reviewed workspace that is intentionally not trusted at first launch.

For every case, record the next actionable message, whether disk changed, and
whether any provider request occurred. Do not infer live-provider qualification
from deterministic fixtures or from a model name.

## Comparison and licensing boundary

No Codex or Pi source was copied into this onboarding change. The existing
comparison record pins the read-only `openai/codex` source at commit
`3d3df0a0cad5d3d8d3340b633787e9dd304ea463` and describes its behavior in
`docs/qualification/v0.7.4-recovery.md:86-113`; that comparison is not a parity
claim. The pinned Pi compatibility target remains
`@earendil-works/pi-coding-agent@0.84.4`; do not silently replace it with a
newer checkout version. The inspected Codex reference is Apache-2.0 and the Pi
reference is MIT. Since this change uses only Octet's native source and does not
reproduce reference code, no third-party implementation text or license notice
is being added here.

## Missing evidence and hard limits

- No live Codex, OpenRouter, LM Studio, or other provider request was made in
  this root. Credentials and real model IDs were not used.
- No fresh human followed the guide without developer assistance, so the
  unassisted-success acceptance criterion remains open.
- No published release installer was executed, no signed asset/checksum was
  retrieved, and no terminal/PTY, color, scrolling, or session-resume journey
  was physically exercised here.
- Native Windows hardware and matching artifact/access were not yet available to
  this phase, so no native Windows process/browser/WSL acceptance was run. The
  offered hardware is a later gate after Linux quiescence and matching
  artifact/checksum readiness; the guide intentionally makes no Windows support
  claim.
- Candidate source behavior and documentation were inspected; Cargo tests,
  builds, formatting, packaging, website checks, and remote/CI checks remain
  unrun under the phase rules.

## Remaining ownership requests

1. **verify-rust (sole current Cargo owner):** run the focused source tests and
   any candidate build/check on a frozen snapshot with the run-root Cargo
   prefix; do not delegate the build slot to readable-activity.
2. **Coordinator/integration:** review this two-file documentation diff and
   reconcile the candidate/release wording with the active `#275` setup root;
   do not collect or overwrite its files from this root.
3. **Human provider QA:** run the three fresh-account provider lanes on the
   published macOS/Linux targets, record exact binary identity and recovery
   outcomes, and redact all credentials/device codes.
4. **Windows owner/coordinator:** after Linux work is quiescent, arrange the
   offered Windows hardware, matching artifact/checksum, and a separate native
   Windows/WSL acceptance plan. No Windows source edit is requested by this
   root.
5. **Website owner:** treat the website as read-only for this task; if the
   quickstart needs navigation/search placement, request a separate reconciled
   handoff rather than editing its dirty checkout here.
