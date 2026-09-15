# Core journey verification on the audit baseline

Issues: #193, #253, #354, #379, #429. Base: `df5a7e809715961b9344af6b52e43a6ca48f56b3`
plus the uncommitted changes recorded in `docs/swarm-audit/EXECUTION-*.md`.
This is local candidate evidence, **not a named release, issue closure, or human
acceptance decision**. The shared working tree was being integrated during these
runs; repeat the commands on the final reviewed commit before release.

Environment: macOS 27.0, Apple silicon, Rust 1.97.1. Tests use disposable homes,
workspaces, sessions, and loopback fixtures, not live provider credentials.

## Observed checks

| Command (repository root) | Observed result | What it proves |
| --- | --- | --- |
| `cargo test --locked -p octet-coding-agent --test host_protocol --test setup_cli_acceptance --test setup_tui_acceptance` | Host protocol 5/5 and CLI setup 5/5 passed; the encompassing command was interrupted at the 120-second tool deadline during the TUI setup target. **Not a passing combined command.** | Process framing, controlled tool policy, ordered media projection, native-session reuse/reopen; explicit setup discovery, preview/cancel/offline/CAS and noninteractive errors. |
| `cargo test --locked -p octet-coding-agent --test slash_command_pty` | Passed, 2/2 | Real binary under a controlling PTY: one-Enter slash dispatch; `/model` picker selection and persisted startup preference without inference. |
| `python3 -m unittest discover -s examples/extensions/api-v03-minimal -p 'test_*.py'` | Passed, 5/5 | Executable example negotiation, tool call, rejection, cancellation and shutdown. Does not by itself prove Rust-host conformance. |

## Media and session evidence limits

`ordered_image_and_audio_cross_the_process_and_provider_boundaries` in
`crates/octet-coding-agent/tests/host_protocol.rs` sends two image files and a
WAV file through the real native host to a deterministic Chat Completions
listener. It asserts content order (`text`, `image_url`, `input_audio`,
`image_url`), WAV format, and base64 payload identity. The fixture uses synthetic
bytes: it proves typed transport, **not image/audio decoding quality or a live
model's understanding**. The [media recipe](../media.md) remains a user-input
recipe, not a recorded provider demonstration.

`inline_provider_run_streams_and_resumes_a_native_session` completes two native
host requests against a loopback fixture, reuses the session path, then reopens
the durable JSONL and checks metadata. This is native-host session evidence,
not a physical TUI `/resume` or process-restart inference journey.

The added model-picker test selects the local fixture model and checks the
saved preference in its disposable home. It does not discover an account's
models, authenticate, or verify availability at a hosted provider.

## Release decision remains separate

The repository `ROADMAP.md` selects core workflow qualification; the backlog
is not a release commitment. No installer was run, signed artifact retrieved,
remote issue closed, or commit created. Physical-terminal color/scrollback/focus,
unassisted fresh-user success, live media comprehension, and published-target
installation remain unrun. #354 requires review of this evidence together with
the final candidate's other gates; these deterministic checks are not authority
to close the epic.
