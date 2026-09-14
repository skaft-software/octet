# Windows distribution current candidate

Status: **source-only candidate; native Windows support and publication are not
established.** No build, package, installer, updater, test, signature, or
native Windows command has been run in this lane.

## Implemented boundary

- `scripts/package-octet-release.sh` retains its four-argument Unix interface
  and its existing version/`octet-host` probe. It accepts
  `x86_64-pc-windows-gnu` only with a fifth `WINDOWS_PROBE_JSON` argument and
  dispatches before the Unix executable probe.
- `scripts/package-octet-windows-release.py` accepts:

  ```text
  TARGET OUTPUT_DIRECTORY VERSION_TAG SOURCE_DIRECTORY WINDOWS_PROBE_JSON
  ```

  The only target is `x86_64-pc-windows-gnu`. It expects
  `target/x86_64-pc-windows-gnu/release/octet.exe` and
  `octet-host.exe`, rejects links/special files (including binary parent directories), checks the PE `MZ` marker, rejects Windows case-fold name collisions,
  copies the finite `docs/package-assets.txt` inventory, and writes
  `octet-VERSION-x86_64-pc-windows-gnu.tar.gz`.
- The archive uses POSIX member names, a versioned top-level directory, fixed
  uid/gid/modes/mtime, sorted entries, GNU tar, and an empty gzip filename. The
  script refuses an existing output archive and prints its SHA-256; the release
  workflow must still create the signed `OCTET_SHA256SUMS` manifest.
- The required probe record is
  `octet.windows.build-probe.v1`. It binds target, version, canonical
  repository, full source/workflow commits, exact immutable workflow ref,
  `observed_platform: windows-x86_64`, each executable SHA-256, the one-line
  `octet VERSION` stdout result, and one parsed native hello frame. The hello
  checks retain protocol `1`, request ID `release-probe`, sequence `1`, type
  `hello`, data protocol `1`, and matching `sdk_version`. The packager only
  reads this record and hashes files; it never launches either `.exe`.
- `scripts/generate-octet-release-metadata.py` keeps the default
  `PUBLISHED_TARGETS`/`TARGETS` tuple and exact three-archive manifest. The
  explicit `--include-windows-candidate` option adds only
  `x86_64-pc-windows-gnu`; it does not alter current workflow invocations or
  old-release verification. Candidate metadata still validates every local
  asset and checksum and uses immutable version URLs.
- `scripts/test-windows-release.py` contains synthetic, no-execution fixtures
  for the `.exe` pair, resources, probe/hash/link/name negatives, deterministic repeated
  archives, candidate metadata, immutable URLs, and byte-for-byte recomputed
  v0.7.0 metadata compatibility. It is written but **UNRUN**.

The package resource set follows the existing inventory contract used by the
native packager and build embedding. Product resources are not changed here.
Because this qualification note is a new `docs/` file and
`docs/package-assets.txt` is read-only in this lane, the coordinator/docs owner
must add its `text` entry before packaging any integrated commit; otherwise the
existing finite-inventory producer will correctly reject the tracked file.

## Native probe record interface

A native Windows build job must create the probe after running the exact checked
out files. Its JSON shape is strict at the outer level:

```json
{
  "schema": "octet.windows.build-probe.v1",
  "target": "x86_64-pc-windows-gnu",
  "version": "0.7.6",
  "repository": "skaft-software/octet",
  "source_commit": "<40 lowercase hex>",
  "workflow_commit": "<40 lowercase hex>",
  "workflow_ref": "skaft-software/octet/.github/workflows/release-octet.yml@refs/tags/octet-binaries-v0.7.6",
  "observed_platform": "windows-x86_64",
  "binaries": {
    "octet.exe": {
      "sha256": "<sha256 of the exact file>",
      "version_stdout": "octet 0.7.6\r\n"
    },
    "octet-host.exe": {
      "sha256": "<sha256 of the exact file>",
      "hello_stdout": "<one JSON hello frame plus its newline>"
    }
  }
}
```

The source commit must be the checked-out source commit. The workflow ref must
remain the exact `release-octet.yml@refs/tags/octet-binaries-vX.Y.Z` identity;
`release_repository` keeps the special old v0.7.0 identity only for its exact
historical source/workflow commit pair. No `latest` or release-API URL is valid.

## Exact follow-up interfaces (not implemented here)

### Native Windows CI

The release workflow owner should add a separately gated Windows job, without
removing the existing three Unix matrix entries:

1. Check out the canonical source tag/commit and record the exact workflow
   commit/ref. Provide a reviewed GNU Windows linker/CRT. The locally observed
   Zig binary and Rust standard library are tool availability only, not support
   evidence.
2. Build both bins with the locked source, for example
   `cargo build --release --locked --target x86_64-pc-windows-gnu -p
   octet-coding-agent --bins`.
3. On native Windows, run `octet.exe --version` and send the documented hello
   request to `octet-host.exe`; require exactly the captured version and one
   protocol-1 frame before writing the probe JSON above. Hash both files only
   after those checks.
4. Call the Python packager with the five positional arguments, run it twice
   into separate directories, and compare archive bytes and SHA-256. Inspect
   the archive for the `.exe` pair and all inventoried resources.
5. In the protected signing job, include the Windows archive in
   `OCTET_SHA256SUMS`, sign/verify it and the manifest with the existing
   `release-octet.yml` identity, then call metadata generation with
   `--include-windows-candidate` only after the Windows build/probe gate is
   approved. Existing Homebrew/npm consumers intentionally still reject the
   four-target candidate until their owners add an explicit contract.

A cross-compiled file on macOS/Linux, Wine/WSL, or a successful Rust target
install cannot replace step 3.

### PowerShell install

The installer owner should add a version-pinned PowerShell entry point with
explicit `-Version`/`-ReleaseTag` and `-InstallDirectory` inputs. It should
construct only:

```text
https://github.com/skaft-software/octet/releases/download/vX.Y.Z/octet-X.Y.Z-x86_64-pc-windows-gnu.tar.gz
```

and the matching `OCTET_SHA256SUMS`/signature assets. It must verify the
existing Sigstore identity and checksum manifest, validate safe POSIX archive
paths and the complete resource tree, require `octet.exe` and
`octet-host.exe`, run the version and native hello probes, then atomically
replace the two installed files. It must not invoke Cargo, use a mutable
`latest` URL, or treat cross-compilation as execution evidence.

### PowerShell/product updater

The updater owner should extend the existing install-method boundary with a
Windows target resolver returning `x86_64-pc-windows-gnu`, Windows names
`bin/octet.exe`/`bin/octet-host.exe` (or the chosen documented install root),
exact-version metadata/checksum verification, and the same version/hello probe
before commit. `update` and `update --check` must preserve the current
channel/version identity and old v0.7.0 metadata compatibility; they must not
silently consume the four-target candidate or query a mutable release alias.

## Proposed verification and remaining gates (all UNRUN)

- Run `python3 scripts/test-windows-release.py` and a Python syntax check through
  the non-Rust verifier; inspect the resulting archive fixture and negative
  cases.
- Run existing Unix package, metadata, Homebrew, npm, and installer checks to
  prove the default three-target and historical contracts are byte/behavior
  compatible.
- Through the sole verify-rust owner, perform the locked Windows cross-check
  with the workspace Rust floor, `x86_64-pc-windows-gnu` std, and a reviewed
  GNU linker/CRT. This lane owns no Rust changes.
- On a real Windows host, build and run both native executables, produce the
  probe, package twice, and verify version, host protocol, resource loading,
  archive extraction, and installation/update behavior. Check command/shell,
  authentication, provider, extension, and terminal behavior separately.
- In protected CI, verify Sigstore bundles, source/workflow identity, the
  four-target checksum manifest, and public immutable release downloads. No
  signing or publication occurred here.

Dependencies/handoffs: `verify-rust` for compiler/target/build evidence;
`verify-nonrust` for this offline fixture and existing packaging regressions;
the release-workflow owner for the separately gated Windows job and signing
matrix; installer/updater owners for PowerShell and Windows path contracts.
This candidate does not claim complete Windows distribution, authentication,
shell, update, installer, or physical terminal support.
