# Integration source review — df5a7e80 + shared diff

Scope: concrete correctness/security regressions in this coding pass, with Browse background target creation, MCP HTTP ownership/network/deadline/framing, provider self-description, extension rescan/metadata/progress, and automation policy/runtime prioritized. Production/test files and Git state are read-only; only this record is owned by this reviewer. Not an independent public-release audit; no external/live automation or credentials.

## Progress checkpoint 1

- HEAD observed: `df5a7e809715961b9344af6b52e43a6ca48f56b3`.
- Initial tracked whole-tree `git diff --binary df5a7e80` SHA-256: `c60211b5a8fc31681c1f2c6e81eaf848c3f553202850a2bf3af6d46b314137f2`. Untracked files are not included in that diff, so explicit source hashes follow.
- Read the complete workers' current Browse/MCP/automation/hooks/provider execution receipts, then complete Browse README/reference/qualification. Existing blockers are acknowledged, not new findings: initial-launch/popup focus, unsupported native Firefox/Safari, missing trusted computer-use API 0.3 authority, absent qualified model-code containment, experimental remote MCP ownership gate, and unrun physical/live qualification.
- Source review in progress. No independent tests or live actions run at checkpoint 1. Other writers still active; salient sources will be rehashed/rechecked before completion.

## Progress checkpoint 2 — actionable source findings (pending final recheck)

1. **P2 / Browse deadline regression:** `extensions/octet-browse/octet_browse/worker.py:1118-1143,1154-1170` uses synchronous `new_browser_cdp_session`, `CDPSession.send` and `detach` without an operation-bound timeout. A CDP create/probe/cleanup response that never arrives holds the sole Playwright owner thread after the caller's deadline; future status/close/open requests and queued shutdown cannot run. Before this pass, `context.new_page()` used the context's 5000 ms default timeout. This is not the already documented physical-focus limitation. Installed pinned 1.57.0 source confirms `_impl/_cdp_session.py:31-38` passes `None` as timeout, the dispatcher runs with `validParams?.timeout`, and `ProgressController.run` installs no timer without it. Minimal repair: use an owner-safe bounded CDP operation mechanism for every new CDP await (including cleanup), cancel/fail closed at the absolute operation budget, and regress a send that never completes rather than only cancellation after a successful send.
2. **P2 / macOS native release contract mismatch:** `extensions/octet-computer-use/octet_computer_use/backend_macos.py:257-262,1255-1266` now requires `release_all() is True` and raises/terminally stops after every successful input otherwise. The actual `macos/native.py:924-958` implementation still returns `None` on every path; only the fake at `tests/test_backend_macos.py:74` returns True. Thus even a normally completed real click/type/press/scroll/drag becomes `input_release_unverified`/unknown effect and stops the runtime; clean stop also always reports degraded. Minimal repair: update the native release API to return trustworthy success/failure (including key/button release failures) and test that actual adapter contract with native calls mocked; do not weaken exact-boolean checking or infer physical release from void best effort. Existing native qualification blockers do not erase this deterministic integration mismatch.

3. **P2 / disabled remote affects normal stdio ownership:** `extensions/octet-mcp/octet_mcp/manager.py:382-391` binds/rejects every `/mcp` command whenever any Streamable HTTP descriptor exists, even `enabled=false` with the experimental gate off. After owner A reads `/mcp status`, owner B cannot even `/mcp show local` (nor manage local stdio servers), despite no remote session/credentials/work ever being admitted. Minimal repair: keep disabled/gated remote descriptors inert and apply remote owner fencing only where remote state/operations actually require it; preserve existing stdio command access. This differs from the documented limitation of an active remote resident not supporting owner migration.

### Offline reproductions observed

- `PYTHONDONTWRITEBYTECODE=1 ... python3 -B` in-memory MCP probe constructed a manager with one stdio descriptor plus one disabled remote, gate false; A's status returned normal `mcp 0/2`, B's `show local` returned `Remote MCP owner mismatch; restart the extension for this host owner.` Asserted `_executor is None`: no processes, DNS, network or scratch writes occurred.
- In-memory native probe bypassed `MacOSNative` construction via `__new__`, set loaded state and empty held-input sets, called actual `release_all` and actual `MacOSBackend._safe_release_all`: observed `None` / `False`. No native OS call or permission request executed.
- Synthetic Browse probe used actual `_new_page_without_activation` with event-blocked `Target.createTarget`. At >100 ms beyond a 25 ms operation deadline, and after abandonment, the helper thread remained blocked; it returned `BrowseError`/cleaned up only after manually releasing the fake transport. All threads joined; no browser, profile, native API or files accessed.

## Progress checkpoint 3 — provider precedence and acknowledged blockers

4. **P2 / self-description overrides a supported explicit context alias:** `crates/octet-coding-agent/src/app/bootstrap.rs:1286-1293` (`self_described_entry`; final recheck line numbers) omits `meta/n_ctx_train` from the context-assertion guard, although the existing custom decoder honors it at `:4238` and `:4269`. A custom inventory entry with `meta: {"n_ctx_train":8192}` plus a valid self-description declaring 96000 context/4096 output gains synthesized top-level `context_window=96000`; `extract_ctx_from_model_entry` then chooses it ahead of the explicit legacy 8192 limit. The host can therefore budget/send context beyond the endpoint's asserted limit, contrary to the new per-leaf precedence contract. Minimal repair: include `meta/n_ctx_train` in the assertion aliases, and regress a conflicting valid self-description against every supported legacy context alias. Source-traced; no Rust execution of this new edge case claimed.

- Also traced the mixed `supported_parameters`/parallel fallback edge: the self-description guard treats a parameter list as authoritative, but the builtin `parallel_tool_calls` decoder ignores that list and falls back to true for Responses. This permissive fallback already exists without self-description at the baseline; not counted as a newly introduced regression. A mixed-assertion fixture would improve the new feature's coverage, but the current review does not inflate it into an independent new safety finding.
- Re-read the fully updated automation README and CONTRACT. They now explicitly acknowledge that `MacOSNative.release_all` lacks the required acknowledgement and only the fake supplies it. **Checkpoint-2 item 2 is therefore retained as confirmed integration/qualification blocker, not counted in the final new-regression total.** A real composed action still cannot complete normally with that adapter; do not claim native readiness from mocked results. The stricter boolean check itself is an intentional fail-closed improvement and should not be weakened.
- Read complete provider guide/design, thinking and snapshot-source references; current/legacy extension guide, API 0.3 and API 0.2 protocol references, hook-enrichment contract, capability ownership reference, and minimal API 0.3 example. No new authority bypass found in the reviewed metadata/progress test-only changes or generation-fenced rescan consumer. General configuration/migration PostMutation delivery remains explicitly unimplemented rather than an invented regression.

No production/test edits made. Final salient-source/hash recheck in progress; full test/release qualification is parent-owned.

### Initial source SHA-256 snapshot

```text
45fcfcb7de1d4cb06e4c40bf38c28cc2a72c7e685518722ec5a8cc92a98a1bb1 extensions/octet-browse/octet_browse/worker.py
a01d1ae50edab7fa6f3138d0dc4d321ea2f48d37626d7c1ec250c4e558b6bc42 extensions/octet-mcp/octet_mcp/manager.py
773a8ded1b787a39d47ec0500421ecf824913c36a726d7cc6feb47bee785fd2c extensions/octet-mcp/octet_mcp/runtime.py
856c874cea53235c9b4fd200a661f5f9ed51f2a196c67bb597d889cb4932f89e extensions/octet-mcp/octet_mcp/streamable_http.py
45f419c985198829db6918e678f1cd756663c2a130fb22771e14d3282c49e676 extensions/octet-mcp/octet_mcp/http_network.py
53bebfcd4312d5511a308b262a870998f95e2a98e4a48b869b8d27eab47606df extensions/octet-mcp/octet_mcp/ownership.py
f6ce91d6ec14156eac2327f95137b2c287cabdd9a6e5fd5272db4ce0f3771ac7 crates/octet-ai/src/discovery.rs
56cacc5def6bffa56f3707eefdaaa2667fd4fe65bbdd7b26b2bf0a4fe7321243 crates/octet-ai/src/client.rs
30b64d34c8e16ad88d18b27f17bffe542bec5f1e7793a453c16b387549c4d7cf crates/octet-ai/src/protocol/mistral_conversations.rs
10ecb959094cd68547438a951628d98d3e0e907506593b47572d140926115c12 crates/octet-coding-agent/src/app/bootstrap.rs
9b8540242e01c41b160119944e8d494b9e467236184ee2aac80dd931308a8944 crates/octet-coding-agent/src/app/mod.rs
7dabb06b3138fa9876522db201839cbe932dd34f3aba214b2b7d1a12b4ea76df crates/octet-coding-agent/src/extensions.rs
71a3716edf180017f1a06b473212a361d098c30d753fe70e26ef436b5ef76ac3 crates/octet-agent/src/extension.rs
f5dce52792dcaf26708e2393584c4e058005a08c842b0b36a305ee4f5e423506 crates/octet-agent/src/extension_process.rs
39e748a96cd6cfa3fa42aee0d5f2030b6ba48c4ec693e663e35e7aa89cfbba3a extensions/octet-computer-use/main.py
6fcbf43644a8597c3210d11063debe1cce4152f96894e17fc92f40c40221f6bd extensions/octet-computer-use/octet_computer_use/policy.py
54982cf10f79e5f31c98a7d85d369bc36ce45332341b16c8532d68fd731baca6 extensions/octet-computer-use/octet_computer_use/runtime.py
525c86f87d1e1fbe0d71d4a3e57a8995ff69fde0303609f5d9713de2446168c6 extensions/octet-computer-use/octet_computer_use/code_runtime.py
46a8b2b70eabe9ef13b405ed9f49e1f838ba781d1777d6d31177b2d33564084b extensions/octet-computer-use/octet_computer_use/lifecycle.py
14a3d7c48ed005366a5dbb3a02147821e609773552369e76daa4b191a98d21ee extensions/octet-computer-use/octet_computer_use/backend_macos.py
```
