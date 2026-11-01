# Native-mediated Pi settings helper

`SettingsManager.create(cwd, agentDir, options)` requires negotiated
`pi_settings_v1` and a live, authenticated native session owner. It is unavailable
during factory inspection or without a native settings binding. The supplied
workspace and agent directory must match the native configured roots; they never
authorize a caller-selected file.

The supported surface is `getGlobalSettings()`, `setExtensionPaths(paths)`,
`flush()` and `drainErrors()`. Global settings are cloned when returned.
Extension-selector updates are admitted by native policy, not arbitrary JSON
writes: existing selectors, their ordering and unrelated current disk fields
must be preserved. Unsupported additions, deletions or replacements fail
explicitly. The MCP onboarding use adds the builtin-disable selector
`-builtin:mcp`; this does not enable extensions or change native tool policy.

`projectTrusted: false` skips project settings entirely. Passing `true` cannot
elevate native workspace trust. No project-setting mutator, credential access,
native configuration write, custom storage backend or in-memory child-settings
override is provided by this facade.

`flush()` waits for the queued native commits. Native storage failures are
available through `drainErrors()` as scoped `Error` objects, matching Pi's error
queue rather than reporting uncommitted writes as successful. A global load
error prevents overwriting that file. Managers captured by background work retain
the original owner and cancellation signal; cancellation or owner retirement
refuses further effects. There is no adapter filesystem fallback.

The transport uses synchronous `pi/settings/read` and asynchronous
`pi/settings/update`. Settings reads are read-only cancellation operations;
late replies cannot revive cancelled reads. Native compilation and recorded
original-package journeys, not adapter fixture results alone, qualify the
complete integration.
