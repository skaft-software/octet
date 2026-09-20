//! Additive invocation options; never changes persisted trust or credentials.
use std::io::{IsTerminal, Read};
use std::path::Path;

use clap::Args;
use octet_agent::Session;
use octet_ai::{Media, Modality, ModelId};

use super::Cli;
use crate::codex_context::{
    CodexContextOverride, CODEX_CONTEXT_WINDOW_CAP, CODEX_PRO_CONTEXT_WINDOW,
};
use crate::config::{Config, Mode, ResumeSelector};
use crate::session_store::SessionStore;

const MAX_INPUT_BYTES: u64 = 20 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Clone, Debug, Default, Args)]
pub struct ParityOptions {
    /// List available models, optionally filtered by fuzzy provider/model search.
    #[arg(long, num_args = 0..=1, default_missing_value = "", value_name = "SEARCH")]
    pub list_models: Option<String>,
    /// Exact workspace-local session ID; create when missing, otherwise resume.
    #[arg(long, value_name = "ID", conflicts_with_all = ["continue_", "resume", "fork", "no_session"])]
    pub session_id: Option<String>,
    /// Set the selected session's display name (must not be empty).
    #[arg(long, short = 'n', value_name = "NAME")]
    pub name: Option<String>,
    /// Unsupported until an accounting-preserving ephemeral backend exists.
    #[arg(long, conflicts_with_all = ["continue_", "resume", "fork", "session_id"])]
    pub no_session: bool,
    /// Comma-separated model patterns (`provider/*`, `sonnet:high`) that scope
    /// model selection and cycling to the credential-filtered catalog.
    #[arg(long, value_name = "PATTERNS")]
    pub models: Option<String>,
    /// Raise the deliberate 272K Codex context cap. Above 272K the whole request
    /// is double-priced (about 2x input / 1.5x output, not only the excess) and
    /// long-running sessions are more likely to lose their websocket. Requires a
    /// Pro/ProLite plan and, above the cap, an explicit acknowledgement.
    #[arg(long = "codex-context-window", value_name = "TOKENS")]
    pub codex_context_window: Option<u64>,
    /// Acknowledge the double-priced cost cliff and increased websocket-drop
    /// risk of raising the Codex context window above the deliberate 272K cap.
    #[arg(
        long = "codex-context-window-acknowledge-cost-cliff",
        requires = "codex_context_window"
    )]
    pub codex_context_window_acknowledge_cost_cliff: bool,
}

impl ParityOptions {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.no_session && self.name.is_some() {
            anyhow::bail!("--no-session cannot name a session because no transcript is persisted");
        }
        if let Some(patterns) = &self.models {
            model_patterns(patterns)?;
        }
        if let Some(name) = &self.name {
            if name.trim().is_empty() || name.chars().any(char::is_control) || name.len() > 256 {
                anyhow::bail!(
                    "--name requires a non-empty name without controls (at most 256 bytes)"
                );
            }
        }
        if let Some(tokens) = self.codex_context_window {
            self.validate_codex_context(tokens)?;
        } else {
            debug_assert!(
                !self.codex_context_window_acknowledge_cost_cliff,
                "clap requires --codex-context-window for the acknowledgement flag"
            );
        }
        Ok(())
    }

    /// Fail-closed validation of `--codex-context-window`, independent of the
    /// model. Entitlement/ceiling checks that need the resolved model run later
    /// in `codex_context::resolve_codex_context_window`.
    fn validate_codex_context(&self, tokens: u64) -> anyhow::Result<()> {
        if tokens == 0 {
            anyhow::bail!("--codex-context-window must be greater than zero");
        }
        if tokens > CODEX_PRO_CONTEXT_WINDOW {
            anyhow::bail!(
                "--codex-context-window {tokens} is above the {CODEX_PRO_CONTEXT_WINDOW}-token maximum any Codex model is entitled to"
            );
        }
        if tokens > CODEX_CONTEXT_WINDOW_CAP && !self.codex_context_window_acknowledge_cost_cliff {
            anyhow::bail!(
                "--codex-context-window {tokens} is above the deliberate {CODEX_CONTEXT_WINDOW_CAP}-token Codex cap: {} Re-run with --codex-context-window-acknowledge-cost-cliff to accept the cost cliff and the websocket-drop risk",
                crate::codex_context::CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING
            );
        }
        Ok(())
    }

    /// The parsed opt-in Codex context-window override, when the flag is set.
    ///
    /// Uses the shared `codex_context` policy type so the CLI, TUI and the
    /// resolution entry point cannot drift.
    pub fn codex_context_override(&self) -> Option<CodexContextOverride> {
        self.codex_context_window.map(|tokens| {
            CodexContextOverride::raising(tokens, self.codex_context_window_acknowledge_cost_cliff)
        })
    }

    /// Publish the opt-in override through the stable environment bridge that the
    /// Codex context-window policy reads, so the value reaches resolution
    /// without a persisted config change. Only set when the flag is present; a
    /// user-provided environment value is otherwise left untouched.
    pub fn install_codex_context_env(&self) {
        // One publication path for every frontend: the TUI effort menu calls
        // `CodexContextOverride::publish` with the same values.
        self.codex_context_override()
            .unwrap_or(CodexContextOverride::NONE)
            .publish();
    }

    /// Apply the `--models` scope: resolve every pattern against the
    /// credential-filtered catalog and select the first scoped model only when
    /// no explicit model was requested for a new session.
    ///
    /// The ordered scope is returned for frontends that cycle models; it is not
    /// persisted, because the shared session ledger has no scope record.
    pub fn resolve_models(&self, config: &mut Config) -> anyhow::Result<Vec<ModelId>> {
        let Some(patterns) = self.models.as_deref() else {
            return Ok(Vec::new());
        };
        let parsed = model_patterns(patterns)?;
        let catalog = crate::app::bootstrap::model_catalog_with_offline(config.offline)?;
        let available = catalog
            .models()
            .map(|spec| (spec.id.0.clone(), spec.endpoint.0.clone()))
            .collect::<Vec<_>>();
        let scope = select_scoped_models(&parsed, &available)?;
        if let Some(selected) = scope.first() {
            if config.model.is_none() && matches!(config.resume, ResumeSelector::New) {
                config.model = Some(selected.id.clone());
                config.model_explicit = true;
                if config.reasoning.is_none() {
                    if let Some(level) = selected.reasoning.as_deref() {
                        config.reasoning = Some(crate::config::parse_reasoning(level)?);
                        config.reasoning_explicit = true;
                    }
                }
                crate::output::stderr_line(format!(
                    "Model scope selected {} ({}).",
                    selected.id.0, selected.pattern
                ));
            }
        }
        Ok(scope.into_iter().map(|scoped| scoped.id).collect())
    }

    /// `--no-session` is headless-only, and the frontend must be chosen
    /// explicitly.
    ///
    /// A bare `octet --no-session` in a terminal would be interactive, and piped
    /// stdin promotes a bare invocation to print mode (`prepare_input`), so the
    /// requirement is checked against the flags the operator actually typed
    /// before stdin is consumed. The ephemeral transcript lives in a temporary
    /// store whose lifetime is one non-interactive run.
    pub fn require_headless_frontend(&self, cli: &Cli) -> anyhow::Result<()> {
        if !self.no_session {
            return Ok(());
        }
        let explicit_mode_is_headless = cli.mode.as_deref().is_some_and(|mode| {
            mode.eq_ignore_ascii_case("json") || mode.eq_ignore_ascii_case("rpc")
        });
        if !cli.print && !explicit_mode_is_headless {
            anyhow::bail!(
                "--no-session requires a headless frontend (--print, --mode json, or --mode rpc)"
            );
        }
        Ok(())
    }

    pub fn select_session(&self, config: &mut Config) -> anyhow::Result<()> {
        if self.no_session {
            return self.begin_ephemeral(config);
        }
        if self.session_id.is_none() && self.name.is_none() {
            return Ok(());
        }
        let store = SessionStore::new(&config.session_dir, &config.workspace);
        // The store is workspace-scoped; materialize it (and its workspace
        // marker, exactly as bootstrap does) before creating a transcript.
        if !store.dir().is_dir() {
            store.write_workspace_marker()?;
        }
        let id = if let Some(id) = &self.session_id {
            // The canonical store validates the ID and rejects non-regular files.
            if !store.session_file_exists(id)? {
                Session::create(store.dir().join(format!("{id}.jsonl")))?;
            }
            id.clone()
        } else {
            match &config.resume {
                ResumeSelector::New => {
                    let path = store.new_path(&crate::modes::timestamp());
                    let id = path.file_stem().expect("allocated session filename").to_string_lossy().into_owned();
                    Session::create(path)?;
                    id
                }
                ResumeSelector::Continue => store.latest()?.id,
                ResumeSelector::Resume(Some(id)) => {
                    store.path_by_id(id)?;
                    id.clone()
                }
                ResumeSelector::Resume(None) | ResumeSelector::Fork(_) => anyhow::bail!(
                    "--name with a session picker or --fork requires the startup metadata hook; select an exact existing session with --session-id, or rename it after forking"
                ),
            }
        };
        if let Some(name) = &self.name {
            store.rename(&id, name.trim())?;
        }
        config.resume = ResumeSelector::Resume(Some(id));
        Ok(())
    }

    /// `--no-session`: run in a private temporary store so no conversation is
    /// persisted in the workspace, while durable accounting is preserved.
    ///
    /// The real session directory is captured so the post-run hook can append an
    /// accounting-only record there and then discard the temporary transcript.
    fn begin_ephemeral(&self, config: &mut Config) -> anyhow::Result<()> {
        if matches!(config.mode, Mode::Interactive) {
            anyhow::bail!(
                "--no-session requires a headless frontend (--print, --mode json, or --mode rpc)"
            );
        }
        let accounting_session_dir = config.session_dir.clone();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let transcript_root =
            std::env::temp_dir().join(format!("octet-no-session-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&transcript_root)?;
        config.session_dir = transcript_root.clone();
        config.resume = ResumeSelector::New;
        crate::session_store::begin_ephemeral_run(
            transcript_root,
            accounting_session_dir,
            config.workspace.clone(),
        );
        Ok(())
    }
}

/// One resolved `--models` entry: an ordered, credential-filtered model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedModel {
    pub id: ModelId,
    /// The pattern that selected this model, for diagnostics.
    pub pattern: String,
    /// Optional `:level` suffix applied when this model becomes the default.
    pub reasoning: Option<String>,
}

/// Split and validate comma-separated `--models` patterns. Empty patterns are
/// rejected instead of silently widening the scope.
pub fn model_patterns(value: &str) -> anyhow::Result<Vec<String>> {
    let patterns = value
        .split(',')
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if patterns.is_empty() {
        anyhow::bail!("--models requires at least one non-empty comma-separated pattern");
    }
    if patterns
        .iter()
        .any(|pattern| pattern.chars().any(char::is_control))
    {
        anyhow::bail!("--models patterns must not contain control characters");
    }
    Ok(patterns)
}

fn split_pattern(pattern: &str) -> (&str, Option<&str>) {
    match pattern.rsplit_once(':') {
        Some((head, level)) if is_reasoning_suffix(level) => (head, Some(level)),
        _ => (pattern, None),
    }
}

/// Whether a `--models` `:level` suffix names a real reasoning level.
fn is_reasoning_suffix(value: &str) -> bool {
    crate::config::parse_reasoning(value).is_ok()
}

/// Whether one `--models` / `/scoped-models` pattern selects a model: the bare
/// model id or the provider-qualified `provider/model` form, case-insensitively,
/// with `*`/`?` globs. A trailing `:level` suffix never participates.
pub fn model_pattern_matches(pattern: &str, id: &str, provider: &str) -> bool {
    let (head, _) = split_pattern(pattern);
    glob_match(head, id) || glob_match(head, &format!("{provider}/{id}"))
}

/// The unique available model a literal reference names, mirroring the
/// reference's `findExactModelReferenceMatch`: `provider/model` or the bare id,
/// case-insensitively. `None` when the reference is empty, globbed, unmatched,
/// or ambiguous across providers, so the caller can fall back to glob matching.
pub fn exact_model_reference<'a>(
    reference: &str,
    available: &'a [(String, String)],
) -> Option<&'a (String, String)> {
    let (head, _) = split_pattern(reference);
    let head = head.trim();
    if head.is_empty() || head.contains(['*', '?']) {
        return None;
    }
    let matches = available
        .iter()
        .filter(|(id, provider)| {
            id.eq_ignore_ascii_case(head) || format!("{provider}/{id}").eq_ignore_ascii_case(head)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// Resolve `--models` patterns against the credential-filtered catalog.
///
/// Reference semantics (`pi` `resolveModelScopeFromModels`): a literal
/// `provider/model` or bare-id reference resolves exactly before glob matching,
/// otherwise the pattern matches either form case-insensitively with `*`/`?`
/// globs; an optional trailing `:level` suffix is only stripped when it names a
/// real reasoning level; matches keep first-seen pattern order and are
/// deduplicated. A pattern that matches nothing is a warning, not a failure, so
/// one typo cannot discard the rest of the scope.
///
/// `available` is `(model id, provider id)` per catalog model. The resolved
/// order is exactly the requested pattern order, so an explicit ordered scope
/// (and its per-model reasoning suffixes) survives round-tripping.
pub fn select_scoped_models(
    patterns: &[String],
    available: &[(String, String)],
) -> anyhow::Result<Vec<ScopedModel>> {
    let mut scope: Vec<ScopedModel> = Vec::new();
    for pattern in patterns {
        let (head, reasoning) = split_pattern(pattern);
        // A literal reference resolves exactly, so a requested order is never
        // re-sorted by a broader glob and an id containing glob characters
        // still resolves as itself.
        if let Some((id, _)) = exact_model_reference(pattern, available) {
            let candidate = ScopedModel {
                id: ModelId(id.clone()),
                pattern: pattern.clone(),
                reasoning: reasoning.map(str::to_owned),
            };
            if !scope.iter().any(|existing| existing.id == candidate.id) {
                scope.push(candidate);
            }
            continue;
        }
        let mut matched = available
            .iter()
            .filter(|(id, provider)| model_pattern_matches(head, id, provider))
            .map(|(id, _)| ScopedModel {
                id: ModelId(id.clone()),
                pattern: pattern.clone(),
                reasoning: reasoning.map(str::to_owned),
            })
            .collect::<Vec<_>>();
        // The catalog is a map, so order it explicitly to keep the selected
        // default stable across runs (upstream preserves its catalog array).
        matched.sort_by(|left, right| left.id.0.cmp(&right.id.0));
        if matched.is_empty() {
            crate::output::stderr_line(format!(
                "Warning: no credential-configured models match --models pattern {pattern:?}; run `octet --list-models`"
            ));
            continue;
        }
        for candidate in matched {
            if !scope.iter().any(|existing| existing.id == candidate.id) {
                scope.push(candidate);
            }
        }
    }
    Ok(scope)
}

/// Case-insensitive glob with `*` (any run) and `?` (one character).
fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.to_lowercase().chars().collect::<Vec<_>>();
    let value = value.to_lowercase().chars().collect::<Vec<_>>();
    let (mut pattern_index, mut value_index) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == '?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == '*' {
            star = Some((pattern_index, value_index));
            pattern_index += 1;
        } else if let Some((star_index, star_value)) = star {
            pattern_index = star_index + 1;
            value_index = star_value + 1;
            star = Some((star_index, star_value + 1));
        } else {
            return false;
        }
    }
    pattern[pattern_index..].iter().all(|ch| *ch == '*')
}

#[derive(Default)]
pub(crate) struct InvocationInput {
    pub json: bool,
    pub media: Vec<Media>,
    pub remaining: Vec<String>,
}

/// RPC retains sole ownership of stdin. Other redirected frontends consume one
/// bounded UTF-8 prompt, not a sequence of physical lines.
pub(crate) fn prepare_input(cli: &mut Cli, cwd: &Path) -> anyhow::Result<InvocationInput> {
    let json = cli
        .mode
        .as_deref()
        .is_some_and(|mode| mode.eq_ignore_ascii_case("json"));
    let rpc = cli
        .mode
        .as_deref()
        .is_some_and(|mode| mode.eq_ignore_ascii_case("rpc"));
    let mut args: Vec<String> = cli.message.take().into_iter().collect();
    args.append(&mut cli.additional_messages);
    if rpc {
        if !args.is_empty() {
            anyhow::bail!(
                "RPC input must be sent as JSONL prompt commands on stdin, not positional prompts"
            );
        }
        return Ok(InvocationInput::default());
    }
    let piped = !std::io::stdin().is_terminal();
    let stdin = if piped {
        read_text(std::io::stdin().lock(), MAX_INPUT_BYTES)?
    } else {
        String::new()
    };
    let mut input = expand_input(&args, &stdin, cwd)?;
    input.json = json;
    cli.message = input.remaining.first().cloned();
    if !input.remaining.is_empty() {
        input.remaining.remove(0);
    }
    // Like upstream, redirected stdin cannot enter an interactive reader after
    // consumption. Explicit --plain still gets its chronological frontend.
    if piped && !json && !cli.plain {
        cli.mode = None;
        cli.print = true;
    }
    let interactive = !cli.print && !json;
    if interactive && (!input.media.is_empty() || !input.remaining.is_empty()) {
        anyhow::bail!("initial images and sequential prompts require --print or --mode json; interactive startup needs a typed input queue hook");
    }
    Ok(input)
}

fn read_text(reader: impl Read, limit: u64) -> anyhow::Result<String> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        anyhow::bail!("prompt input exceeds {limit} bytes");
    }
    Ok(String::from_utf8(bytes)?)
}

fn escape_attribute(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn expand_input(args: &[String], stdin: &str, cwd: &Path) -> anyhow::Result<InvocationInput> {
    let mut text = stdin.to_owned();
    let mut media = Vec::new();
    let mut messages = Vec::new();
    let mut total_bytes = stdin.len() as u64;
    for arg in args {
        let Some(path) = arg.strip_prefix('@') else {
            messages.push(arg.clone());
            continue;
        };
        let path = if let Some(tail) = path.strip_prefix("~/") {
            dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("home directory unavailable"))?
                .join(tail)
        } else {
            cwd.join(path)
        };
        let path = path
            .canonicalize()
            .map_err(|error| anyhow::anyhow!("@file {}: {error}", path.display()))?;
        let bytes =
            octet_agent::secure_fs::read_regular_file_bounded(&path, MAX_FILE_BYTES as usize)?;
        total_bytes += bytes.len() as u64;
        if total_bytes > MAX_INPUT_BYTES {
            anyhow::bail!("combined @file/stdin input exceeds {MAX_INPUT_BYTES} bytes");
        }
        if bytes.is_empty() {
            continue;
        }
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some("image/png")
        } else if bytes.starts_with(b"\xff\xd8\xff") {
            Some("image/jpeg")
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some("image/gif")
        } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
            Some("image/webp")
        } else {
            None
        };
        let label = escape_attribute(&path.to_string_lossy());
        if let Some(mime) = mime {
            if media.len() >= 8 {
                anyhow::bail!("at most 8 @file images may be attached");
            }
            media.push(Media::image_bytes(bytes.into(), mime.parse()?));
            text.push_str(&format!("<file name=\"{label}\"></file>\n"));
        } else {
            let content = String::from_utf8(bytes).map_err(|_| {
                anyhow::anyhow!(
                    "@file {} is neither UTF-8 text nor a supported image",
                    path.display()
                )
            })?;
            text.push_str(&format!(
                "<file name=\"{label}\">\n{}\n</file>\n",
                content.trim_start_matches('\u{feff}')
            ));
        }
    }
    if !messages.is_empty() {
        text.push_str(&messages.remove(0));
    }
    if !text.is_empty() || !media.is_empty() {
        messages.insert(0, text);
    }
    Ok(InvocationInput {
        json: false,
        media,
        remaining: messages,
    })
}

pub(crate) fn list_models(config: &Config, search: &str) -> anyhow::Result<()> {
    let catalog = crate::app::bootstrap::model_catalog_with_offline(config.offline)?;
    let mut models = catalog
        .models()
        .filter(|spec| fuzzy_match(search, &format!("{} {}", spec.endpoint.0, spec.id.0)))
        .collect::<Vec<_>>();
    models.sort_by(|a, b| (&a.endpoint.0, &a.id.0).cmp(&(&b.endpoint.0, &b.id.0)));
    if models.is_empty() {
        crate::output::stdout_line("No matching available models.");
        return Ok(());
    }
    crate::output::stdout_table_line("PROVIDER\tMODEL\tCONTEXT\tMAX-OUT\tTHINKING\tIMAGES");
    for model in models {
        let terminal = crate::output::stdout_is_terminal();
        crate::output::stdout_table_line(format!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            crate::output::table_field(&model.endpoint.0, terminal),
            crate::output::table_field(&model.id.0, terminal),
            model.limits.context_window,
            model.limits.max_output_tokens,
            model.capabilities.reasoning.is_some(),
            model
                .capabilities
                .input_modalities
                .contains(Modality::Image)
        ));
    }
    Ok(())
}

fn fuzzy_match(query: &str, value: &str) -> bool {
    let value = value.to_lowercase();
    query.split_whitespace().all(|token| {
        let mut chars = value.chars();
        token
            .to_lowercase()
            .chars()
            .all(|needle| chars.by_ref().any(|ch| ch == needle))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expansion_combines_stdin_files_and_first_prompt_then_preserves_sequence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "\u{feff}context").unwrap();
        std::fs::write(dir.path().join("a.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let input = expand_input(
            &[
                "@a.txt".into(),
                "first".into(),
                "@a.png".into(),
                "second".into(),
            ],
            "piped\n",
            dir.path(),
        )
        .unwrap();
        assert_eq!(input.media.len(), 1);
        assert_eq!(input.remaining.len(), 2);
        assert!(input.remaining[0].starts_with("piped\n<file"));
        assert!(input.remaining[0].contains("\ncontext\n"));
        assert!(input.remaining[0].ends_with("first"));
        assert_eq!(input.remaining[1], "second");
    }
    #[test]
    fn missing_and_oversized_inputs_fail_before_submission() {
        let dir = tempfile::tempdir().unwrap();
        assert!(expand_input(&["@missing".into()], "", dir.path()).is_err());
        let file = std::fs::File::create(dir.path().join("large")).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        assert!(expand_input(&["@large".into()], "", dir.path()).is_err());
        assert!(read_text(&b"abcd"[..], 3).is_err());
    }
    #[test]
    fn fuzzy_search_is_case_insensitive_and_token_conjunctive() {
        assert!(fuzzy_match("OAI gp4", "openai gpt-4o"));
        assert!(!fuzzy_match("openai claude", "openai gpt-4o"));
    }
    #[test]
    fn model_patterns_match_ids_provider_qualified_forms_and_levels() {
        let available = vec![
            ("custom/alpha-model".to_owned(), "custom-openai".to_owned()),
            ("gpt-6-astra".to_owned(), "openai".to_owned()),
        ];
        let bare = select_scoped_models(&["*alpha*".to_owned()], &available).unwrap();
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].id.0, "custom/alpha-model");
        let qualified =
            select_scoped_models(&["custom-openai/custom/alpha-*".to_owned()], &available).unwrap();
        assert_eq!(qualified[0].id.0, "custom/alpha-model");
        let provider = select_scoped_models(&["openai/gpt-*".to_owned()], &available).unwrap();
        assert_eq!(provider[0].id.0, "gpt-6-astra");
        let level = select_scoped_models(&["openai/gpt-*:high".to_owned()], &available).unwrap();
        assert_eq!(level[0].reasoning.as_deref(), Some("high"));
        assert_eq!(level[0].id.0, "gpt-6-astra");
        // An unmatched pattern warns and leaves the rest of the scope intact.
        let mixed =
            select_scoped_models(&["nope*".to_owned(), "openai/gpt-*".to_owned()], &available)
                .unwrap();
        assert_eq!(mixed.len(), 1);
        assert_eq!(mixed[0].id.0, "gpt-6-astra");
        assert_eq!(model_patterns("a, b").unwrap(), vec!["a", "b"]);
        assert!(model_patterns(" ,").is_err());
    }

    /// 5.7 — a literal reference resolves exactly before glob matching, so an
    /// explicit ordered scope keeps the requested order and each model keeps its
    /// requested reasoning suffix across a persistence round-trip.
    #[test]
    fn literal_references_preserve_requested_order_and_reasoning_suffixes() {
        let available = vec![
            ("custom/alpha-model".to_owned(), "custom-openai".to_owned()),
            ("gpt-6-astra".to_owned(), "openai".to_owned()),
            ("gpt-6-luna".to_owned(), "openai".to_owned()),
        ];
        // Requested order is the pattern order, not the catalog order.
        let ordered = select_scoped_models(
            &[
                "gpt-6-luna:high".to_owned(),
                "custom-openai/custom/alpha-model:low".to_owned(),
                "gpt-6-astra".to_owned(),
            ],
            &available,
        )
        .unwrap();
        assert_eq!(
            ordered
                .iter()
                .map(|scoped| (scoped.id.0.as_str(), scoped.reasoning.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("gpt-6-luna", Some("high")),
                ("custom/alpha-model", Some("low")),
                ("gpt-6-astra", None),
            ]
        );
        // The persisted id list round-trips through the same resolver.
        let persisted = ordered
            .iter()
            .map(|scoped| match &scoped.reasoning {
                Some(level) => format!("{}:{level}", scoped.id.0),
                None => scoped.id.0.clone(),
            })
            .collect::<Vec<_>>()
            .join(",");
        let parsed = model_patterns(&persisted).unwrap();
        let round_tripped = select_scoped_models(&parsed, &available).unwrap();
        assert_eq!(
            round_tripped
                .iter()
                .map(|scoped| (scoped.id.0.as_str(), scoped.reasoning.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("gpt-6-luna", Some("high")),
                ("custom/alpha-model", Some("low")),
                ("gpt-6-astra", None),
            ]
        );
        // An ambiguous bare id falls back to glob matching instead of guessing.
        let ambiguous = vec![
            ("shared".to_owned(), "one".to_owned()),
            ("shared".to_owned(), "two".to_owned()),
        ];
        assert!(exact_model_reference("shared", &ambiguous).is_none());
        assert!(model_pattern_matches("shared", "shared", "one"));
    }
}
