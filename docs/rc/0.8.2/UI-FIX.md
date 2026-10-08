# Local UI follow-up for the octet 0.8.2 RC

Baseline: `a78080e39d6e17a68afd879cff43850f33ed15b5`.
Follow-up source: `04875f49ae5e581730ebadae066c7d95e07fd694`.
The original Git index, binaries and frozen archives are preserved. `source.patch`
contains only this follow-up; `source.tar.gz` contains its exact complete source.

## Changes

- Completion shows only `N.N tok/s` when available and omits speed otherwise.
  Server timing still wins over the stream estimate; detailed provenance and
  rejection reasons remain in `/status` and telemetry. No E2E fallback.
- Native Tern welcome uses octet-owned roles and transparent, explicitly spaced
  layout nodes. The byte mark retains its 128×64 logical request, 2:1 silhouette,
  eight contiguous bars and existing model/theme-aware colours. This avoids
  Tern 0.3.1's OMP-only logo CSS (`52px !important` width / `45px !important` height).
- Native Bash, exec and local `!` cards keep the full command expanded. Output
  text and images are not mounted in non-verbose frames, including failures.
  Ctrl+O toggles the captured output; native card toggles cannot override this
  policy or collapse/truncate the command. Captured source remains unchanged.

## Verified for this source

- 48 Tern unit tests passed.
- Five completion/render/copy tests passed.
- Three real-binary synthetic-Tern PTY tests passed, including a long command,
  all emitted frames before disclosure, and both actual Ctrl+O transitions.
- Workspace formatting and patch whitespace passed.
- Optimized macOS ARM64 rebuild passed (stock release profile, both binaries).
- Version/help, protocol-1 host hello, wrapper syntax and binary hashes passed.

The original RC's `../run.sh` now launches these rebuilt binaries; restart octet
using that same wrapper to load the changes. `./run.sh` is a self-contained
wrapper for this follow-up. Both default cache warming off without changing
configuration. Original runtime binaries and archives are unchanged.

Actual Tern desktop pixels and live-provider measurements remain unverified.
The full workspace suite and strict Clippy were not rerun: the original RC's
seven test failures, 13 dead-code lint diagnostics and Codemode suite instability
remain release blockers (see `../VERIFICATION.md`). This is still FROZEN, unsigned,
local-only work, not a published or fully qualified release. The daily-driver
checkout was not edited; nothing was committed, merged, tagged or published.
