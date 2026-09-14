# Bedrock current candidate

**Issue:** #246
**Recorded baseline:** `e2eef46b051360600a06e72dc1694b4924c09c7c`
**Candidate commit:** pending; source-only, uncommitted

## Candidate scope

The existing Bedrock Converse codec remains the provider boundary. This candidate adds
focused qualification rather than changing the generic client or bootstrap:

- SigV4 canonical URI handling re-encodes decoded URL path segments once, so a Bedrock
  model ID such as `...v1:0` is signed as the exact `%3A` path bytes sent on the wire.
  Canonical query ordering and the exact prepared body hash remain covered.
- Bedrock usage now retains `cacheReadInputTokens` and `cacheWriteInputTokens` while
  preserving the existing total-token underflow guard.
- The private AWS chain is explicit and bounded: environment pair, static profile,
  then allowlisted ECS/EC2 metadata. The request signer invokes the resolver per
  request, so rotating task/instance credentials are not cached in the catalog.
  Profile `credential_process` is parsed only as inert data and is never executed.
  Metadata credential fields reject control bytes; metadata transport already has a
  one-second bound, no proxy, and no redirects.
- A complete body without `messageStop` is rejected rather than ending without a
  terminal response, matching the pinned Bedrock stream contract.
- Dedicated fixtures cover stream framing, both CRCs, exception frames, usage,
  exact signed request bytes, stream cancellation, missing terminal framing,
  credential-free provider setup, precedence/refresh, and metadata URL restrictions.

## Actual API/source evidence

- `crates/octet-ai/src/protocol/bedrock.rs` builds `POST /model/<api-name>/converse-stream`,
  uses AWS Event Stream frames, checks prelude/message CRCs before JSON, rejects EOF
  without `messageStop`, and emits the canonical `Started`/part/usage/`Finished` contract.
- `crates/octet-ai/src/client.rs:1091-1114` prepares the final body before invoking a
  request-aware signer; `:1235-1246` selects the Bedrock decoder; `:897-1057` stops
  after terminal output and retains cancellable body bounds.
- `crates/octet-coding-agent/src/app/bootstrap.rs:2741-2758` already wires the
  regional auth/base URL/static catalog path. `declarations.json:955-993` already
  declares the `bedrock_converse`/`aws_sigv4` route. These shared files were not edited.
- `docs/providers.md:44-53`, `docs/design/octet-ai.md:141-143`, and
  `docs/provider-thinking.md:65-76` are the applicable checked-in contracts.
- The exact pinned Pi 0.84.4 source was checked read-only at commit
  `b79e4cc834970cca69daebffab7df1da7d1e52c4` (MIT; archive SHA-256
  `19f5fc67c69cb75aea1584b57e156bfd16116cac45fac647f8173ec956e6f9b3`). Its
  Bedrock stream signs the serialized Converse command, retains cache read/write
  counters, and rejects a body ending without a stop reason. The distinct Pi 0.85.1
  checkout was not used as the pinned target. No comparator source was copied or
  modified.

## Written artifacts

- `crates/octet-ai/src/auth.rs`
- `crates/octet-ai/src/protocol/bedrock.rs`
- `crates/octet-coding-agent/src/providers/auth.rs`
- `crates/octet-ai/tests/bedrock_current.rs`
- `crates/octet-coding-agent/tests/bedrock_auth_current.rs`
- this qualification record

All fixtures use local synthetic bytes/loopback servers. They do not read real AWS
metadata or credentials and do not execute credential-process commands.

## Proposed checks (UNRUN)

- Focused `octet-ai` Bedrock unit tests plus `bedrock_current` integration tests:
  request/body signing, encoded model path, fragmented Event Stream input, prelude
  and message CRC rejection, exception/error provenance, cache usage, missing-terminal
  rejection, and dropping a live local response body.
- Focused `octet-coding-agent` provider-auth unit tests plus `bedrock_auth_current`:
  environment/profile/metadata precedence, refresh-on-sign, inert credential-process,
  allowlisted metadata URL forms, control-byte rejection, public route declaration,
  and secret redaction.
- Rust formatting, `--locked` compilation, full workspace tests, and diff checks.
  Coding roots were not allowed to execute any of these in this invocation.

## Dependencies and gates

No source wiring transfer is currently required. The verify-rust owner must admit the
five owned source/test paths together; any shared-client or bootstrap/catalog change
would be a separate typed-interface request, not an edit in this candidate.

Still unqualified: no live Bedrock account/model/region acceptance, no real AWS
credential refresh, no metadata-service observation, no installed picker/discovery
run, and no physical-network cancellation measurement. Local stream drop proves only
local HTTP-body cancellation; it does not prove remote generation cancellation or
zero billing. The candidate also does not claim availability for models absent from
the existing authenticated/static catalog.
