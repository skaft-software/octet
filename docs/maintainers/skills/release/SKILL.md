---
name: release
description: Prepare, package, publish, verify, and recover octet releases. Use for release preparation, local packaging checks, publishing, and failed release CI.
version: 0.1.0
required-tools:
  - read
  - bash
tags:
  - maintainer
  - release
---
# Releasing octet

Run repository commands from the repo root. This skill is a checklist over the
existing release tooling; read each script or workflow **before** running it.

Versioning: the workspace version in `Cargo.toml` is the single source of truth
(`Cargo.toml:13`). `patch` = fixes and additions, `minor` = breaking changes.
Extension manifests pin `requires_octet` separately
(`extensions/*/extension.toml`).

## 1. Changelog and release notes

- Run the [`/cl`](../../prompts/cl.md) audit so `CHANGELOG.md` covers every
  user-visible commit since the last tag.
- Add `docs/releases/vX.Y.Z.md` following the previous file in that directory.
- Bump the workspace version and any pinned `requires_octet` values.

## 2. Package the candidate

Read and run the packaging scripts with an explicit target and an isolated output
directory (never inside the repo root):

```sh
scripts/package-octet-release.sh TARGET /tmp/octet-release vX.Y.Z "$PWD"
```

Supporting generators: `scripts/generate-octet-release-metadata.py`,
`scripts/create-source-archive.py`, `scripts/package-octet-npm.sh`,
`scripts/generate-homebrew-formula.py`, `scripts/package-octet-serve-release.sh`.

## 3. Local smoke test

From outside the checkout (for example `/tmp`), run the packaged binary and check
startup, `--version`, `--help`, `--list-models`, and one real prompt with the
intended default provider. Use the
[interactive-testing skill](../interactive-testing/SKILL.md) for the TUI steps.
Startup alone is not a passing smoke test.

```sh
cd /tmp && /tmp/octet-release/bin/octet --version
```

## 4. Publish

- Read the `.github/workflows/release-octet.yml` in full first. It triggers on
  `octet-binaries-v*` / `octet-installer-v*` tags (or `workflow_dispatch` with an
  existing canonical stable tag) and resolves the canonical tag, version, and
  source commit.
- npm publication uses trusted publishing
  ([docs/release/npm-trusted-publishing.md](../../../release/npm-trusted-publishing.md));
  there is no local `npm publish` step.
- Never move or re-push an existing release tag.

## 5. Verify

- Confirm the release artifacts, checksums (`SHA256SUMS`), and the installer
  resolve at the exact version. `scripts/test-installer-version.py`,
  `scripts/test-packaged-docs.py`, and `scripts/test_homebrew_formula.sh` cover
  parts of this.
- Record the observed result. Do not describe an unrun check as qualified.

## 6. Recover a failed release

Inspect the failed job before acting. Fix the cause, then re-run only the failed
job. Do not re-run a version bump or re-tag; packaging and download steps are
idempotent where the artifact already exists.
