# Roadmap
octet is a high-performance coding agent with a small, model-flexible host. Extensions add tools and bounded host-shaped integrations, not a general extension platform.
Young and rough: these are selected outcomes, not a promise to ship the whole backlog. The 0.8.0 build is a local RC, not a published release.
## Now
- Verify released installation, the TUI, `/model`, and resumable sessions ([#354](https://github.com/skaft-software/octet/issues/354)).
- Make audio/image attachments dependable on documented routes; reject unsupported input clearly ([#379](https://github.com/skaft-software/octet/issues/379)).
- Qualify reconnect recovery, responsive API waits, and stable Browse focus ([#350](https://github.com/skaft-software/octet/issues/350), [#346](https://github.com/skaft-software/octet/issues/346), [#377](https://github.com/skaft-software/octet/issues/377)).
- Qualify a bounded API 0.4 authoring smoke: discover/enable one local tool, negotiate, call it, cancel work, and shut down cleanly; retain protocol/SDK conformance ([#253](https://github.com/skaft-software/octet/issues/253)).
## Next
- Publish reproducible current-release speed/resource evidence; keep historical task-quality results and their limits intact ([#191](https://github.com/skaft-software/octet/issues/191)).
- Improve human maintainability through small, behavior-preserving changes ([maintainability plan](docs/design/maintainability.md)).
## Later
Further theme, companion, voice, and computer-use work needs separate selection. Existing Serve, Browse, MCP, web-search, and subagent integrations remain in scope with their documented limits.
## Not promised
Pi extension execution/parity, an everything-as-extension platform, computer-use parity, or benchmark superiority. Pi inventory/import and native provider support are separate from Pi extension execution. [Project 5](https://github.com/orgs/skaft-software/projects/5) is the engineering backlog, not a release commitment.
