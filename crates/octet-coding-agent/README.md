# octet-coding-agent

The `octet` terminal coding agent. It supports interactive, chronological plain,
and response-only print modes; local OpenAI-compatible endpoints; major cloud
providers; branchable persistent sessions; bounded tools; context compaction;
and explicit workspace trust/tool policies.

The customization layer is deliberately local and inspectable: drop prompt
templates, skills, or executable extensions into a project `.octet/` directory,
then inspect or reload them without rebuilding the binary. See the
[resource contract](../../docs/resources.md), [Pi migration](../../docs/pi-migration.md),
[extension API](../../docs/extensions.md),
[theme status](../../docs/themes.md), [session tools](../../docs/sessions.md), and
[examples](../../examples/README.md).

See the [workspace README](https://github.com/skaft-software/ygg#readme) for
installation, provider setup, safety defaults, and release status.

## Inline tool images

Inline tool-result images are off by default. Opt in with `--show-images`,
`OCTET_SHOW_IMAGES=1`, or `show_images = true` in user configuration. octet only
places validated inline payloads on Kitty terminals; unsupported terminals use
text fallbacks. URLs and paths are never loaded for terminal display, and
copy/plain/print output plus terminal-write logs remain payload-free.

## SDK and native host

Rust consumers can embed the public `octet-agent` and `octet-ai` crates directly.
The product crate also builds as the `octet_sdk` library, keeping the runtime used
by `octet` and `octet-host` in one implementation.

Non-Rust applications should run `octet-host`, a versioned NDJSON process
boundary with request correlation, monotonic event sequences, bounded frames,
durable sessions, ordered typed image/audio input, and explicit process-group cancellation.
Install both entry points with:

```console
cargo install --locked --path crates/octet-coding-agent --bins
```

See the [native SDK host protocol](../../docs/sdk.md) for the integration
contract.
