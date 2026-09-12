# Current source reference

[Documentation](README.md) · [Getting started](getting-started.md) · [Security](../SECURITY.md)

The former combined reference is now a topic index. These source documents
are not release, channel, or live-provider qualification.
Existing fragment links below lead to the canonical guide or retained reference,
not a second user manual.

| Topic / old anchor | Canonical content |
| --- | --- |
| <a id="optional-packages"></a>Optional packages | [Package choices and availability](installation.md#optional-packages) |
| <a id="executable-extension-bundles"></a>Executable extension bundles | [Package CLI](cli.md#packages-and-serve), [activation menu](commands.md#extension-activation-menu), [extension contract](extensions.md) |
| <a id="graphical-serve-extension"></a>Graphical Serve extension | [Serve package commands](cli.md#packages-and-serve) and [graphical guide](experimental/octet-serve/README.md) |
| <a id="container"></a>Container | [Source container build](installation.md#container) |
| <a id="quick-start"></a>Quick start | [Provider setup](providers.md) |
| <a id="use-a-cloud-model"></a>Use a cloud model | [Cloud credentials and models](providers.md#cloud-setup), [Codex login](providers.md#codex-subscription-login) |
| <a id="use-custom-openai-compatible-providers"></a>Custom providers | [Local setup](providers.md#local-and-custom-endpoints) and [registry](providers.md#custom-registry) |
| <a id="thinking-controls"></a>Thinking controls | [Endpoint-specific choices and validation](provider-thinking.md) |
| <a id="cold-start-lifecycle-feedback"></a>Cold-start feedback | [Readiness negotiation and limits](providers.md#cold-start-feedback) |
| <a id="what-ships-in-the-binary"></a>What ships in the binary | [Frontends](terminal.md#choose-a-frontend) and [built-in tools](tools.md#built-in-tools) |
| <a id="three-frontends"></a>Three frontends | [TUI, plain, print](terminal.md#choose-a-frontend) |
| <a id="built-in-tools"></a>Built-in tools | [Tools and permissions](tools.md#built-in-tools) |
| <a id="provider-and-protocol-support"></a>Provider and protocol support | [Protocols, transport, and replay](providers.md#protocols-and-transport), [Astra limits](providers.md#astra-source-limits) |
| <a id="reasoning-without-transcript-noise"></a>Reasoning | [Model-supported choices](providers.md#reasoning), [display](terminal.md#reasoning-and-progress), [workers](../extensions/octet-subagents/README.md) |
| <a id="multimodal-prompts"></a>Multimodal prompts | [Images and audio](media.md#formats-and-limits) |
| <a id="durable-branchable-sessions"></a>Durable branchable sessions | [Resume and branch](sessions.md#resume-and-branch) |
| <a id="context-and-compaction"></a>Context and compaction | [Request budgeting](context.md#request-budgeting) |
| <a id="terminal-experience"></a>Terminal experience | [Scrolling and rendering](terminal.md#scrolling-and-rendering) |
| <a id="terminal-theming"></a>Terminal theming | [Default-only theme status](themes.md) |
| <a id="interactive-command-reference"></a>Interactive commands | [Slash commands](commands.md#slash-commands) and [keys](commands.md#keys) |
| <a id="configuration"></a>Configuration | [Precedence](configuration.md#precedence), [settings](configuration.md#settings), [environment](configuration.md#environment-variables) |
| <a id="cli-reference"></a>CLI reference | [Flags and subcommands](cli.md) |
| <a id="filesystem-native-customization"></a>Filesystem customization | [Discovery, precedence, and trust](resources.md#locations-and-precedence) |
| <a id="pi-migration-inventory"></a>Pi migration inventory | [Inventory](pi-migration.md#current-command), [explicit import/restore](pi-migration.md#import-portable-setup-data) |
| <a id="prompt-templates"></a>Prompt templates | [Templates and arguments](instructions.md#prompt-templates) |
| <a id="skills"></a>Skills | [Explicit activation and resources](instructions.md#skills) |
| <a id="executable-extensions"></a>Executable extensions | [Current authoring and legacy boundary](instructions.md#extension-authoring), [extensions](extensions.md) |
| <a id="self-documentation"></a>Self-documentation | [Source and packaged asset roots](instructions.md#self-documentation) |
| <a id="architecture"></a>Architecture | [Runtime responsibilities](design/octet-agent.md#responsibilities) and [product design](design/octet-coding-agent.md) |
| <a id="octet-ai"></a>`octet-ai` | [Canonical inference model](design/octet-ai.md#canonical-model) |
| <a id="octet-agent"></a>`octet-agent` | [Run-loop responsibilities](design/octet-agent.md#responsibilities) |
| <a id="octet-coding-agent"></a>`octet-coding-agent` | [Product boundaries](design/octet-coding-agent.md) |
| <a id="sexy-tui-rs"></a>`sexy-tui-rs` | [Renderer crate](../crates/sexy-tui-rs/README.md) and [TUI design](design/octet-tui.md) |
| <a id="reliability-and-security-engineering"></a>Reliability and security | [Recovery and security boundaries](tools.md#recovery-and-security) |
| <a id="development"></a>Development | [Required checks](../CONTRIBUTING.md#tests), [build profiles](build-profiles.md), [source build](installation.md#build-from-a-checkout) |
| <a id="repository-map"></a>Repository map | [Codebase](../README.md#codebase) and [crate contracts](../crates/octet-coding-agent/README.md) |
| <a id="documentation"></a>Documentation | [Topic navigation](README.md); [public roadmap](https://github.com/skaft-software/octet/blob/main/ROADMAP.md); [engineering backlog](https://github.com/orgs/skaft-software/projects/5) |
| <a id="historical-ygg-distribution-reference"></a>Historical Ygg distribution | [Historical installation only](reference/historical-installation.md) |
| <a id="historical-installer"></a>Historical installer | [Ygg v0.6.7 installer](reference/historical-installation.md#historical-installer) |
| <a id="npm-distribution"></a>Historical npm distribution | [Conditional historical npm channel](reference/historical-installation.md#npm-distribution) |
| <a id="homebrew-distribution"></a>Historical Homebrew distribution | [Historical macOS tap](reference/historical-installation.md#homebrew-distribution) |
| <a id="cargo"></a>Historical Cargo | [Pinned Ygg source installation](reference/historical-installation.md#cargo) |
| <a id="from-a-checkout"></a>Historical checkout | [Old Ygg checkout command](reference/historical-installation.md#from-a-checkout) |
| <a id="updating"></a>Historical updating | [Ygg channels and one-time hotfix](reference/historical-installation.md#updating) |
| <a id="historical-performance-summary"></a>Historical performance | [Frozen Ygg v0.6.2 results](benchmarks/tb21-v0.6.2/README.md#result), [audit](benchmarks/tb21-v0.6.2/README.md#integrity-audit), [bounded raw comparison](benchmarks/tb21-v0.6.2/README.md#published-codex-comparison) — not octet measurements or official placement |

## Maintainer paths

Beyond the [codebase map](../README.md#codebase): `fuzz/` holds the session-record
fuzz target, `deploy/` the non-root container build, `scripts/` the pinned installer,
and `third_party/` upstream license texts. `sdk/python/` is the dependency-free
legacy extension SDK; current authoring uses [API 0.3](extensions/API-0.3-REFERENCE.md).
