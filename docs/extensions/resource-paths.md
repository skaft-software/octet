# Resource path discovery

`resource_paths_v1` is an optional API 0.4 consumer for temporary filesystem
skill/prompt/theme roots. It is unrelated to opaque `resource_refs_v1` and
`get_applicable_operations`. API 0.3 generated contracts are unchanged.

The runtime option defaults to **false**. A product may enable it only when its
active-session lifecycle drives the request and publishes the real loader result.
Declaring `resources_discover` without negotiating the offered feature fails
initialization. It must not be enabled only to make a factory's registration pass.

The host invokes `hook/run` with `hook: "resources_discover"`, payload
`{cwd: <host workspace>, reason: "startup" | "reload"}`, and its current
owner-bound execution context. It must wait for `session_start`, including any
deferred interactive hook, and keep the frontend servicing reverse/UI requests.
This is not a factory/initialize/preflight hook. The dedicated native process
method pins one connection; the generic `run_hook` method refuses this hook.

Response (not the generic hook disposition envelope):

```json
{"resource_paths":{"skill_paths":["/reviewed/pkg/skills"],"prompt_paths":[],"theme_paths":[]}}
```

`resource_paths` is mandatory; omitted arrays mean empty. Unknown keys, nulls,
wrong types, relative paths, parent components and controls fail validation.
Paths are absolute UTF-8, at most 4096 bytes each, at most 64 entries / 64 KiB
across all three arrays before deduplication. No arbitrary JSON or objects cross
this boundary. The host request deadline is at most 5 seconds. A product pass
allows 16 contributing processes and retains at most 256 paths / 512 KiB,
in deterministic extension-name and returned-array order. Over-budget or failed
contributors produce diagnostics; an empty response is a genuine withdrawal.

Only an enabled, trusted, admitted extension process can contribute. The feature
is not an extension sandbox or additional shell/network authority. Local external
roots are authorized by that process's existing host-authority grant. Workspace
paths still need workspace trust or an existing explicit path grant. Root and
ancestor symlinks, missing inputs and special files are rejected. Parser reads
keep the existing bounded/no-follow semantics; admission-time filesystem checks
are not an OS confinement or transactional tree-snapshot guarantee.

The actual native skill, prompt and theme loaders consume accepted roots.
Contributed roots follow native defaults and precede user explicit roots. Native
later-name-wins diagnostics apply; Pi's first-skill-wins behavior is **not**
claimed. Skills use the existing 256 KiB entrypoint and bounded model catalog;
prompt and theme loaders keep their own input/expansion/schema bounds. A skill's
body is not injected merely because it was discovered. Prompt expansion uses the
new registry; theme selection parses a real theme document, not just its name.

This consumer accepts native Markdown skills, Markdown/TOML prompts, and TOML or
Pi JSON themes. JSON themes pass through the same bounded native loader and
precedence rules. Supported colors are hex, ANSI indices, terminal defaults,
variable references, and `oklch(...)`/`okhsl(...)` converted natively with the
pinned Pi 1.0.2 color implementation. No Node adapter or preprocessing is
required; see [Pi JSON themes](../themes.md#pi-json-themes) for the source pin
and rendering limits. Within one directory the existing lexical order makes a same-stem TOML
file win over JSON. An invalid higher-precedence winner must not resurrect a
lower-precedence theme. Native theme snapshots retain normalized TOML and the
original inspectable source path.

Build a complete replacement from the original user configuration. At an idle
boundary publish the skill registry, prompt registry and rendered skill catalog
together, update the Agent's system prompt, then apply the selected theme and
refresh shell suggestions. Never accumulate previously added roots. Keep base
paths outside the augmented config, remove obsolete roots on reload, and fence
owner/instance/generation again immediately before publication. Retirement must
withdraw stale contributions before another provider request or resource command.
Nothing is written to config, installation metadata or durable sessions.

Authorized App frontends now wire startup, reload, retirement, and atomic
publication. Native-host/preflight construction stays default-off; Mode or
factory registration alone never supplies the consumer capability. Retained
startup outcome records host-attempt completion, not remote execution settlement.
Actual-process and App tests cover these paths; this does not establish full Pi
or CLM compatibility, and an empty CLM reply cannot qualify nonempty loading.
