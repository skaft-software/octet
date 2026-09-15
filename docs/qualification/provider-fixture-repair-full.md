# Provider fixture repair (full)

**Status:** source-only assertion repair; verification intentionally **UNRUN**.

## Scope

This qualification record covers only the owned integrated fixtures:

- `crates/octet-ai/tests/bedrock_current.rs`
- `crates/octet-ai/tests/google_current.rs`

No production code, provider wiring, catalog, or shared transport code was changed.

## Bedrock fixture contract

The Bedrock fixture continues to require the exact ConverseStream wire contract:

- the model route contains the encoded `%3A` separator;
- serialized request bytes remain exact, including the SHA-256 body hash;
- SigV4 authorization retains the fixture access key and `us-east-1/bedrock` scope;
- the session-token wire value and Amazon Event Stream accept header are checked;
- response usage retains input, cache-read, cache-write, output, and total counts.

Credential privacy is checked at the in-process signer boundary by the focused `auth.rs` unit test. That test inspects the actual `AwsSigV4Signer` output before transport and checks the session-token `HeaderValue` sensitivity bit, while the fixture checks the signer debug representation for redacted `Secret` values and no fixture credentials. The received Wiremock headers are treated as serialized bytes: they assert the exact token value, not `is_sensitive()`, because that in-process bit is not preserved by HTTP reconstruction. Existing stream framing, exception, cancellation, and missing-terminal assertions remain in place.

## Google streaming-error contract

The Google fixture still serves a deterministic loopback SSE response with HTTP 200,
`responseId: error-1`, and an `INVALID_ARGUMENT` provider error. It now requires the
client-facing `AiError::StreamFailure` wrapper and verifies the wrapped provider error's
code (`400`), status (`INVALID_ARGUMENT`), message, and response/request identifier.
It also preserves stream acceptance metadata: one provider event, a body observed,
zero decoded/content/buffered bytes for the rejected event, and coherent elapsed/last-
event timing.

The existing successful streaming usage/tool/signature assertions and the cancellation
boundary that drops a live response before a malformed trailing frame are unchanged.

## Verification record

No Cargo, rustc, test, rustfmt, build, or other verification command was run. Focused
Bedrock and Google tests, compilation, formatting, and workspace checks remain **UNRUN**
and must be performed only in the approved verification lane.
