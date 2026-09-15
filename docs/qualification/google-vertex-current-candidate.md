# Google / Vertex current candidate

**Issues:** #244 (native Gemini) and #248 (Vertex inference/ADC)
**Recorded baseline:** `e2eef46b051360600a06e72dc1694b4924c09c7c`
**Candidate status:** source-only, uncommitted; compilation and execution are pending.

## Source candidate

The existing assigned implementation remains the production candidate:
`crates/octet-ai/src/protocol/google.rs` builds the native `generateContent`
SSE request, maps text/reasoning/tool calls/usages (including separate
thought-token output accounting)/errors, preserves bounded Google thought signatures,
merges cumulative function arguments, and stops at the terminal event. `crates/octet-coding-agent/src/providers/vertex.rs`
constructs only validated regional Vertex authorities and resolves authorized-
user/service-account ADC through the fixed Google token authority with bounded
responses and cached short-lived bearer tokens. No production rewrite or new
provider-name branch was needed.

The codec's colocated tests now cover structured request encoding, thought
signature metadata, reasoning/text separation, cumulative tool arguments,
usage/terminal assembly, and native provider errors. The Vertex tests cover the
fixed endpoint boundary, PKCS#8 requirement, offline authorized-user refresh
and cache, supported ADC parsing, project metadata, and rejection of unsupported
or empty credentials.

## Dedicated deterministic fixtures

- `crates/octet-ai/tests/google_current.rs` uses loopback Wiremock only. It
  checks the Gemini path and API-key header, system instruction, native tool
  declaration/selection, function-call output, thought signature, usage,
  structured provider error, and dropping `ResponseStream` as the cancellation
  boundary before a malformed trailing frame.
- `crates/octet-coding-agent/tests/vertex_current.rs` checks the public
  credential-free Google versus Vertex setup declarations, ADC/static catalog
  classification, and absence of URL/token/private-key material.

Unavailable ADC-backed models remain a bootstrap/runtime concern: the existing
resolver returns `None` when the implicit ADC file is absent and rejects unsafe
explicit paths; live credentials are not used by these fixtures. Tool-result
media remains intentionally lossy in the codec and is not silently claimed as
native support.

## Verification record

No Cargo, rustc, test, rustfmt, or other command was run in this source-only
coding root. No live Gemini/Vertex request, metadata service, credential file,
or installed binary was accessed. Formatting and compilation of both new test
files and the colocated modules are therefore **unrun**.

## Exact remaining gates / next step

1. On the assigned verification snapshot, run the focused `octet-ai` Google
   tests plus `cargo test -p octet-ai --test google_current`, and the focused
   Vertex module tests plus `cargo test -p octet-coding-agent --test
   vertex_current`, with the coordinator's approved locked/shared-target
   environment. Run rustfmt/checks only in the verification lane.
2. Inspect the integrated generated provider declarations and bootstrap picker
   with no credentials to confirm Vertex contributes no models without ADC;
   record the exact catalog result. Do not change shared registration files here.
3. If those checks pass, perform separately authorized loopback/live-provider
   qualification with disposable credentials only: Gemini and Vertex request,
   stream terminal/usage/error, cancellation, ADC refresh/expiry, and project /
   location endpoint cases. Physical/network and live gates are not claimed by
   this candidate.

The first bounded next step is the focused compile/test run above; any failure
should be repaired only in the owned files, preserving the shared provider
contract and credential privacy boundary.
