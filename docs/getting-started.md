# Getting started

[Documentation](README.md) · [Installation](installation.md) · [Providers](providers.md)

This is the shortest safe path from an installed `octet` to a first useful,
tool-using coding task. Choose an installation lane, choose **one** provider
lane, verify it with a read-only request, and only then allow edits or commands.
Shell commands are in `sh` blocks; text in `text` blocks is entered inside the
interactive Octet UI, not in your shell.

> **Version boundary.** The published release is 0.7.6, while this checkout can
> contain candidate changes. Check `octet --version` and `octet --help` for the
> binary you will run. The `octet setup` contract below is verified against this
> checkout; do not assume a published binary has a candidate-only flag unless
> its own help lists it. Source builds are not signed release artifacts.

## 1. Choose an installation lane

### macOS or GNU/Linux x86-64: published native binary

The current native release supports macOS Apple silicon/Intel and GNU/Linux
x86-64. Install from the version-pinned release, then check the version:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/skaft-software/octet/releases/download/v0.7.6/install-octet.sh | sh
octet --version
```

Expected output includes `octet 0.7.6`. The release page is authoritative for
signed assets and availability; npm, Homebrew, crates.io, and SDK registries are
separate unpublished channels.

### Intentional source checkout: macOS or GNU/Linux

Use this lane only when you intentionally need the checkout under test. Install
Rust 1.86+ and [ripgrep](https://github.com/BurntSushi/ripgrep), then build and
run the binary by its full path:

```sh
cargo build --release --locked -p octet-coding-agent --bins
./target/release/octet --version
```

A source build does not replace an installed copy or establish release
publication. Use the matching binary consistently for the rest of this guide.

### Native Windows

There is no published native Windows download or support claim in this release
snapshot. Do not treat WSL, cross-compilation, or a macOS/Linux check as native
Windows qualification. Stop at this availability gate and use a separately
qualified Windows instruction when one is published; do not invent a download
URL.

## 2. Start in the repository you want to change

Run the following shell commands from the target repository root, not from the
Octet installation directory:

```sh
cd /path/to/project
octet --workspace "$PWD" --help
```

The `--workspace` form is optional when the shell is already in the repository.
Relative tool paths and the default command working directory use this workspace.

For a repository you have not reviewed, begin without `--workspace-trusted`.
That keeps project `.octet` configuration, `AGENTS.md`, and project resources out
of the initial context. After reviewing those files, add
`--workspace-trusted` on a later launch if you want them loaded; it does not grant
executable-extension trust or relax safety floors.

Use `--safe-mode` for the first run. It asks before every workspace mutation and
shell call, disables external paths, and keeps executable extensions stopped. It
is an approval policy, not an operating-system sandbox; use an isolated account,
container, VM, or platform sandbox for untrusted code. Full access is the default
when `--safe-mode` is absent and is appropriate only inside a boundary you chose.

## 3. Choose one provider lane

Provider setup, model availability, and connection verification are separate.
No extension is needed for this first coding task: installing an extension,
enabling it, trusting it, configuring it, and verifying its capability are
separate actions. Never put a credential value in a prompt, URL, repository file,
shell command, history, or session fixture.

### A. Codex subscription

Prerequisite: an account that can use the hosted Codex subscription flow and a
browser (or a second device) for device authorization. Run this in a shell; the
command exits after login:

```sh
octet --login codex
```

Octet prints the OpenAI verification URL and a one-time code. Keep the code
private and enter it only on the OpenAI page. If opening a browser is not
available, use the same flow without a browser opener:

```sh
octet --login codex --headless
```

After authorization, start from the repository root with a model ID listed by
your account-scoped inventory:

```sh
octet --safe-mode --model 'MODEL_ID'
```

`MODEL_ID` is a placeholder, not a promise of availability. For example, use
`gpt-5.6-luna` only if it is listed for this account; Astra may be listed as
`codex/gpt-6-astra`. A subscription plan or model name alone does not authorize
an unavailable model. `--offline` can use fresh cached/fallback metadata but does
not verify live availability or make inference local.

### B. OpenRouter API key

Prerequisite: an OpenRouter account, a usable API key, and a model available to
that account. Load the key into the environment without putting its value in a
command. On a Bash-compatible shell, this value-free pattern reads it at a hidden
prompt:

```sh
read -r -s OPENROUTER_API_KEY
export OPENROUTER_API_KEY
```

If the shell does not support that pattern, use its password manager or secret
manager integration. Never replace the commands with a literal key.

Start from the repository root and choose a model from the live OpenRouter
inventory or the `/model` picker:

```sh
octet --safe-mode --model 'openrouter/PROVIDER/MODEL'
```

The quoted ID is a placeholder. `openrouter/anthropic/claude-sonnet-4.6` is an
example of the shape only; account, model, and endpoint availability are
specific to OpenRouter. OpenRouter uses its built-in route and
`OPENROUTER_API_KEY`; do not turn it into a custom endpoint merely to copy a
credential value into a URL. Initial discovery is optional but useful for the
picker; `--offline` skips it and still does not disable inference traffic.

### C. LM Studio or another local compatible server

Prerequisite: start the local server, load a model, and note its API model ID.
LM Studio's documented OpenAI-compatible default is
`http://localhost:1234/v1/`. Compatible servers such as llama.cpp, vLLM, and
SGLang may use another explicit `/v1/` URL. This setup flow does not scan
localhost or the network.

The current-checkout setup flow is review-first. `--manual-model` avoids the setup
probe; the first command prints a receipt and does not write provider state:

```sh
octet setup --preset lm-studio --manual-model LOCAL_MODEL_ID
```

Review the endpoint, model, credential policy, and authority lines. Then repeat
the command with `--yes` to commit the reviewed values:

```sh
octet setup --preset lm-studio --manual-model LOCAL_MODEL_ID --yes
octet --safe-mode --model 'custom/local/LOCAL_MODEL_ID'
```

To use one bounded `/models` discovery request instead, omit `--manual-model` in
the preview, copy the discovered **API model ID** from the receipt, and commit
only after review. The setup discovery pass sends at most one request to the
selected endpoint; a confirmed rerun may probe again unless it also uses
`--offline --manual-model`:

```sh
octet setup --preset lm-studio
octet setup --preset lm-studio --model LOCAL_MODEL_ID --yes
```

For another explicitly selected compatible endpoint, use `--endpoint`; no probe
or write is the safest recovery when paired with `--offline --manual-model`:

```sh
octet setup --endpoint http://127.0.0.1:8000/v1/ \
  --no-auth --offline --manual-model LOCAL_MODEL_ID
octet setup --endpoint http://127.0.0.1:8000/v1/ \
  --no-auth --offline --manual-model LOCAL_MODEL_ID --yes
```

Replace `--no-auth` with `--api-key-env LOCAL_API_KEY` when the server requires a
bearer key; never use both options. The variable name is stored, not its value.
`--manual-model` supplies an explicit inventory and does not test inference; the
first task below is the connection check. Setup stores custom provider state in
an owner-private registry and saves the selected model preference only after
catalog verification. In the CLI, `--cancel` returns before endpoint discovery;
an interactive cancellation after discovery cannot undo that prior request.

## 4. Verify the route before allowing changes

Inside the TUI, `/status` shows the selected model, route, and capabilities. It
is an interactive command, not a shell command. Use the exact model ID from the
provider lane you chose (`custom/local/LOCAL_MODEL_ID` for the manual local
recipe). A narrow first request can verify provider inference and the read/search
tool boundary without permitting edits or commands:

```sh
octet --safe-mode --tools read,search --no-edit --no-write --no-process \
  --no-context-files --model 'MODEL_ID'
```

Submit this in the TUI:

```text
Read the repository's top-level manifest and locate one relevant source/test pair for my small coding task. Tell me which files you inspected, what the project uses to verify changes, and one narrow next step. Do not edit files or run commands.
```

**Expected first outcome:** a response naming real repository paths and a
verification plan, with no file mutation and no shell call. A successful response
is a useful live smoke check for the selected route, not proof of every model,
provider, terminal, or network condition.

After reviewing the project instructions and that response, start the first
change-capable session. Keep safe mode and approve each proposed effect yourself:

```sh
octet --continue --safe-mode --model 'MODEL_ID'
```

Then enter a small, bounded task such as:

```text
Implement the smallest change needed for [my small coding task]. Inspect the
relevant source and nearest tests first. Ask before every edit or command, show
changed paths and the diff, and stop for my review before committing.
```

If you intentionally reviewed and want project instructions loaded, add
`--workspace-trusted` to this launch. Trust is independent from tool enablement;
`--safe-mode` still prevents executable extensions and asks for mutations and
shell calls.

## 5. Recovery when setup or a model is missing

| Symptom | Recovery |
| --- | --- |
| No configured provider/model | Use Codex login, set the OpenRouter variable, or run the explicit local `octet setup` route. Interactive setup may offer local choices; print/RPC modes do not open a picker. |
| Codex credential missing, expired, or without a usable refresh token | Run `octet --login codex` again; use `--headless` when browser opening is unavailable. Select only an ID shown by the refreshed account inventory. |
| OpenRouter is absent or returns authentication failure | Set `OPENROUTER_API_KEY` through the secret manager, restart Octet, and choose a model listed for the account. `--offline` is not a credential repair. |
| Local endpoint is unreachable | Start the server and model, then check the exact explicit `/v1/` URL. No scan or retry of other endpoints occurs. A manual offline setup can save metadata, but inference still needs the server. |
| Discovery returns no usable model | Use `--manual-model LOCAL_MODEL_ID`; pair it with `--offline` when no `/models` request should occur. Manual setup is not a live-connection qualification. |
| Setup preview says it was not saved | Inspect the receipt, then rerun the same reviewed command with `--yes`. `--cancel` leaves state unchanged. |
| Credential policy is wrong | Choose exactly one of `--no-auth` and `--api-key-env VAR`; never put a secret in the endpoint URL or a repository config. |
| Provider ID already exists or the registry changed | Choose another provider ID, or deliberately rerun with `--replace` after review. On a concurrent-registry error, reload and merge deliberately; setup never silently overwrites another writer. |
| A resumed model is unavailable | Cancel/resume a different session or select a currently available model with `/model`; do not infer availability from the old name. |

For detailed transport, credential, discovery, and reasoning limits, see
[providers](providers.md), [CLI setup options](cli.md#provider-setup), and
[tools and permissions](tools.md). For durable session recovery, see
[sessions](sessions.md).

## Source identity

Octet source names are `octet`, `octet-host`, `octet-*`, `OCTET_*`, and `.octet`.
An Octet installation does not migrate Ygg data automatically. Keep provider
credentials and sessions in their owner-private stores, and keep all values out
of prompts, screenshots, support reports, and issue bodies.
