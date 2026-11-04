# octet-voice 0.10.0 experimental release handoff

Voice is an optional, **experimental first-party** extension, maintained in this
repository. The octet v0.10.0 owner decides whether and when to publish it. This
preparation does not publish a repository, tag, bundle or website, and does not
qualify live voice or cross-platform audio. Installation, dependency provisioning,
enablement and any provider-credential disclosure remain separate user decisions.

Octet's official installer currently obtains exact-version bundles from the
canonical octet release, using the catalog compiled into the host. Release
packaging uses tracked source in the host repository, not a live checkout of this
repository. The prepared integration therefore uses a reviewed snapshot pinned to
a full voice commit; the standalone repository remains the source of truth. The
Packages page must list voice only when that release includes the signed bundle.
Do not publish a listing that advertises an unavailable install command.

## Qualification and handoff

1. Review and commit the voice source. Run the `Provider-free tests` workflow from
   that exact commit; committing the workflow is not evidence that it ran. Its
   Linux, macOS and Windows jobs use locked dependencies and no devices or provider
   calls. With PR #639 open, select `host_target=candidate`; this resolves and builds
   its exact head on Linux. The run must be repeated when voice or host changes.
2. Verify that the octet release candidate pins the intended voice commit and that
   its tracked snapshot has the same source bytes and executable modes. Review the
   manifest, capabilities and source-only bundle allowlist. The candidate host must
   contain PR #639's generic SDK and quiet-shortcut changes; do not reapply the old
   patches. See `HOST-INTEGRATION.md` for the previously checked source identities.
3. Once the final `v0.10.0` tag exists, rerun the workflow from the intended voice
   commit with `host_target=tag`. After the stable public release exists, run with
   `host_target=release` to check that it is not a draft or prerelease. The workflow
   rechecks the selected reference before packaging, but any subsequent movement
   still requires a new run. A locally built host is not evidence for a distributed
   binary. `auto` selects the open PR head or the tag after it closes.
4. In the same successful workflow run check `SOURCE_COMMIT`, `OCTET_HOST_COMMIT`
   and `OCTET_HOST_TARGET` against the intended voice commit and final host source.
   Verify `SHA256SUMS` and compare the digest with a trusted authenticated handoff;
   a checksum stored beside an artifact is not a signature. The workflow artifact
   expires after 30 days, is not durable release distribution and is not publication.
5. Independently verify the official host and voice release assets using their
   release checksums, signatures and source-provenance checks. Rebuild the voice
   archive from the pinned source, compare its bytes with the actual signed voice
   bundle, and test that bundle with the verified octet release binary on Linux:

   ```sh
   python build-package.py /tmp/voice-release-stage \
     --archive /tmp/voice-release-check/octet-voice-0.10.0.tar.gz
   cmp /tmp/voice-release-check/octet-voice-0.10.0.tar.gz "$VERIFIED_VOICE_ARCHIVE"
   python scripts/host-smoke.py --host "$VERIFIED_OCTET_BINARY" \
     --venv "$HOME/.local/share/octet-voice/venv" --archive "$VERIFIED_VOICE_ARCHIVE"
   ```

   Staging and output paths must be new. The venv must already contain the locked
   dependencies. The smoke fixture deliberately has no PipeWire tools on its PATH
   and stops before credentials, audio devices or provider calls. It does not
   verify signatures, qualify live voice or test other platforms.
6. Before live audio or provider testing, obtain permission for microphone access,
   network/provider transmission and possible charges. Separately qualify capture,
   playback, interruption, mute/stop, delegation and failure cleanup on each
   claimed platform. Current limits and prior Linux observations are documented in
   `HOST-INTEGRATION.md`. Provider-free success is not live-device evidence.
7. The release owner coordinates octet's canonical bundle/signing workflow, the
   source repository's public availability, and the website's stable-release
   refresh. Confirm the experimental listing, standalone source link and inert
   `octet extension install octet-voice` route only after the matching signed asset
   exists. Verify the public installed journey and the Packages page after the
   authorized publication. Escalate unrelated release blockers separately.

The voice workflow has read-only permissions and never tags or publishes. Any
new publication automation needs a separate review of identity, immutable assets,
signing, scoped write permissions, approval, recovery and post-publication checks.
