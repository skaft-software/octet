# Typed Python SDK host smoke

The additive `Extension.typed_tool` recipe is in the [SDK guide](../../../sdk/python/README.md#typed-tools-and-diagnostics-api-04).
The original single-file `extension.py`, explicit dictionary tool API, package
generator and existing host-smoke executable are unchanged.

The real executable fixture is `sdk/python/tests/typed_fixture.py`. It uses the
adjacent source SDK without installing anything and writes a PID-bearing append-only
call log supplied as its sole argument. Its six tools are `typed_roundtrip`,
`invalid_output`, `typed_wait`, `typed_progress`, `typed_diagnostics`, and
`malformed_diagnostics`. All typed inputs use the shared record fixture in
`sdk/conformance/typed-values-v1.json`. Waiting exposes an `entered` barrier before
cooperative cancellation and a `cancelled` barrier afterwards. Diagnostics use a
real private workspace file and its computed revision.

From the checkout, run the SDK unit and raw subprocess checks:

```sh
PYTHONDONTWRITEBYTECODE=1 PYTHONPATH=sdk/python \
  python3 -m unittest discover -s sdk/python/tests -v
```

Production-host acceptance is separate. Reuse an existing compatible Cargo cache
through `CARGO_TARGET_DIR` rather than creating a second large host build:

```sh
CARGO_BUILD_JOBS=1 cargo test --offline --locked \
  --manifest-path examples/extensions/python-single-file/host-smoke/Cargo.toml \
  --test typed -- --nocapture --test-threads=2
```

This executes eighteen named actual `ExtensionProcess` tests, each launching a
source Python fixture with a private workspace and child HOME. Seven direct `a*` methods check typed
roundtrip, shared invalid inputs with zero handler entries, invalid output,
default/null/present optional values, retained diagnostics and malformed-profile
refusal, barrier-controlled cancellation with generation supervision disabled,
and negotiated progress absent from final results. Child call logs are printed
before cleanup. Raw subprocess tests additionally assert exact progress sequence,
unnegotiated refusal, malformed/oversized frames, EOF and a single cancellation
terminal. They do not substitute for the production host tests.

The additional `progress_capture::a07_progress_excluded_from_model_transcript`
method launches `sdk/python/tests/progress_fixture.py` through the real host,
registers it in a real `Agent`, and serves two deterministic Anthropic HTTP/SSE
responses on loopback. It captures both actual model requests, observes the exact
two Agent status events, and checks that the successful typed BlobRef and explicit
summary survive while progress, payload and private transfer locators stay out of
the model transcript. A bounded capture is printed; optionally set
`OCTET_PYTHON_VALUES_EVIDENCE_DIR` to retain `a07-model-capture.json`. No external
provider or inference is used. The fixture's raw subprocess checks remain
supplementary, not substitutes for that Agent test.

Six `r*` methods launch `sdk/python/tests/resource_fixture.py` and check generated
resource schemas/operations, same-native-state roundtrips, release/rejected reuse,
wrong owner/type refusal, busy release, caller cancellation before actual native
execution settles, invalid/error parent retirement, and failed disposal remaining
invalid. The `allow_terminal` file is an explicit native execution barrier; no
sleep guesses decide when release or cancellation should win.

Four `b*` methods launch `sdk/python/tests/bulk_fixture.py` with host-configured
`BulkStorage`. A 512 KiB binary payload remains in bounded local-file transfers;
results contain only closed BlobRef metadata. These methods check publication /
verified reread, wrong owner or integrity metadata, invalid/error parent refusal,
cancellation after provisional commit, and cancellation of a reader without
revoking the session's retained blob. Unit and raw subprocess checks also cover
path traversal/symlink refusal, finite caller bounds, parser failures and ticket /
lease cleanup; they are not production storage/admission evidence.

No external provider/network, install, global configuration, or simulator is needed;
the Agent capture uses only a private local scripted HTTP server.
Unsupported secure local-file platforms fail closed; no platform matrix is
implied. Test names define coverage, not a claim that an unrun suite passed.
