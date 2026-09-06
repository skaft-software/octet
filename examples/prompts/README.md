# Prompt template examples

Copy these files into `.octet/prompts/` for one trusted project or `~/.octet/prompts/` globally. Markdown templates intentionally use Pi-compatible frontmatter and argument expansion. octet also accepts the small TOML form and adds deterministic variables such as `{{prompt}}` and `{{workspace}}`.

Invoke a discovered template with `/local-review …`, `/prompt local-review …`, or select one at startup with `octet --prompt local-review "…"`.

Add `--debug-prompt` to display the complete deterministic expansion and its
content hash before the request is sent.
