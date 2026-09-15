# LEDGER rows-audits — Phase 3 acceptance audits 3a/3d/3e + orphan (9 issues)

Format: `#NNN | verdict | evidence | missing`
Policy: `cargo check --workspace --all-targets` green and the FAILURES.md failure list
(mistral_current 2/16, pi.rs 1/1249) are taken as given; this worker ran no test.
Verdicts name the target that would prove each item.

#244 | implemented | crates/octet-ai/src/protocol/google.rs:1 (Google Generative AI/Vertex generateContent, function-call, thought-signature and SSE codec), crates/octet-coding-agent/src/providers/declarations.json:592 (GEMINI env auth + google static inventory + x-goog-api-key discovery header), test targets crates/octet-ai/tests/google_current.rs:98 and crates/octet-coding-agent/tests/vertex_current.rs:6 | —
#246 | implemented | crates/octet-ai/src/protocol/bedrock.rs:1 (Converse/ConverseStream codec with incremental CRC-checked AWS Event Stream decoder), crates/octet-coding-agent/src/providers/auth.rs:245 (credentials: env -> profile -> IMDS; unit test auth.rs:695 ordered without metadata fallback), test targets crates/octet-ai/tests/bedrock_current.rs and crates/octet-coding-agent/tests/bedrock_auth_current.rs | —
#274 | implemented | crates/octet-coding-agent/src/modes/interactive.rs:4831 (guided first-run provider setup with LM Studio local preset), crates/octet-coding-agent/src/provider_setup.rs:736, test target crates/octet-coding-agent/tests/setup_tui_acceptance.rs:656 (real-binary VT100 first-run local-provider journey, asserts LM Studio/OpenAI-compatible picker) | —
#275 | implemented | crates/octet-coding-agent/src/cli.rs:29 (deterministic SetupCommand: --preset/--endpoint/--model/--manual-model), crates/octet-coding-agent/src/cli.rs:125 (`octet setup --yes`, review-only default), test target crates/octet-coding-agent/tests/setup_cli_acceptance.rs:668 (print/rpc unresolved startup is actionable and non-interactive) | —
#282 | implemented | crates/octet-coding-agent/src/tui/pickers.rs:1308 (model picker title/purpose via OrdinarySurfaceMetadata), crates/octet-coding-agent/src/tui/pickers.rs:671 (resume session picker), lib test crates/octet-coding-agent/src/tui/view/ordinary_surface_contract_tests.rs:422 `ordinary_surface_contract_migrates_resume_extensions_and_inline_completion` | —
#283 | implemented | crates/octet-coding-agent/src/tui/view.rs:4566 (show_report_text shared title/purpose/status/action chrome), crates/octet-coding-agent/src/modes/interactive.rs:3712 (help/context/cost/cache dispatch; status at view.rs:4655), lib test crates/octet-coding-agent/src/tui/view/ordinary_surface_contract_tests.rs:473 `ordinary_report_contract_keeps_navigation_and_lifecycle_semantic` | —
#112 | implemented | Cargo.toml:44 (`ci-test`: limited debug, no incremental), Cargo.toml:51 (`profiling`: debug=full, lto=off, strip=none), docs/build-profiles.md:26; measurement-retention contract target scripts/tests/test_bench_systems.py:162 (retained repetitions/raw samples), wired at .github/workflows/ci.yml:94 | —
#173 | implemented | crates/octet-ai/src/stream.rs:18 (queued/loading/ready state), crates/octet-ai/src/client.rs:1084 (opt-in x-octet-lifecycle negotiation + namespaced SSE-comment parse), test targets crates/octet-ai/tests/client_stream.rs:128 and crates/octet-agent/tests/agent_run.rs (openai_lifecycle_feedback_is_forwarded_but_not_persisted) | feedback is endpoint-opt-in: stock llama.cpp/vLLM must emit x-octet-lifecycle / `: octet-lifecycle:`; no automatic cold-start probe and no 503 handling (docs/providers.md:266)
#430 | implemented | docs/instructions.md:66 ("The model has no skill-specific search, load, or resource tool"), docs/design/octet-coding-agent.md:267 ("No model-facing skill-specific tools are registered"); docs/tools.md carries no stale names | —

NOTES:
- No test was run by this worker; `cargo check --workspace --all-targets` green and the FAILURES.md failure list are taken as given.
- All 9 rows are `implemented`; none `partial`/`absent`. Every cited file was read this run and the line numbers are current.
- #173 is the only row with a scope caveat: the feedback channel requires the endpoint to opt in, so a stock llama.cpp/vLLM server needs a wrapper that emits the negotiated header/comment.
- Remaining work in this bundle is acceptance-evidence recording (Phase 3 audits); #430's doc fix has already landed in the tree.
