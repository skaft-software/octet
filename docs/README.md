# octet documentation

[Project](../README.md) · [Workflows](workflows.md) · [Examples](../examples/README.md)

This repository is the canonical product documentation. Start with an outcome,
then follow its reference contract and checked-in example. No external website
is needed to configure, extend, embed, or investigate the source.

**Version boundary:** octet is the intended 0.7.0 identity. In this pre-rename checkout the
source still uses `ygg`, `ygg-host`, `ygg-*`, `YGG_*`, and `.ygg`. Existing
technical documents retain those spellings intentionally; they do not establish
that renamed commands, packages, or data roots work. Historical release and
benchmark documents retain their measured/shipped version. The
[0.7.0 draft](releases/v0.7.0.md) is not a release-readiness report.

## Choose a task

| I want to… | Guide | Reference / source |
| --- | --- | --- |
| Build and run the checkout | [Local build](../README.md#build-this-checkout) | [Contribution checks](../CONTRIBUTING.md#tests), [build profiles](build-profiles.md) |
| Fix a defect and deliver evidence | [Repository change workflow](workflows.md#1-make-a-reviewable-repository-change) | [Prompt examples](../examples/prompts/README.md), [sessions](sessions.md) |
| Delegate independent investigations | [Read-only delegation](workflows.md#delegate-independent-investigations) | [Subagents package](../extensions/ygg-subagents/README.md), [ownership boundaries](design/extension-capability-and-orchestration-boundaries.md) |
| Build a domain tool or workflow hook | [Extension workflow](workflows.md#2-build-and-use-a-domain-extension) | [Examples](../examples/README.md), [extension contract](extensions.md), [Python SDK](../sdk/python/README.md) |
| Embed the agent in an application | [Native host workflow](workflows.md#3-embed-a-read-only-repository-assistant) | [SDK and NDJSON contract](sdk.md) |
| Use a cloud or local model | [Provider setup and credentials](current-reference.md#quick-start) | [Provider compatibility](pi-provider-compatibility.md) |
| Change instructions, prompts, or skills | [Customization reference](current-reference.md#filesystem-native-customization) | [Discovery, trust, and reload](resources.md) |
| Resume, fork, export, or repair a session | [Sessions](sessions.md) | [Session command examples](current-reference.md#durable-branchable-sessions) |
| Understand authority before running tools | [Security policy](../SECURITY.md) | [Tool policy](current-reference.md#built-in-tools) |
| Inventory existing Pi resources | [Pi migration](pi-migration.md) | [Migration skill example](../examples/skills/pi-migration/SKILL.md); inventory and bounded compatibility, not full parity |
| Use the optional graphical interface | [Serve reference](current-reference.md#graphical-serve-extension) | [Serve docs](experimental/ygg-serve/README.md); separately packaged and version matched |
| Look up a flag or slash command | [CLI reference](current-reference.md#cli-reference), [interactive commands](current-reference.md#interactive-command-reference) | The local source binary's `--help` |

## Contracts for builders

- [Extension protocol API 0.1 / 0.2](extensions/PROTOCOL-REFERENCE.md) and
  [generated API 0.3 reference](extensions/API-0.3-REFERENCE.md). Select the exact
  wire your implementation supports; the Python runtime is an explicit 0.1/0.2
  adapter, not an implicit 0.3 runtime.
- [Inference layer](design/ygg-ai.md), [agent runtime](design/ygg-agent.md),
  [product bootstrap](design/ygg-coding-agent.md), and
  [terminal renderer](design/ygg-tui.md).
- [Presentation](design/ygg-presentation.md),
  [command and picker surfaces](design/ygg-command-picker-surfaces.md), and
  [theme status](themes.md).
- [Complete inherited source reference](current-reference.md): the former root
  README's substantive provider, tool, session, configuration, and architecture
  material, with historical installation guidance explicitly separated.

## Project, identity, and evidence

- [Canonical identity assets](assets/octet/README.md): source marks and wordmark,
  not screenshots of an actual candidate.
- [Draft 0.7.0 notes](releases/v0.7.0.md), [historical 0.6.7 notes](releases/v0.6.7.md),
  [changelog](../CHANGELOG.md), [roadmap](../ROADMAP.md), and
  [contributing](../CONTRIBUTING.md).
- [Distribution contract](distribution.md) and [npm release workflow](release/npm-trusted-publishing.md):
  inherited channel machinery, not proof that an octet package is published.
- [Measurement methodology](benchmarks/README.md) and
  [historical Ygg v0.6.2 Terminal-Bench evidence](benchmarks/tb21-v0.6.2/README.md).
  Historical campaigns, local audits, synthetic fixtures, and live candidate
  evidence are distinct; none substitutes for the others.
