# Automation execution evidence

Baseline: `df5a7e80` plus this worker's uncommitted diff (no commits). Shared-tree scope: `extensions/octet-computer-use/**`, `crates/octet-coding-agent/src/host/**`, computer/automation integration tests, this file.

## Contract inspection

Read the complete computer-use CONTRACT/README, root README/SECURITY, extension guide, API 0.3 reference, legacy authoring/security reference, capability-ownership design, and API 0.3 minimal example README before editing.

**Host integration blocker (#383):** API 0.3 offers no `policy/evaluate`, `approvals`, target-selection service, or trusted computer stop/takeover service (`docs/extensions/API-0.4-REFERENCE.md`). The existing Python gate's legacy intent omits exact action evidence. `crates/octet-coding-agent/src/host/policy.rs` is the separate native protocol-1 controlled host, which deliberately never starts executable extensions. It cannot safely be turned into the missing coding-product extension authorization adapter. Changes to schema/agent/extensions.rs require the parent's other owner. No cooperative confirmation, model context, or native permission flag will be treated as that authority.

**Containment blocker (#391):** the source runtime currently labels prerequisite availability as genuine containment and can fork inherited host memory/FDs. Namespace setup does not isolate the current process into a new PID namespace and does not eliminate inherited descriptors or capabilities. No qualified OS model-code sandbox is present. Fail-closed rejection is independently implementable; no model-code worker or OS setting will be exercised.

## Verification log

- Initial `git status --short`: clean.
- No tests run yet. Native macOS/Windows, permission UI, live desktop, provider, installed bundle, and packaging qualification remain explicitly unrun.

### Checkpoint 1 — local scope and composition

- Root-cwd discovery command failed with three `ModuleNotFoundError: octet_computer_use` imports (test invocation lacked extension cwd); corrected invocation is below, not a product failure.
- Baseline `cd extensions/octet-computer-use && python3 -m unittest discover -s tests -p 'test_*.py'`: **32 tests passed**.
- Added exact local `evaluate_action`/scope/parent/evidence one-use checks and a trusted-local macOS lifecycle composition; no Rust host authority or wire methods invented. Standalone remains inert.
- First expanded run: **64 tests, one error** (native identity exception was not translated to the lifecycle boundary). Fixed that boundary. One attempted fix used the wrong cwd and made no edits; the rerun reproduced the same error.
- Corrected expanded run: **64 tests passed** (`Ran 64 tests in 0.169s`, Python 3.14.7). All native adapters synthetic.
- `python3 -m py_compile main.py octet_computer_use/policy.py octet_computer_use/runtime.py octet_computer_use/lifecycle.py octet_computer_use/code_runtime.py`: passed at the earlier 32-test checkpoint. Later edits still need final compilation/test rerun.

### Final checkpoint — `df5a7e80+diff`

Observed baseline confirmed by `git rev-parse --short=8 HEAD`: **df5a7e80**. Interpreter: **Python 3.14.7**. No commits, git-state changes, global formatter, OS permission changes, live desktop input, provider calls, or model-code workers were used.

Implemented source changes:

- `extensions/octet-computer-use/octet_computer_use/policy.py:797`: bounded registered grants replace digest/frame-only replay tracking. `PolicyGate` binds scope, parent request, exact action, private observation/native evidence, expiry, one-use token retry and concurrent revocation. It refuses legacy intent-only evaluators and manual credential classes. Continuing recaptures require live observation scope.
- `extensions/octet-computer-use/main.py:116` and `octet_computer_use/runtime.py:42`: explicit trusted-local macOS composition connects actual entrypoint/lifecycle/policy/backend code, lazily constructs only an explicitly supplied backend, and supports observe/exact AX-center click/navigation keypress/focused editable type/scroll. Unsupported operations are denied, not approximated. This is not a new host wire method or production authorization adapter.
- `octet_computer_use/runtime.py:220`: fresh evidence after policy, final one-use redemption, no native-identity copies accepted from model output, unknown acknowledgements terminal, and no inference of task success from input.
- `octet_computer_use/lifecycle.py:1723` and `:2413`: late captures cannot revive stopped evidence; approval/cleanup acknowledgements must be actual booleans. Main cancellation/EOF/shutdown/response loss revoke scope and settle the runtime. Native stop is terminal and status becomes not-ready.
- `octet_computer_use/backend_macos.py:1081`: permission/foreground/window/AX revalidation after separate native confirmation; missing or failed explicit input-release acknowledgement cannot be reported as success. The old native helper remains best-effort, so its missing acknowledgement remains a qualification blocker rather than fabricated release evidence.
- `octet_computer_use/code_runtime.py:1068`, `:1082`, `:1357`: remove unqualified fork dispatch and reject all in-package code execution; primitive presence, metadata booleans and injected sandbox objects cannot enable model source. Direct worker entry is inert too.
- README/CONTRACT now distinguish implemented synthetic composition from absent host services, unavailable model-code containment, unsupported operation/media integration and native/packaging qualification.

Deterministic verification before the final dispatch-hook refinement:

| Command (extension cwd unless stated) | Observed result |
| --- | --- |
| `python3 -m unittest discover -s tests -p 'test_*.py'` | **89 passed**, final expanded run 0.053s |
| Same command in a three-iteration shell loop | **89 passed each**, 0.054s / 0.053s / 0.052s |
| `python3 -m py_compile main.py octet_computer_use/*.py tests/test_policy.py tests/test_runtime.py tests/test_code_runtime.py tests/test_lifecycle.py` | Exit 0 |
| Root `git diff --check -- extensions/octet-computer-use docs/swarm-audit/EXECUTION-automation.md` | Exit 0 |

New deterministic suites: `tests/test_policy.py`, `tests/test_runtime.py`, `tests/test_lifecycle.py`, `tests/test_code_runtime.py`. Existing macOS/Windows/screenshot suites also ran. The macOS fixture now explicitly acknowledges release; this is synthetic evidence only. Intermediate expanded runs of 73, 86, 87 and 88 tests also passed as coverage was added. Result inspection included tracked diffs plus the newly added runtime and tests. No Rust files were modified and no Rust build/test was run for this Python-only patch.

## Exact remaining blockers / issue disposition

- **#383 remains partial/blocked at the product host boundary.** No generated API 0.3 automation policy/approval/target selection/trusted control service exists. No Rust host authority was fabricated. The parent/other owner would need an explicit negotiated contract and coding-product frontend/session integration outside this worker's ownership. A local evaluator contract and synthetic approval fixtures do not close the host issue.
- **#385 remains partial.** Source macOS composition is covered by mocks; real native action/identity/focus/input-release/takeover qualification and all Windows/native packaging parity evidence are unrun. In particular the existing MacOSNative release helper does not return a verified explicit acknowledgement.
- **#386 remains partial.** The supported trusted-local operation subset now has tested persistent observation/action lifetime and trusted local stop/EOF/cancellation cleanup. Host-provided generation rollover, actual frontend takeover wiring, process-loss release, Windows composition and screenshot/API 0.3 artifact projection are not implemented/qualified here. `start`, `double_click`, `drag`, `move`, `wait`, `screenshot` are denied in the composed runtime.
- **#391 remains partial.** Unsafe availability/dispatch claims are fixed fail-closed with tests. No qualified OS launcher exists; model-code execution is unavailable, not purportedly contained. Full containment requires a reviewed host launcher with independently evidenced descriptor/memory/PID/capability/syscall boundaries and bounded lifecycle bridge semantics.

Artifact/session reference: this file plus the shared uncommitted source diff; no additional octet session/artifact identifier was exposed to this worker.

### Final dispatch-hook refinement and verification

A last diff/security review moved one-use redemption into the native backend's explicit local authorization callback **after** permission/identity/AX revalidation, immediately before input. The added regression expires the grant inside native revalidation and observes zero input calls. This is still a trusted-local dependency, not a negotiated host service or a replacement for the #383 blocker above.

Final source verification (supersedes the 89-test count above): **90 tests passed** in the expanded run (0.059s), then **90 passed in each of three repeats** (0.056s / 0.054s / 0.054s). The documented `py_compile` command again exited 0. Final scoped `git diff --check` exited 0. No native/code worker/platform qualification was added by these tests. Remaining issue dispositions are unchanged.
