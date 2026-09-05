# octet documentation

[Project](../README.md) · [Workflows](workflows.md) · [Examples](../examples/README.md)

This repository is the canonical product documentation. Start with an outcome,
then follow its reference contract and checked-in example. No external website
is needed to configure, extend, embed, or investigate the source.

**Version boundary:** this checkout defines octet 0.7.0 source identities:
`octet`, `octet-host`, `octet-*`, `OCTET_*`, and `.octet`. This is an unpublished
source version, not proof of a qualified build or public installation channel.
No Ygg command aliases, old-root readers, or automatic first-party migration are
provided. Historical releases and benchmarks retain their original identities.
The [0.7.0 notes](releases/v0.7.0.md) distinguish implementation from remaining gates.

## Choose a task

| I want to… | Guide | Reference / source |
| --- | --- | --- |
| Start from sound and visual references | [Audio-led brief](workflows.md#start-with-an-audio-reference) | [Formats, limits, consent, and privacy](current-reference.md#multimodal-prompts); native WAV/MP3 on a compatible OpenAI Chat route, not web audio |
| Build and run the checkout | [Local build](../README.md#build-this-checkout) | [Contribution checks](../CONTRIBUTING.md#tests), [build profiles](build-profiles.md) |
| Fix a defect and deliver evidence | [Repository change workflow](workflows.md#1-make-a-reviewable-repository-change) | [Prompt examples](../examples/prompts/README.md), [sessions](sessions.md) |
| Delegate independent investigations | [Read-only delegation](workflows.md#delegate-independent-investigations) | [Subagents package](../extensions/octet-subagents/README.md), [ownership boundaries](design/extension-capability-and-orchestration-boundaries.md) |
| Build a domain tool or workflow hook | [Extension workflow](workflows.md#2-build-and-use-a-domain-extension) | [Examples](../examples/README.md), [extension contract](extensions.md), [Python SDK](../sdk/python/README.md) |
| Embed the agent in an application | [Native host workflow](workflows.md#3-embed-a-read-only-repository-assistant) | [SDK and NDJSON contract](sdk.md) |
| Use a cloud or local model | [Provider setup and credentials](current-reference.md#quick-start) | [Provider compatibility](pi-provider-compatibility.md) |
| Change instructions, prompts, or skills | [Customization reference](current-reference.md#filesystem-native-customization) | [Discovery, trust, and reload](resources.md) |
| Resume, fork, export, or repair a session | [Sessions](sessions.md) | [Session command examples](current-reference.md#durable-branchable-sessions) |
| Understand authority before running tools | [Security policy](../SECURITY.md) | [Tool policy](current-reference.md#built-in-tools) |
| Inventory existing Pi resources | [Pi migration](pi-migration.md) | [Migration skill example](../examples/skills/pi-migration/SKILL.md); inventory and bounded compatibility, not full parity |
| Use the optional graphical interface | [Serve reference](current-reference.md#graphical-serve-extension) | [Serve docs](experimental/octet-serve/README.md); separately packaged and version matched |
| Look up a flag or slash command | [CLI reference](current-reference.md#cli-reference), [interactive commands](current-reference.md#interactive-command-reference) | The local source binary's `--help` |

## Contracts for builders

- [Extension protocol API 0.1 / 0.2](extensions/PROTOCOL-REFERENCE.md) and
  [generated API 0.3 reference](extensions/API-0.3-REFERENCE.md). Select the exact
  wire your implementation supports; the Python runtime is an explicit 0.1/0.2
  adapter, not an implicit 0.3 runtime.
- [Inference layer](design/octet-ai.md), [agent runtime](design/octet-agent.md),
  [product bootstrap](design/octet-coding-agent.md), and
  [terminal renderer](design/octet-tui.md).
- [Presentation](design/octet-presentation.md),
  [command and picker surfaces](design/octet-command-picker-surfaces.md), and
  [theme status](themes.md).
- [Complete current source reference](current-reference.md): the former root
  README's substantive provider, tool, session, configuration, and architecture
  material, with historical installation guidance explicitly separated.

## Project, identity, and evidence

- [Canonical identity assets](assets/octet/README.md): source marks and wordmark,
  not screenshots of an actual candidate.
- [Draft 0.7.0 notes](releases/v0.7.0.md), [historical 0.6.7 notes](releases/v0.6.7.md),
  [changelog](../CHANGELOG.md), [roadmap](../ROADMAP.md), and
  [contributing](../CONTRIBUTING.md).
- [Distribution contract](distribution.md) and [npm release workflow](release/npm-trusted-publishing.md):
  renamed source machinery, not proof that an octet package is published.
- [Measurement methodology](benchmarks/README.md) and
  [historical Ygg v0.6.2 Terminal-Bench evidence](benchmarks/tb21-v0.6.2/README.md).
  Historical campaigns, local audits, synthetic fixtures, and live candidate
  evidence are distinct; none substitutes for the others.
