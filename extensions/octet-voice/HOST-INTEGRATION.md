# Host integration

Target: an octet v0.10.0 candidate. The SDK snapshot and patches below were
prepared from octet base `47389ad1`. A later candidate may already include some
or all of those generic host changes; inspect its source before applying a patch.
This repository does not fork or replace octet's authentication, process
supervisor, UI protocol, or coding agent.

- The complete extension is at the repository root. `build-package.py` packages
  an explicit source allowlist, including the reviewed Python SDK under `vendor/`.
- The SDK snapshot has unreleased generic API 0.4 convenience surfaces for static
  shortcuts, credential resolution, input injection, request timeouts, and cached
  remote UI. See `SDK-PROVENANCE.txt` and `tests/sdk/`. The separately reviewed
  candidate-host SDK diff is available as `patches/sdk-api-0.4.patch` in this source
  repository; it applies to base `47389ad1` and is not included in the extension
  archive. No globally installed SDK is modified; the native host protocol is
  unchanged. Review and retest the diff against the host revision being integrated.
- `patches/quiet-shortcuts.patch` is an optional generic host patch. It removes
  the two unconditional shortcut-dispatch transcript notices, preserving actual
  results/errors. It is not applied or included in installed extension bundles.
  Review it against the matching octet source and run the host's shortcut tests
  before building your own host. Do not reapply it if the candidate already
  includes the change. The extension works without it.
- Codex credentials come from native `provider/credentials`, using the same
  native OAuth login and refresh as coding. No second sign-in or credential-file
  discovery is implemented. Public OpenAI Realtime uses the native OpenAI provider
  instead, only when explicitly selected.

## Experimental first-party v0.10.0 handoff

As checked on 2026-11-04, public octet PR #639 at head
`f51fe29aeac328ebf6047f4767dfeb5e575b5273` already contains the generic
Python SDK integration (`a17249b6`) and quiet shortcut dispatch (`cc660b7c`).
The host SDK files at that head match the SDK vendored by voice source commit
`7ad5e1cf011e93567452618cd817de5114c747cf`. The older patches must not be
reapplied to that head. These are host-side prerequisites; they do not by
themselves include voice in an octet release.

The provider-free workflow in this checkout tests Linux, macOS and Windows. It
also builds an exact public octet host source on Linux and exercises installation,
explicit enablement, status, feature negotiation and UI opening with audio absent.
By default it selects the PR head while PR #639 is open, and the v0.10.0 tag
once it is closed. Only after those jobs pass does it build a reproducible
source-only bundle and upload `octet-voice-0.10.0-experimental`. The artifact
contains the archive, `SHA256SUMS`, `SOURCE_COMMIT`, `OCTET_HOST_COMMIT` and
`OCTET_HOST_TARGET`. A manual run can explicitly select the PR (`candidate`),
the tag (`tag`) or the published stable release (`release`). The workflow
rechecks the selected host reference before packaging. It does not publish a
release or automatically rerun when the public PR moves. Changes to this workflow
must be committed and run before they constitute CI evidence.

The workflow artifact expires after 30 days; its visibility follows the repository
and Actions settings, and it is not durable release distribution. Obtain the
expected commit and digest through a trusted channel; a checksum in the same
artifact alone does not authenticate its origin. The manifest targets exactly
host version 0.10.0. Recheck the tested commit against the final host and test the
actual release binary rather than inferring its source from that version string.
See `RELEASE.md` in the source repository for the handoff gates; it is not included
in the source-only bundle.

For the source revisions above, private workflow run `41573640244` passed its
Linux, macOS and Windows provider-free jobs and produced an archive with SHA-256
`bcf1767a4c1821e8e622b3e988324b21db0a844c6575785ed17c46e0ee79efde`.
This is evidence for that earlier source commit, not for later documentation or
workflow changes. A clean local rebuild matched that archive byte for byte. On
Linux, an offline release build of the exact PR head installed it in an isolated home, discovered
and explicitly enabled it, and returned a stopped `/voice status`. A deliberately
missing audio prerequisite then stopped `/voice start` after feature negotiation
and remote UI opening, before credentials, devices or provider calls. A separate
idle PTY check on that host dispatched a generic SDK shortcut without the old
notice. These checks do not qualify live voice, other platforms, busy shortcut
dispatch, or a future final release artifact; repeat integration checks against
the actual release candidate before handoff.

Release workers do not automatically discover this separate repository: the
native host's compiled official catalog and release packaging consume the host
repository's `extensions/release-catalog.txt` and tracked source snapshots. The
v0.10.0 preparation adds a snapshot pinned to an exact voice commit; this
repository remains the source of truth. The release owner must verify the pin,
qualification, version-matched signed bundle and Packages-page listing before
publication. Preparing a catalog entry or CI artifact is not publication.

## Qualification boundaries

Provider-free tests cover protocol, UI, lifecycle, queues and platform selection.
Linux local Codex OAuth connection, mute, 50-second idle, resize and clean stop
have been exercised using the installed standalone bundle. An independent
107-second output-only synthetic-silence soak reported zero PipeWire underruns,
no playback error and at most 2880 queued bytes. These are not speech-fidelity
or multi-hour reliability qualifications. macOS and Windows backends have
fake-stream coverage only, not real-device qualification.
Neither provider-free tests nor a successful connection prove audible quality.
There is no acoustic echo cancellation, no recording, and no automatic reconnect
or ambiguous handoff replay. Native handoff acknowledgement is admission only.

This repository intentionally excludes native octet history, credentials, user
preferences, virtual environments, audio recordings, diagnostic transcripts and
workstation paths. Source publication is separate from releasing production binaries.
