# v0.7.4 local engineering evidence

See [methods, results and limits](../v0.7.4-performance.md). These are not signed
release-binary measurements or claims about live inference or terminal paint.

## Identity and reproduction

`manifest.json` records the base commit, measured source and executable hashes,
compiler, platform, and public artifact hashes. Apply `measured-source.patch` to
that base commit to reconstruct the three measured source files. Both original
and candidate algorithms run in the same executable. Later test-only additions
strengthen five-provider admission/auth/cache isolation; they are not silently
represented as the source of these original timing trials.

The patch and two benchmark logs are stored losslessly as text values in
`raw-text.json` (avoiding patch-context whitespace being mistaken for source
whitespace errors). Extract them into a scratch directory when reproducing:

```python
import json
from pathlib import Path
for name, text in json.loads(Path("raw-text.json").read_text()).items():
    Path(name).write_text(text)
```

The two benchmark logs retain every trial and successful test completion.
Use the Cargo commands in the linked report. Each cell contains nine independent,
alternating-order trials; suffix measurements divide each 200-call duration by
200. Fixture setup and correctness comparisons are outside timing. Cached and
offline cases, including their noisy regressions, are retained.

`startup.json` records all nine successful candidate-only PTY trials.
`startup.py` is the exact experimental driver, not an installed product command:

```sh
mkdir -p /tmp/octet-startup-evidence
python3 docs/benchmarks/v0.7.4-local/startup.py target/debug/octet \
  /tmp/octet-startup-evidence/new-results.json
```

Use a nonexistent output file. It creates disposable homes and a synthetic
manual loopback provider, inherits no credentials, submits no prompt, and
measures synchronized PTY output rather than emulator paint. Its `dev` profile
label describes the recorded run; use the matching build when reproducing it.

## Sanitation and exclusions

The absolute binary path and nonessential hostname/kernel host identity were
replaced with public placeholders. Numerical samples, source patch, benchmark
logs and driver bytes are unchanged. Public hashes were recomputed. No campaign
homes, credential registries, sessions or raw PTY transcripts are included.

Earlier release-profile benchmark attempts were interrupted before completing
all history sizes; they are excluded, not counted as successful trials. The
completed profiling-profile log supersedes them. One retained prototype PTY
trial timed out during shutdown because its driver stopped draining output;
the corrected driver continuously drains, and all nine final trials exited 0.
An earlier v0.7.3 development-binary run is not v0.7.4 evidence.

An earlier agent-library run had 496 passes, one failure and one ignored test:
`stdout_eof_does_not_bypass_search_timeout_or_cleanup` could not read its
`child.pid` fixture. Its isolated rerun passed; the final full run passed all
498 tests (one ignored benchmark). This is disclosed rather than treated as a
performance result or evidence that the timeout test can be waived.
