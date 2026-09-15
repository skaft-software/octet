# Bedrock wire URL repair

**Issue:** #246  
**Status:** source-only repair; verification intentionally unrun

## Defect

The ConverseStream fixture uses the Bedrock route

```text
/model/anthropic.claude-3-7-sonnet-20250219-v1%3A0/converse-stream
```

but `path_segments_mut().push(model.spec.api_name)` leaves the model ID's colon as
a literal path character. The fixture already records the required encoded path in
`crates/octet-ai/tests/bedrock_current.rs:218-223`.

## Repair

`crates/octet-ai/src/protocol/bedrock.rs` still builds the route with path segments,
then reparses the completed serialized path with `Url::set_path` after replacing
literal `:` characters with `%3A`. This uses the URL crate's path setter to preserve
the existing escape rather than producing `%253A`; endpoint path handling and the
request body are otherwise unchanged.

The existing signer in `crates/octet-ai/src/auth.rs:412-435` is already the correct
counterpart. It reads the URL's percent-encoded path, obtains decoded segments, and
AWS-encodes each segment once, so the prepared path and SigV4 canonical URI both
contain `%3A`.

## Preserved fixture contract

The fixture's existing assertions remain unchanged, including:

- exact serialized ConverseStream body;
- `x-amz-content-sha256` over those exact body bytes;
- AWS credential/region authorization checks and sensitive session-token handling;
- `cacheReadInputTokens`, `cacheWriteInputTokens`, and total usage;
- Event Stream response handling and the Bedrock accept header.

No signer, generic-client, bootstrap, catalog, or provider-auth change is needed.

## Evidence

- `crates/octet-ai/src/protocol/bedrock.rs` constructs `POST /model/<api-name>/converse-stream`.
- `crates/octet-ai/tests/bedrock_current.rs:183-250` captures the loopback request and
  checks the encoded path, body, hash, authorization, token sensitivity, accept header,
  and usage.
- `crates/octet-ai/src/auth.rs:1266-1336` checks encoded-path canonicalization,
  query ordering, exact body hashing, and signature sensitivity to body changes.

Tests, builds, formatting, and other commands were not run, as required for this
invocation.
