START 2026-09-15T17:42:55Z wiring9 alive
- 2026-09-15T17:42Z wiring9 alive: mapped the gap. Confirmed `resolve_launchable_child_session`
  lives in `pub mod delegation` (crates/octet-agent/src/lib.rs:74) so the CLI can call it without
  touching octet-agent. Confirmed `path_by_id` is the single choke point for `--resume <id>` in both
  print (bootstrap.rs:5739) and interactive (bootstrap.rs:5613) launches, plus cli/parity.rs:210.
  Delegation root is `<session-dir>/<workspace-key>/.delegation` (bootstrap.rs:6031).
