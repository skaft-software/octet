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

## Shared authentication (item 30)

`octet_ai::provider_auth::codex` owns `CredentialStore::memory`, explicit
`CredentialStore::new(path)`, `CodexResolver`, and `login_with_interaction`.
`LoginInteraction` receives presentation facts and decides whether to open a
browser. The AI package never reads stdin, prints, opens browsers or installs
signals. The CLI/TUI supplies those operations. Existing credential JSON, private
filesystem rules and bounded cross-process locking are retained by moving their
implementation, including the platform backends and regression tests. This
necessarily moves the filesystem primitive; the agent public path re-exports it.

Construction is inert. `is_signed_in` reads local state without importing or
refreshing. Legacy import is an explicit frontend opt-in, not a library default.
Only explicit login, logout, refresh and cache publication mutate storage.
A resolver owns a started refresh task through rotation persistence even if its
request waiter is dropped. Keep the Tokio runtime alive until such bounded work
finishes; process termination/runtime shutdown and ambiguous remote exchanges
remain uncertain. Cancellation is not revocation or permission to replay.

## Catalog extraction (item 31)

The declaration manifest/generator, static presets, compatibility and pricing
now live under `octet_ai::providers`. The frontend reuses those owners and the
pure `inventory_metadata` and Codex decoders. Existing catalogs and filtering
remain intact. `providers::codex::CodexCatalog::fallback` and `from_inventory`
construct immutable credential-free snapshots without any I/O. Supplied
inventory must come from the caller's trusted account/endpoint; parsing alone
does not authenticate it. `load_local` explicitly reads private storage and
never imports or refreshes. `discover` explicitly authorizes only Codex inventory
and necessary refresh, with bounded bodies/deadlines and no redirects.

Snapshots distinguish supplied inventory, online discovery, fresh account/plan
cache, and conservative suggestions. `bind_into` binds the declared Responses
endpoint and a resolver pinned to the account/plan; replacement requires a new
catalog. It never chooses direct OpenAI. Capabilities, defaults, unknown pricing
and authoritative omissions retain their existing semantics. Context arithmetic
is shared; environment publication and session notes remain frontend-owned.
