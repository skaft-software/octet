---
name: add-provider
description: Add or change an octet provider/model codec, catalog entry, and regression tests. Use for new endpoints, protocol capabilities, or model metadata.
version: 0.1.0
required-tools:
  - read
  - edit
  - write
  - bash
tags:
  - maintainer
  - providers
---
# Adding or changing a provider

Run repository commands from the repo root. `docs/providers.md` owns the
user-facing provider contract; this skill is the change checklist.

## 1. Decide the layer

- **Protocol/codec** (wire format, streaming, tool calls): `crates/octet-ai/src/protocol/`.
  Existing families: `anthropic.rs`, `bedrock.rs`, `google.rs`,
  `mistral_conversations.rs`, `openai_chat.rs`, `openai_responses.rs`, `sse.rs`.
- **Catalog/metadata** (model ids, limits, modalities, thinking): the checked-in
  model metadata consumed by `crates/octet-ai` — regenerate it from its
  generator, never hand-edit generated output.
- **Declarations** (endpoint, auth, headers): provider declarations, not the
  agent loop. Keep provider-specific behavior out of `octet-agent`.

## 2. Implement

- Reuse the canonical request/response types; do not fork them per provider.
- Treat every provider byte as untrusted, bounded input. Bound sizes, never
  panic on malformed frames, and preserve the existing retry/replay rules.
- Keep auth and credential handling inside the host-brokered policy; do not add a
  new credential store or bypass `docs/security` boundaries.
- Support the documented capability set the provider actually offers; do not
  claim discovery capabilities the snapshot does not provide.

## 3. Tests

- Add a protocol test next to the codec (fixtures + assertions), plus an
  integration test under `crates/octet-ai/tests/` when the family has one.
- Prove both success and rejection paths: malformed input, truncation, unknown
  fields, and a missing-finish case.
- Run the narrow commands, for example:

  ```sh
  cargo test -p octet-ai --test <family>_current --locked
  cargo check -p octet-ai -p octet-coding-agent --locked
  ```

## 4. Documentation and evidence

- Update `docs/providers.md` with the user-visible capability and any limits. Do
  not claim live-provider qualification from source-only checks.
- If provider compatibility artifacts are generated
  (`scripts/generate-pi-provider-compatibility.py`,
  `scripts/refresh-models-dev-pricing.py`), regenerate and review the diff.
- Add a `## [Unreleased]` CHANGELOG entry when the change is user-visible.
