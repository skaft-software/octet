# Pi usability pass: work-in-progress patches (unreviewed)

These are safety copies of workers' local branches, taken when a usage limit stopped them.
They are not merged and not reviewed. Each workstream has:

- `<K>-commits/`: `git format-patch 852ca760..<branch>`; apply with `git am`.
- `<K>-uncommitted.patch`: work not yet committed on top of those commits; apply with
  `git apply`.

| K | Branch | Scope |
|---|---|---|
| J | eval/pi-jank | One-off I/O diff against Pi 1.4.0 to find every cause of jank |
| F | fix/pi-footer-r2 | Footer instant, consistent, Pi-exact |
| P | fix/pi-before-prompt-r2 | before_agent_start / prompt composition parity |
| S | perf/pi-warm-exit-r2 | Warm startup and exit time |
| R | review/pi-compat-r2 | Defect review of c610a671..852ca760 |
| D | fix/pi-diagnostics-ux-r2 | Concise extension diagnostics presentation |
| M | fix/pi-model-precedence-r2 | Saved octet model/reasoning beat Pi defaults |

Scope and rules: `docs/maintainers/pi-usability-brief.md`. Delete this directory once the
work has landed.
