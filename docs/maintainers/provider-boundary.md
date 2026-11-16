# Reusable provider boundary

Revalidated at `b7382076` (items 29–34). Reuse the existing `octet-ai` package;
there is no second provider registry or inference engine.

| Owner | Existing responsibilities | Extraction boundary |
| --- | --- | --- |
| `octet-ai` | `AiClient`, canonical messages/events, `ModelCatalog`, `Auth::Dynamic`, `CredentialResolver`, codecs, metadata and integer pricing | Remains the inference dependency; add explicit provider authentication/storage and catalog composition here. |
| `octet-agent` | Agent loop, tools, extensions, sessions; descriptor-bound `secure_fs` | No agent-loop dependency is permitted from AI. Move the filesystem primitive needed by private credentials down and retain its public agent export. |
| `octet-coding-agent/auth/codex` | OAuth wire, browser callback, file/cache storage, resolver, login presentation | Move reusable behavior; leave stdin, output, browser launching, signal handling and method selection in the caller. |
| `octet-coding-agent/providers` | Declarations, route registration, compatibility, static presets, pricing, environment/ADC and Copilot binding | Preserve declarations and exact routes; do not replace catalogs or lifecycle policy. |
| `octet-coding-agent/app/bootstrap` | Account-scoped Codex inventory/cache plus frontend readiness, context notes, defaults and picker orchestration | Extract pure catalog/route behavior; retain readiness envelopes, diagnostics and selection in the frontend. |

Storage construction must not read credentials, import legacy state, create files
or make requests. File access is explicit and caller-rooted; in-memory storage
requires no application directory. Login interaction is supplied by the caller.
Local catalog inspection never authorizes refresh, discovery or inference.
A bound route uses the existing dynamic resolver, not a token export. Inference
is one caller-authorized operation; tools and continuation are not AI duties.

The separation reference is Pi source at
`9c9ce8547e5a88cbe834a53e9a6d83a789d08e47`:
`packages/ai/src/auth/types.ts:50–94,153–165` separates serialized storage and
interaction; `packages/ai/src/auth/resolve.ts:105–155` retains refresh/persistence
ownership after cancellation. This is a responsibility model, not a change to
Octet credential precedence, storage format or provider wire contracts.
