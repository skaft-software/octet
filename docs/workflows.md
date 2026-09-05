# Build useful workflows with octet

[Documentation](README.md) · [Source reference](current-reference.md) · [Security](../SECURITY.md)

These recipes connect checked-in examples to useful outcomes. They are
**source-grounded instructions, not recorded successful 0.7.0 demonstrations**.
Commands deliberately use the current pre-rename `ygg` spelling. Build both
binaries using the [checkout instructions](../README.md#build-this-checkout).
Run commands below from the checkout root; substitute your actual configured
model for `MODEL_ID` and your task for the example prompt.

Use a disposable repository and isolated account, VM, or container for
experiments. A source build does not replace installed binaries, but running it
can still read and write the current user's `.ygg` configuration, credentials,
and sessions. Do not use a personal Ygg home to qualify the clean octet rename.
Only provide credentials and workspace data the selected provider may receive.
`--offline` suppresses optional discovery, not inference network traffic.

## 1. Make a reviewable repository change

**Outcome:** a narrow patch, a regression check, and a session that another
person can inspect or resume—not just an answer saying the defect is fixed.

The checked-in [grounded implementation template](../examples/prompts/grounded-implement.toml)
expands the current workspace and task, then asks the agent to inspect, keep the
change scoped, and run the smallest relevant verification. It is a prompt, not
a substitute for tool-policy enforcement or review.

1. Select a cloud or local route using [provider setup](current-reference.md#quick-start).
   Record the model ID and reasoning setting so another run is comparable.
2. Invoke the template from an explicit source, without copying it into global
   configuration or trusting every project resource:

   ```sh
   ./target/release/ygg --model MODEL_ID --safe-mode \
     --no-context-files --tools read,search,edit,write,bash \
     --prompt-template ./examples/prompts/grounded-implement.toml \
     --prompt grounded-implement --debug-prompt \
     'Reproduce the reported parser failure, fix its root cause, and add a regression test. Do not commit. Report the exact checks and any remaining uncertainty.'
   ```

   Review the expanded prompt and source hash. `--debug-prompt` is a diagnostic,
   not a dry-run switch; the invocation can continue to a provider request.
   Inspect and approve each proposed shell call and workspace mutation. Safe
   mode does not start executable extensions.
3. Require the exact failing input, the root-cause location, and a test that
   fails before the fix and passes after it. Review the resulting diff and
   broader relevant checks yourself. If verification cannot run, keep that
   limitation in the handoff rather than treating a model's conclusion as proof.
4. In the same interactive session, name and export the work:

   ```text
   /name parser regression
   /export ./parser-handoff.ygg-session.json
   ```

   Use `./target/release/ygg --continue` in the same workspace to reopen the
   latest session, or use `--resume SESSION_ID` to select it explicitly. The
   [session contract](sessions.md) explains branching, narrow torn-tail repair,
   and redaction. Export is redacted by default, but review the file before
   sharing it; arbitrary prose can still contain private information.

**Evidence to retain:** reproduction command/input, patch, before/after check
results, model/reasoning and source revision, session ID, and a reviewed export.
Prompt selection and its hash are session provenance; they do not certify that
the model obeyed every instruction.

### Delegate independent investigations

When two investigations are genuinely independent, keep one parent responsible
for the patch and final verification. The checked-in
[subagents package](../extensions/ygg-subagents/README.md) and its
[skill](../extensions/ygg-subagents/skills/ygg-subagents/SKILL.md) describe the
owner-bound service, optional ceilings, continuation, and explicit stop behavior.

In a **separate full-access, OS-isolated run**, use the matching checkout
extension without installing or replacing a bundle in your personal home:

```sh
./target/release/ygg --model MODEL_ID --no-context-files \
  --extension-dir ./extensions \
  --enable-extension ygg-subagents --trust-extension ygg-subagents
```

Check `/extensions status` for the selected source and successful `agent_sessions`
negotiation before asking for delegated work. Python 3.9+ is required; this
package vendors its SDK. If version or service negotiation fails, stop and
reconcile the exact-source bundle—do not grant broader authority as a workaround.
Then ask:

```text
Investigate this parser failure. Delegate exactly two independent investigations:
parser-path traces the parsing path; regression-map inspects existing tests.
Give each worker only read and search tools, use the inherited model, and ask for
path:line evidence and uncertainty. Neither worker may edit or run commands.
Keep the parent responsible for deduplicating and verifying their findings.
Do not implement a fix until I ask.
```

The parent must actually request `tools: ["read", "search"]` at spawn: the
extension's default grants all five standard tools. Inspect `/subagents` and
wait for authoritative settlement; a spawn or stop acknowledgement is not
completion. Shared cwd is not filesystem isolation. There are at most eight
active children and depth one, not an arbitrary recursive team. Child usage
is accounted separately from parent prompt context. Record accepted unique
findings and failed/cancelled workers, not just a polished combined answer.

## 2. Build and use a domain extension

**Outcome:** give the agent a small, named operation with a typed argument
schema and predictable output, instead of repeatedly explaining a shell
procedure. Keep domain behavior outside the native model loop.

Start with [git-tools](../examples/extensions/git-tools/README.md), not a new
framework. Its [manifest](../examples/extensions/git-tools/extension.toml)
connects `git_status` and `/checkpoint` to the
[Python handler](../examples/extensions/git-tools/extension.py). The handler
validates options, runs Git without a shell and with a five-second timeout,
limits the returned entries, and creates no commit. `/checkpoint` is a status
**preview**, not a durable Git checkpoint. This remains trusted process code;
Git and repository configuration are not an OS containment boundary.

1. Inspect the complete example and [Python SDK contract](../sdk/python/README.md).
   In your isolated development environment, install the SDK for the Python
   interpreter used by the extension's shebang:

   ```sh
   python3 -m pip install ./sdk/python
   ```

2. First run the existing example against a disposable checkout. Git must be
   on `PATH`. Use the explicit examples directory and one-invocation trust:

   ```sh
   ./target/release/ygg --model MODEL_ID --no-context-files \
     --extension-dir ./examples/extensions \
     --enable-extension git-tools --trust-extension git-tools
   ```

   Inspect `/extensions status`, run `/checkpoint before-review`, and ask the
   agent to call `git_status` and distinguish staged, modified, and untracked
   paths. Verify its report against your deliberately prepared workspace state.
   Do not use `--safe-mode` here: it intentionally keeps extensions stopped.
3. Adapt a **copy** of the example for your domain. Keep the directory name,
   manifest name, declared tools/commands, and handler names consistent. Define
   bounded arguments, explicit error results, and a compact text result that
   includes the evidence needed for the next model turn. Keep stdout protocol-only;
   use SDK diagnostics on stderr. Add tests for invalid arguments, unavailable
   dependencies, and oversized results. The example checks captured output size
   after the subprocess returns; it is not a streaming memory-limit implementation.
4. Run the SDK's deterministic handshake/dispatch tests from the checkout, then
   add equivalent coverage for the adapted handler:

   ```sh
   PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=./sdk/python \
     python3 -m unittest discover -s sdk/python/tests -p test_extension.py
   ```

   The [existing tests](../sdk/python/tests/test_extension.py) exercise schema
   agreement, tool dispatch, malformed requests, and shutdown. They are protocol
   evidence, not a model-driven end-to-end pass for your new tool. Use
   `/extensions reload` after a compatible change and verify that the selected
   source actually reinitializes; discovery never implies execution.

For repeatable context shaping rather than a tool, adapt
[local-model-workflow](../examples/extensions/local-model-workflow/README.md).
Its [handler](../examples/extensions/local-model-workflow/extension.py) generates
short, deterministic labeled context from model, workspace, and active-skill
metadata; it does not read files or call a model. This is a practical place for
your repository's review checklist. It does not prove lower token use or better
local-model results without a matched evaluation.

**Language boundary:** Python is a convenience, not a host requirement. Any
language that can implement the [bounded JSON-RPC stdio contract](extensions.md)
can supply an executable extension. The example uses frozen API 0.1; metadata
retention, cancellation, and typed media do not magically become API 0.2/0.3
features. The Python `Extension` runtime supports 0.1/0.2, while generated 0.3
types alone are not a 0.3 Python runtime. Semantic renderer contributions are
protocol data; do not assume they appear in the current TUI.

**Evidence to retain:** adapted source/manifest, exact runtime and API versions,
protocol tests, selected-source diagnostics, an actual tool call/result, and a
reviewed domain outcome. An extension manifest is consent metadata, not a sandbox.

## 3. Embed a read-only repository assistant

**Outcome:** add repository analysis to an application written in your language
while Rust owns the provider loop and durable conversation. The application
owns its UI, workspace selection, credential handling, and cancellation.

The [native host contract](sdk.md) includes complete request/event examples.
The checked-in [cross-process fixture](../crates/ygg-coding-agent/tests/host_protocol.rs)
shows a real host client, bounded frame handling, an inline fake provider, and
session resume. It is the starting implementation evidence, not a live-provider
acceptance result.

1. Launch `./target/release/ygg-host` in a dedicated process group. Drain stdout
   as UTF-8 NDJSON and drain stderr independently with a retention cap. Send:

   ```json
   {"protocol_version":1,"request_id":"hello-1","command":"hello"}
   ```

   Accept work only after validating protocol version 1, the matching request
   ID, and the advertised features. Each frame is at most 1 MiB including its
   newline; the host runs one request at a time. Reject sequence gaps and
   mismatched request/run/session IDs. Unknown request fields are errors.
2. Choose a model via the `models` request or configure a vetted inline route
   as described in [SDK provider setup](sdk.md#inline-providers). Never log
   credential-bearing requests. With an available configured `MODEL_ID`, send
   this shape, substituting real absolute application-owned paths:

   ```json
   {"protocol_version":1,"request_id":"review-1","command":"run","run_id":"run-1","session_id":"review-session","workspace":"/srv/workspace","session_dir":"/srv/workspace/.host-sessions","model":"MODEL_ID","prompt":"Read README.md and identify the documented build and verification commands. Cite the file; do not run commands or edit files.","tools":["read"],"allow_file_mutation":false,"context_files":false,"offline":true}
   ```

3. Reduce `accepted`, `started`, streamed events, and `settled` until the
   terminal `final_result` or `protocol_error`. Do not mistake streamed text or
   `settled` alone for a successful response. Check `final_result.data.status`,
   retain the returned `sessionFile`, and display only bounded, appropriately
   sanitized content. For a later turn, use a new request/run ID and
   `resume_session` with that same regular session file inside `session_dir`.
4. On ordinary completion send `shutdown` and wait for its flushed response.
   On timeout or caller cancellation terminate the **entire dedicated process
   group** and drain it to exit. Protocol v1 has no in-band abort command.

The host fixes effect policy to Controlled: it denies headless mutation
approvals and does not start executable extensions. `allow_file_mutation: true`
is not an escape hatch to full access. For an application needing a different
host-owned policy or the Copilot integration seam, investigate the Rust
embedding contract rather than pretending the NDJSON host exposes those powers.

**Evidence to retain:** handshake/features, sanitized request metadata, ordered
terminal events, resumed-session continuity, cancellation/cleanup result, and a
reviewed answer grounded in the selected repository. Never publish raw secret
frames, session contents, or customer paths without review.

## From recipe to release evidence

For each live demonstration record the exact source revision/binary identity,
platform, model route/reasoning, extension manifest and API, permissions,
workspace inputs, reproduction commands, terminal outcome, and reviewed output.
Keep failure and cancellation cases. Add actual TUI/web captures only after a
branded candidate exists and privacy review completes. Unit fixtures and these
instructions do not establish an end-to-end run, comparative performance, Pi
parity, or release readiness; see the [draft gate list](releases/v0.7.0.md).
