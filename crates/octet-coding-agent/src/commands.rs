#![allow(missing_docs)]

use std::path::Path;

use crate::app::{reasoning_label, App, Reconfig};
use crate::cli::parity::ScopedModel;
use crate::codex_context::CodexContextTier;
use crate::compaction::{context_window, estimate_next_request_tokens};
use crate::config::CompactionMode;
use crate::presentation::{format_token_rate, ModelDisplayMetadata};
use crate::session_store::active_branch_title;
use octet_agent::{
    analyze_session_cache, analyze_session_cache_stats, CacheStats, EntryValue, Session,
    UsageRecordKind,
};
use octet_ai::{
    AssistantPart, Cost, Message, Model, ModelId, Protocol, ResponsesRuntimeProfile, Usage,
};

/// Parsed in-TUI command. Commands are deliberately separate from shell CLI
/// options: only editor text beginning with `/` enters this grammar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Login(Option<String>),
    Setup,
    Logout(Option<String>),
    Model(Option<String>),
    Thinking(Option<String>),
    /// Inspect or change the compiled terminal appearance selector.
    Theme(Option<String>),
    /// Set, clear, or report the Codex-only fast service tier.
    Fast(Option<bool>),
    Verbose(Option<bool>),
    /// Request an immediate final answer without exposing tools.
    Answer(Option<String>),
    Compact,
    CompactWithInstructions(String),
    AutoCompact(Option<AutoCompactSetting>),
    Reload,
    New,
    Resume(Option<String>),
    /// Fork the active session from a selected user-message boundary.
    Fork,
    /// Clone the active session at its current head.
    Clone,
    Status,
    Context,
    Help(Option<String>),
    Hotkeys,
    Copy,
    Session,
    Cost,
    Cache,
    Update,
    /// Read the current version's bundled release notes without inference.
    Changelog,
    Name(Option<String>),
    Export(Option<String>),
    Exit,
    /// Hidden diagnostics surface: writes rendered lines and message JSONL to
    /// the owner-private debug log. Deliberately absent from the suggestion
    /// list, matching the reference's hidden `/debug`.
    Debug,
    /// List or invoke named prompt templates. The optional string preserves
    /// the template name and raw arguments for deterministic expansion.
    Prompt(Option<String>),
    Skills(SkillsSubcommand),
    /// Inspect or reload explicitly enabled executable extensions.
    Extensions(ExtensionsSubcommand),
    /// Inspect or mutate the durable session goal.
    Goal(GoalCommand),
    /// Inspect or change user-level display and default preferences.
    Settings(SettingsCommand),
    /// Inspect or change the ordered model cycling scope.
    ScopedModels(ScopedModelsCommand),
    /// Local shell escape. `!command` results are model-visible context;
    /// `!!command` results are explicitly excluded from it.
    Bash(BashEscape),
    Unknown(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoCompactSetting {
    Mode(CompactionMode),
    ThresholdPercent(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtensionsSubcommand {
    Menu,
    Status,
    Reload,
    Inspect { reference: String },
    Action { extension: String, action: String },
}

/// Subcommands for the `/goal` slash command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoalCommand {
    /// Set or replace the objective.
    Set(String),
    /// Show the current objective and lifecycle state.
    Status,
    /// Pause automatic continuation.
    Pause,
    /// Resume a paused objective.
    Resume,
    /// Remove the objective.
    Clear,
    /// Show goal command usage.
    Help,
}

/// Subcommands for the `/skills` slash command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillsSubcommand {
    /// List all discovered skills.
    List,
    /// Show details about a specific skill.
    Show(String),
    /// List all currently active skills.
    Active,
    /// Search discovered skill metadata.
    Search(String),
    /// Explicitly load and activate a skill.
    Load(String),
    /// Rescan all configured skill roots.
    Reload,
    /// Explicitly deactivate a skill.
    Off(String),
}

/// Subcommands for the `/settings` slash command.
///
/// Settings are user-level display/default preferences. Project trust is
/// deliberately absent: this surface never persists a default trust decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsCommand {
    /// Show the effective settings summary.
    Show,
    /// Select the compiled terminal appearance, or open the picker.
    Theme(Option<String>),
    /// Enable, disable, or report inline tool-result image placement.
    Images(Option<bool>),
    /// Set, or report, the persisted default model for new sessions.
    DefaultModel(Option<String>),
    /// Set, or report, the persisted default reasoning level.
    DefaultReasoning(Option<String>),
    /// Report the active endpoint's declared transport.
    Transport,
    /// Report the editor-padding policy.
    Padding,
}

/// Subcommands for the `/scoped-models` slash command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScopedModelsCommand {
    /// Show the ordered cycling scope and what would be persisted.
    Show,
    /// Scope cycling to every currently available model.
    All,
    /// Remove the scope restriction and the persisted pattern list.
    Clear,
    /// Enable every model a model-id or provider glob selects.
    Enable(String),
    /// Disable every model a model-id or provider glob selects.
    Disable(String),
    /// Enable the target when any of it is disabled; otherwise disable it.
    Toggle(String),
    /// Move one scoped model within the ordered cycling scope.
    Move { model: String, direction: ScopeMove },
}

/// One ordered movement inside the `/scoped-models` scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeMove {
    Up,
    Down,
    Top,
    Bottom,
}

impl ScopeMove {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "up" => Some(Self::Up),
            "down" => Some(Self::Down),
            "top" => Some(Self::Top),
            "bottom" => Some(Self::Bottom),
            _ => None,
        }
    }
}

/// One parsed local shell escape (`!command` / `!!command`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BashEscape {
    /// Exact command text after the `!`/`!!` prefix.
    pub command: String,
    /// `true` for `!!command`: the result must never enter model context.
    pub excluded: bool,
}

impl BashEscape {
    /// Parse one trimmed editor submission, or `None` when it is not a shell
    /// escape. `!` and `!!` alone parse as empty commands so dispatch can
    /// report usage instead of silently running nothing.
    pub fn parse(input: &str) -> Option<Self> {
        let body = input.trim().strip_prefix('!')?;
        let excluded = body.starts_with('!');
        let command = body.strip_prefix('!').unwrap_or(body).trim();
        Some(Self {
            command: command.to_owned(),
            excluded,
        })
    }
}

/// One command shown in the prompt's live slash-command suggestions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlashCommandSuggestion {
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
    pub(crate) accepts_argument: bool,
}

macro_rules! slash {
    ($name:literal, $usage:literal, $description:literal, $accepts:literal) => {
        SlashCommandSuggestion {
            name: $name,
            usage: $usage,
            description: $description,
            accepts_argument: $accepts,
        }
    };
}

// The popup is the command-discovery surface, so this stays a single flat list:
// no one-off `Session` header; self-help commands live in the same catalog.
/// Commands accepted by exact name but deliberately absent from the popup and
/// from `/help`, matching the reference's hidden `/debug`.
const HIDDEN_COMMANDS: &[&str] = &["debug"];

const SLASH_COMMANDS: &[SlashCommandSuggestion] = &[
    slash!("new", "/new", "start a fresh conversation", false),
    slash!(
        "resume",
        "/resume [id]",
        "re-open or list recent sessions",
        true
    ),
    slash!("fork", "/fork", "fork from a previous user message", false),
    slash!("clone", "/clone", "clone the current session", false),
    slash!(
        "session",
        "/session [info]",
        "inspect session file, messages, tokens and cost",
        false
    ),
    slash!(
        "hotkeys",
        "/hotkeys",
        "show resolved user keybindings",
        false
    ),
    slash!("copy", "/copy", "copy the last assistant message", false),
    slash!("model", "/model [id]", "select or change the model", true),
    slash!(
        "thinking",
        "/thinking [level]",
        "set reasoning effort",
        true
    ),
    slash!(
        "theme",
        "/theme [auto|light|dark|name]",
        "choose terminal appearance or a discovered theme",
        true
    ),
    slash!(
        "answer",
        "/answer [instruction]",
        "answer now from current evidence without tools",
        true
    ),
    slash!(
        "compact",
        "/compact [instructions]",
        "compact conversation context",
        true
    ),
    slash!(
        "auto-compact",
        "/auto-compact [off|local|native|85%]",
        "show or configure automatic compaction",
        true
    ),
    slash!(
        "fast",
        "/fast [on|off|status]",
        "inspect or set the Codex fast service tier",
        true
    ),
    slash!(
        "verbose",
        "/verbose [on|off]",
        "show or hide raw tool details",
        true
    ),
    slash!(
        "reload",
        "/reload",
        "reload instructions, prompts, skills, and extensions",
        false
    ),
    slash!("login", "/login [provider]", "sign in to a provider", true),
    slash!(
        "setup",
        "/setup",
        "add an API key or set up a provider",
        false
    ),
    slash!(
        "logout",
        "/logout [provider]",
        "remove stored credentials",
        true
    ),
    slash!("status", "/status", "show model and diagnostics", false),
    slash!(
        "context",
        "/context",
        "show what occupies the model context",
        false
    ),
    slash!(
        "help",
        "/help [command]",
        "show commands and octet self-documentation",
        true
    ),
    slash!("cost", "/cost", "show turn and session cost", false),
    slash!("cache", "/cache", "show prompt-cache diagnostics", false),
    slash!(
        "changelog",
        "/changelog",
        "read this version's bundled release notes (interactive TUI)",
        false
    ),
    slash!(
        "update",
        "/update",
        "check for a newer octet release; run `octet update` to install",
        false
    ),
    slash!("name", "/name [name]", "show or rename this session", true),
    slash!(
        "export",
        "/export [path]",
        "export this session with secret redaction",
        true
    ),
    slash!(
        "prompt",
        "/prompt [name] [arguments]",
        "list or expand prompt templates",
        true
    ),
    slash!(
        "skills",
        "/skills [subcommand]",
        "manage and view agent skills",
        true
    ),
    slash!(
        "extensions",
        "/extensions [status|reload|inspect <agent-session:…>|action <extension> <action-id>]",
        "enable, disable, inspect, or reload executable extensions",
        true
    ),
    slash!(
        "goal",
        "/goal [objective|status|pause|resume|clear]",
        "inspect or manage the durable session goal",
        true
    ),
    slash!(
        "settings",
        "/settings [theme|images on/off|default model/reasoning|transport|padding]",
        "show or change display and default preferences",
        true
    ),
    slash!(
        "scoped-models",
        "/scoped-models [all|clear|enable|disable|toggle|move]",
        "manage the ordered model cycling scope",
        true
    ),
    slash!("exit", "/exit", "exit octet", false),
];

/// Package-local copy of docs/releases/v<CARGO_PKG_VERSION>.md. Keep the copy
/// inside the crate: an include outside CARGO_MANIFEST_DIR breaks cargo packages.
pub(crate) const CURRENT_CHANGELOG: &str = include_str!(concat!(
    "tui/view/releases/v",
    env!("CARGO_PKG_VERSION"),
    ".md"
));

/// Do not accidentally send this local, read-only TUI command to a provider in
/// frontends that have no report surface. Their ordinary error channel owns it.
pub(crate) fn reject_tui_changelog(input: &str) -> anyhow::Result<()> {
    if matches!(parse(input), Command::Changelog) {
        anyhow::bail!("/changelog is available in the interactive TUI; launch octet without --plain, --print, or --mode rpc and enter /changelog");
    }
    Ok(())
}

/// Complete TUI-ordered built-in slash-command catalog.
pub fn slash_commands() -> &'static [SlashCommandSuggestion] {
    SLASH_COMMANDS
}

/// Render local command help without contacting a model.
pub fn help_text(workspace: &Path, topic: Option<&str>) -> String {
    let mut text = String::from("octet help\n\n");
    text.push_str(
        "octet can explain and extend itself. Ask it about behavior, or request an explicit change; in a octet source checkout it is told where to read the canonical docs and Rust source.\n\n",
    );

    if let Some(topic) = topic.map(str::trim).filter(|topic| !topic.is_empty()) {
        let topic = topic.trim_start_matches('/');
        let matches = SLASH_COMMANDS
            .iter()
            .filter(|command| command.name == topic || command.name.starts_with(topic))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [command] => {
                text.push_str(&format!("{} — {}", command.usage, command.description));
            }
            [] => text.push_str(&format!("No built-in command matches /{topic}.")),
            _ => {
                text.push_str(&format!("Several commands match /{topic}:\n"));
                for command in matches {
                    text.push_str(&format!("  {} — {}\n", command.usage, command.description));
                }
            }
        }
        text.push_str("\n\n");
    } else {
        text.push_str("Slash commands:\n");
        for command in SLASH_COMMANDS {
            text.push_str(&format!("  {} — {}\n", command.usage, command.description));
        }
        text.push_str(
            "\nProject prompt templates, skills, and executable extensions are discovered at runtime; use /prompt, /skills, and /extensions status to inspect them (bare /extensions manages activation).\n\n",
        );
    }

    text.push_str(&crate::resources::self_documentation_help(workspace));
    text
}

/// Whether the active endpoint declares the Codex Responses route.
///
/// The fast service tier is a Codex-route capability. This gates on the
/// declared protocol and endpoint runtime profile -- never on a provider name --
/// so a route that declares the Codex profile is admitted and every other route
/// is rejected explicitly instead of being silently ignored.
pub fn codex_fast_tier_endpoint(model: &Model) -> bool {
    model.spec.protocol == Protocol::OpenAiResponses
        && model
            .endpoint
            .runtime
            .responses_profile
            .accepts_service_tier()
}

/// Current fast selection, independent of queued changes or provider acceptance.
pub fn fast_status_text(model: &Model, tier: Option<octet_ai::ServiceTier>) -> &'static str {
    if !codex_fast_tier_endpoint(model) {
        "Fast mode: unavailable on this model route"
    } else if tier == Some(octet_ai::ServiceTier::Priority) {
        "Fast mode: on — priority requested; provider acceptance and speed are not guaranteed"
    } else {
        "Fast mode: off — no priority requested"
    }
}

/// Whether the active endpoint declares the Codex Responses route.
///
/// `/fast` and the context-window surface are Codex-route capabilities, so both
/// gate on the declared protocol and endpoint runtime profile rather than on a
/// provider name. Widening this is an endpoint-declaration change.
pub fn codex_responses_endpoint(model: &Model) -> bool {
    model.spec.protocol == Protocol::OpenAiResponses
        && model.endpoint.runtime.responses_profile == ResponsesRuntimeProfile::Codex
}

/// Read-only Codex context-window facts for one Codex route, plus the
/// deliberately fail-closed raise path.
///
/// The deliberate 272K working cap is a maintainer decision, not a defect: it
/// is reported as a clamp together with its reason rather than hidden. Above
/// 272K the whole request is priced differently, so the surface names the
/// uncertainty operation instead of rendering an exact-looking figure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexContextSurface {
    model_id: String,
    /// Window the live session actually budgets against.
    effective_window: u64,
    /// Window the plan selects before octet's deliberate cap.
    advertised_window: u64,
    /// Largest window the model's entitlement allows an override to request.
    entitled_max_window: u64,
    /// Whether the account's plan carries the Pro/ProLite entitlement.
    entitled: bool,
}

impl CodexContextSurface {
    /// Capture the surface for a Codex route, or `None` for every other route.
    ///
    /// `entitled` mirrors `ChatGptPlan::uses_max_context_window`; the caller
    /// reads it from the subscription credential and defaults to `false` when
    /// it cannot be established, so an unknown plan never grants a raise.
    pub fn capture(model: &Model, entitled: bool) -> Option<Self> {
        if !codex_responses_endpoint(model) {
            return None;
        }
        let model_id = model.spec.id.0.clone();
        let (fallback_default, entitled_max_window) =
            crate::codex_context::entitled_context_windows(&model_id);
        let advertised_window =
            if CodexContextTier::from_plan_entitlement(entitled) == CodexContextTier::Extended {
                fallback_default.max(entitled_max_window)
            } else {
                fallback_default
            };
        Some(Self {
            model_id,
            effective_window: model.spec.limits.context_window,
            advertised_window,
            entitled_max_window,
            entitled,
        })
    }

    /// Window the live session budgets against.
    pub fn effective_window(&self) -> u64 {
        self.effective_window
    }

    /// The deliberate reduction of the advertised window, when the cap (rather
    /// than the plan's own default) is what narrowed it.
    pub fn clamp(&self) -> Option<crate::codex_context::CodexContextClamp> {
        let working = crate::codex_context::working_context_window(&self.model_id);
        (self.effective_window == working && self.advertised_window > working).then(|| {
            crate::codex_context::CodexContextClamp {
                model_id: self.model_id.clone(),
                advertised_context_window: self.advertised_window,
                effective_context_window: self.effective_window,
            }
        })
    }

    /// `true` when the effective window is above the standard 272K tier, where
    /// the whole request is double-priced and cost/usage cannot be claimed
    /// exactly.
    pub fn has_uncertain_usage(&self) -> bool {
        self.effective_window > crate::codex_context::CODEX_CONTEXT_WINDOW_CAP
    }

    /// The `Session::record_usage_uncertainty` operation to record for this
    /// route, when its accounting is uncertain.
    #[cfg(test)]
    pub fn uncertain_usage_operation(&self) -> Option<&'static str> {
        self.has_uncertain_usage()
            .then_some(crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION)
    }

    /// The largest window this route may be raised to, or `None` when the plan
    /// carries no entitlement for a window above the deliberate cap.
    pub fn raise_target(&self) -> Option<u64> {
        (self.entitled
            && self.entitled_max_window
                > crate::codex_context::working_context_window(&self.model_id))
        .then_some(self.entitled_max_window)
    }

    /// Bounded, truthful facts for the effort/thinking menu.
    ///
    /// Plain user language only, matching the session's Codex context notes:
    /// every window is labelled, no internal API path or operation identifier is
    /// ever rendered, and cost is never shown as an exact figure above the 272K
    /// standard tier. The diagnostic operation id stays in
    /// [`Self::uncertain_usage_operation`], which the session recorder reads
    /// directly; the deliberate 272K cap is a decision, never a defect.
    pub fn summary_lines(&self) -> Vec<String> {
        let label = crate::codex_context::context_window_label;
        let cap = label(crate::codex_context::CODEX_CONTEXT_WINDOW_CAP);
        let entitled = self.entitled_max_window.max(self.effective_window);
        let mut lines = vec![format!(
            "Codex context window — advertised {}, entitled {}, effective {}{}",
            label(self.advertised_window),
            label(entitled),
            label(self.effective_window),
            if self.has_uncertain_usage() {
                ": cost and usage are UNCERTAIN"
            } else {
                ""
            },
        )];
        if self.has_uncertain_usage() {
            lines.push(format!(
                "At {} — above the {cap} standard tier — every request is double-priced (about 2x input and 1.5x output for the whole request, not just the excess) and long-running sessions are likelier to drop the Codex websocket, so this session's cost and usage are reported as UNCERTAIN instead of an exact figure.",
                label(self.effective_window),
            ));
        }
        match self.clamp() {
            Some(clamp) => lines.push(clamp.message()),
            None => lines.push(format!(
                "octet budgets {} for this Codex route; the provider advertises {}.",
                label(self.effective_window),
                label(self.advertised_window),
            )),
        }
        match self.raise_target() {
            Some(target) => lines.push(format!(
                "Raise to {}: {}",
                label(target),
                self.raise_instruction(target)
            )),
            None => lines.push(self.raise_blocked_reason()),
        }
        lines
    }

    /// Why no raise is offered for this route.
    ///
    /// Every window is labelled, so the reader never has to guess which number
    /// is the deliberate cap and which is the plan's own entitlement ceiling.
    pub fn raise_blocked_reason(&self) -> String {
        let label = crate::codex_context::context_window_label;
        let working = label(crate::codex_context::working_context_window(&self.model_id));
        if self.entitled_max_window <= crate::codex_context::working_context_window(&self.model_id)
        {
            format!(
                "no raise is available for {}: the deliberate {working} window is already this model's entitlement ceiling",
                self.model_id
            )
        } else {
            format!(
                "raising above the deliberate {working} window requires a Codex Pro or ProLite plan"
            )
        }
    }

    /// Validate one raise request through the shared Codex context-window
    /// policy. Fail closed: an unacknowledged or above-entitlement request is
    /// refused and the deliberate cap stays in force.
    pub fn raise(&self, requested: u64, acknowledged: bool) -> Result<u64, String> {
        let (fallback_default, fallback_max) =
            crate::codex_context::entitled_context_windows(&self.model_id);
        crate::codex_context::resolve_codex_context_window(
            &self.model_id,
            CodexContextTier::from_plan_entitlement(self.entitled),
            fallback_default,
            fallback_max,
            None,
            crate::codex_context::CodexContextOverride::raising(requested, acknowledged),
        )
        .map(|resolved| resolved.context_window)
        .map_err(|error| error.to_string())
    }

    /// The exact launch settings that actually apply a raise.
    ///
    /// The Codex context window is resolved once at launch from the process
    /// environment, so no in-session selection can change the live catalog.
    pub fn raise_instruction(&self, requested: u64) -> String {
        format!(
            "launch with `--codex-context-window {requested} --codex-context-window-acknowledge-cost-cliff`, or set {}=1 and {}={requested} (takes effect on the next launch)",
            crate::codex_context::CODEX_CONTEXT_ACKNOWLEDGE_ENV,
            crate::codex_context::CODEX_CONTEXT_OVERRIDE_ENV,
        )
    }
}

/// Model label used when reporting a rejected `/fast`.
pub fn model_route_label(model: &Model) -> &str {
    model
        .spec
        .display_name
        .as_deref()
        .unwrap_or(&model.spec.api_name)
}

/// Suggestions for an editor value while its first token is a slash command.
pub fn slash_suggestions(input: &str) -> Vec<&'static SlashCommandSuggestion> {
    let Some(query) = input.strip_prefix('/') else {
        return Vec::new();
    };
    if query.contains(char::is_whitespace) || query.contains('\n') {
        return Vec::new();
    }
    let direct = slash_commands()
        .iter()
        .filter(|command| command.name.starts_with(query));
    // Keep the user's typed command first: Enter selects the highlighted row.
    // The related auth/setup action is discoverable, but never steals the
    // default selection from a direct prefix match.
    let related = slash_commands().iter().filter(|command| {
        matches!(command.name, "login" | "setup")
            && !command.name.starts_with(query)
            && ["login", "setup"]
                .iter()
                .any(|name| name.starts_with(query) && *name != query)
    });
    direct.chain(related).collect()
}

/// Complete a unique command-name prefix. Argument-taking commands receive a
/// trailing space so the next keystroke naturally begins their argument.
#[cfg(test)]
pub fn complete_slash_command(input: &str) -> Option<String> {
    let suggestions = slash_suggestions(input);
    let [command] = suggestions.as_slice() else {
        return None;
    };
    Some(format!(
        "/{}{}",
        command.name,
        if command.accepts_argument { " " } else { "" }
    ))
}

fn parse_on_off(value: &str) -> Option<bool> {
    match value {
        "on" | "true" | "yes" => Some(true),
        "off" | "false" | "no" => Some(false),
        _ => None,
    }
}

fn parse_goal_command(argument: &str) -> GoalCommand {
    let argument = argument.trim();
    if argument.is_empty() {
        return GoalCommand::Status;
    }

    let mut words = argument.splitn(2, char::is_whitespace);
    let first = words.next().unwrap_or_default();
    let rest = words.next().map(str::trim).unwrap_or_default();
    match first.to_ascii_lowercase().as_str() {
        "status" if rest.is_empty() => GoalCommand::Status,
        "pause" if rest.is_empty() => GoalCommand::Pause,
        "resume" if rest.is_empty() => GoalCommand::Resume,
        "clear" if rest.is_empty() => GoalCommand::Clear,
        "help" if rest.is_empty() => GoalCommand::Help,
        "set" if !rest.is_empty() => GoalCommand::Set(rest.to_owned()),
        "set" => GoalCommand::Help,
        _ => GoalCommand::Set(argument.to_owned()),
    }
}

/// Parse a slash command without interpreting models, paths, or capabilities.
pub fn parse(input: &str) -> Command {
    let input = input.trim();
    if let Some(escape) = BashEscape::parse(input) {
        return Command::Bash(escape);
    }
    let Some(body) = input.strip_prefix('/') else {
        return Command::Unknown(input.to_owned());
    };
    let mut parts = body.split_whitespace();
    let name = parts.next().unwrap_or_default();

    let matches: Vec<_> = SLASH_COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(name))
        .collect();
    let full_name = if SLASH_COMMANDS.iter().any(|command| command.name == name) {
        name
    } else if HIDDEN_COMMANDS.contains(&name) {
        // Accepted by exact name only: hidden commands are never advertised and
        // never resolved by prefix, so the popup cannot reveal them.
        name
    } else if let [command] = matches.as_slice() {
        command.name
    } else {
        return Command::Unknown(input.to_owned());
    };

    // Resolve the command name before parsing the variable-arity `/skills`
    // arguments, so a future command sharing its prefix remains ambiguous.
    if full_name == "skills" {
        let args: Vec<&str> = parts.collect();
        let sub = match args.as_slice() {
            [] | ["list"] => SkillsSubcommand::List,
            ["active"] => SkillsSubcommand::Active,
            ["show", id] => SkillsSubcommand::Show(id.to_string()),
            ["search", query @ ..] if !query.is_empty() => {
                SkillsSubcommand::Search(query.join(" "))
            }
            ["load", id] | ["reload", id] => SkillsSubcommand::Load(id.to_string()),
            ["reload"] => SkillsSubcommand::Reload,
            ["off", id] | ["unload", id] => SkillsSubcommand::Off(id.to_string()),
            _ => return Command::Unknown(input.to_owned()),
        };
        return Command::Skills(sub);
    }

    if full_name == "prompt" {
        let argument = body[name.len()..].trim();
        return Command::Prompt((!argument.is_empty()).then(|| argument.to_owned()));
    }

    if full_name == "compact" {
        let instructions = body[name.len()..].trim();
        return if instructions.is_empty() {
            Command::Compact
        } else {
            Command::CompactWithInstructions(instructions.to_owned())
        };
    }

    if full_name == "answer" {
        let argument = body[name.len()..].trim();
        return Command::Answer((!argument.is_empty()).then(|| argument.to_owned()));
    }

    if full_name == "name" || full_name == "export" {
        let argument = body[name.len()..].trim();
        let argument = (!argument.is_empty()).then(|| argument.to_owned());
        return if full_name == "name" {
            Command::Name(argument)
        } else {
            Command::Export(argument)
        };
    }

    if full_name == "extensions" {
        let args = parts.collect::<Vec<_>>();
        return match args.as_slice() {
            [] | ["list"] => Command::Extensions(ExtensionsSubcommand::Menu),
            ["status"] => Command::Extensions(ExtensionsSubcommand::Status),
            ["reload"] => Command::Extensions(ExtensionsSubcommand::Reload),
            ["inspect", reference] => Command::Extensions(ExtensionsSubcommand::Inspect {
                reference: (*reference).to_owned(),
            }),
            ["action", extension, action] => Command::Extensions(ExtensionsSubcommand::Action {
                extension: (*extension).to_owned(),
                action: (*action).to_owned(),
            }),
            _ => Command::Unknown(input.to_owned()),
        };
    }

    if full_name == "goal" {
        let argument = body[name.len()..].trim();
        return Command::Goal(parse_goal_command(argument));
    }

    if full_name == "settings" {
        let args = parts.collect::<Vec<_>>();
        return match args.as_slice() {
            [] => Command::Settings(SettingsCommand::Show),
            ["theme"] => Command::Settings(SettingsCommand::Theme(None)),
            ["theme", value] => {
                Command::Settings(SettingsCommand::Theme(Some((*value).to_owned())))
            }
            ["images"] => Command::Settings(SettingsCommand::Images(None)),
            ["images", value] => match parse_on_off(value) {
                Some(enabled) => Command::Settings(SettingsCommand::Images(Some(enabled))),
                None => Command::Unknown(input.to_owned()),
            },
            ["default", "model"] => Command::Settings(SettingsCommand::DefaultModel(None)),
            ["default", "model", id] => {
                Command::Settings(SettingsCommand::DefaultModel(Some((*id).to_owned())))
            }
            ["default", "reasoning"] => Command::Settings(SettingsCommand::DefaultReasoning(None)),
            ["default", "reasoning", level] => {
                Command::Settings(SettingsCommand::DefaultReasoning(Some((*level).to_owned())))
            }
            ["transport"] => Command::Settings(SettingsCommand::Transport),
            ["padding"] => Command::Settings(SettingsCommand::Padding),
            _ => Command::Unknown(input.to_owned()),
        };
    }

    if full_name == "scoped-models" {
        let args = parts.collect::<Vec<_>>();
        return match args.as_slice() {
            [] | ["status"] => Command::ScopedModels(ScopedModelsCommand::Show),
            ["all"] => Command::ScopedModels(ScopedModelsCommand::All),
            ["clear"] | ["off"] => Command::ScopedModels(ScopedModelsCommand::Clear),
            ["enable", target] => {
                Command::ScopedModels(ScopedModelsCommand::Enable((*target).to_owned()))
            }
            ["disable", target] => {
                Command::ScopedModels(ScopedModelsCommand::Disable((*target).to_owned()))
            }
            ["toggle", target] => {
                Command::ScopedModels(ScopedModelsCommand::Toggle((*target).to_owned()))
            }
            ["move", model, direction] => match ScopeMove::parse(direction) {
                Some(direction) => Command::ScopedModels(ScopedModelsCommand::Move {
                    model: (*model).to_owned(),
                    direction,
                }),
                None => Command::Unknown(input.to_owned()),
            },
            _ => Command::Unknown(input.to_owned()),
        };
    }

    if full_name == "help" {
        let args = parts.collect::<Vec<_>>();
        return match args.as_slice() {
            [] => Command::Help(None),
            [topic] => Command::Help(Some((*topic).to_owned())),
            _ => Command::Unknown(input.to_owned()),
        };
    }

    let argument = parts.next().map(str::to_owned);
    if parts.next().is_some() {
        return Command::Unknown(input.to_owned());
    }

    match full_name {
        "login" => Command::Login(argument),
        "setup" if argument.is_none() => Command::Setup,
        "logout" => Command::Logout(argument),
        "model" => Command::Model(argument),
        "thinking" => Command::Thinking(argument),
        "theme" => Command::Theme(argument),
        "verbose" => match argument.as_deref() {
            None => Command::Verbose(None),
            Some("on" | "true" | "yes") => Command::Verbose(Some(true)),
            Some("off" | "false" | "no") => Command::Verbose(Some(false)),
            Some(_) => Command::Unknown(input.to_owned()),
        },
        "fast" => match argument.as_deref() {
            None | Some("status") => Command::Fast(None),
            Some("on" | "true" | "yes") => Command::Fast(Some(true)),
            Some("off" | "false" | "no") => Command::Fast(Some(false)),
            Some(_) => Command::Unknown(input.to_owned()),
        },
        "auto-compact" => match argument.as_deref() {
            None => Command::AutoCompact(None),
            Some("on" | "true" | "yes") => {
                Command::AutoCompact(Some(AutoCompactSetting::Mode(CompactionMode::Local)))
            }
            Some("off" | "false" | "no") => {
                Command::AutoCompact(Some(AutoCompactSetting::Mode(CompactionMode::Disabled)))
            }
            Some("local") => {
                Command::AutoCompact(Some(AutoCompactSetting::Mode(CompactionMode::Local)))
            }
            Some("native" | "native-responses" | "native_responses" | "responses") => {
                Command::AutoCompact(Some(AutoCompactSetting::Mode(
                    CompactionMode::NativeResponses,
                )))
            }
            Some(value) => value
                .strip_suffix('%')
                .and_then(|percent| percent.parse::<u8>().ok())
                .filter(|percent| (1..=100).contains(percent))
                .map(|percent| {
                    Command::AutoCompact(Some(AutoCompactSetting::ThresholdPercent(percent)))
                })
                .unwrap_or_else(|| Command::Unknown(input.to_owned())),
        },
        "reload" if argument.is_none() => Command::Reload,
        "new" if argument.is_none() => Command::New,
        "resume" => Command::Resume(argument),
        "fork" if argument.is_none() => Command::Fork,
        "clone" if argument.is_none() => Command::Clone,
        "status" if argument.is_none() => Command::Status,
        "context" if argument.is_none() => Command::Context,
        "cost" if argument.is_none() => Command::Cost,
        "cache" if argument.is_none() => Command::Cache,
        "hotkeys" if argument.is_none() => Command::Hotkeys,
        "copy" if argument.is_none() => Command::Copy,
        "session" if matches!(argument.as_deref(), None | Some("info")) => Command::Session,
        "update" if argument.is_none() => Command::Update,
        "changelog" if argument.is_none() => Command::Changelog,
        "exit" if argument.is_none() => Command::Exit,
        "debug" if argument.is_none() => Command::Debug,
        _ => Command::Unknown(input.to_owned()),
    }
}

/// Render a capability gate as an explicit enabled/disabled word rather than a
/// bare boolean, so `/status` reads as a security report.
fn gate(enabled: bool) -> &'static str {
    if enabled {
        "enabled"
    } else {
        "disabled"
    }
}

fn path_access(allow_external_paths: bool) -> &'static str {
    if allow_external_paths {
        "current-user paths (absolute, ~/ and relative)"
    } else {
        "workspace-only guard"
    }
}

fn effect_policy(policy: octet_agent::EffectPolicy) -> &'static str {
    match policy {
        octet_agent::EffectPolicy::Controlled => {
            "controlled (workspace mutation and unsafe bash calls need approval; other ambient host effects denied)"
        }
        octet_agent::EffectPolicy::ControlledBashApproval => {
            "controlled (all bash calls need approval; other ambient host effects denied)"
        }
        octet_agent::EffectPolicy::UnsafeHost => {
            "full access (classified effects use ambient OS authority)"
        }
    }
}

fn session_activity_counts(session: &octet_agent::Session) -> (usize, usize) {
    let mut model_turns = 0usize;
    let mut tool_calls = 0usize;
    let mut cursor = session.head();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(&id) else {
            break;
        };
        if let EntryValue::Message(Message::Assistant(message)) = &entry.value {
            model_turns = model_turns.saturating_add(1);
            tool_calls = tool_calls.saturating_add(
                message
                    .content
                    .iter()
                    .filter(|part| matches!(part, AssistantPart::ToolCall(_)))
                    .count(),
            );
        }
        cursor = entry.parent.clone();
    }
    (model_turns, tool_calls)
}

fn token_count(value: u64) -> String {
    if value >= 1_000 {
        let thousands = value as f64 / 1_000.0;
        format!("{thousands:.1}k")
    } else {
        value.to_string()
    }
}

/// Render an exact microdollar amount with enough precision to make small
/// requests visible in a report.
pub fn format_microdollars(value: u64) -> String {
    format!("${}.{:06}", value / 1_000_000, value % 1_000_000)
}

/// Render a spend limit in ordinary dollars, rounded to cents.
pub fn format_microdollars_cents(value: u64) -> String {
    let cents = value.saturating_add(5_000) / 10_000;
    format!("${}.{:02}", cents / 100, cents % 100)
}

/// Present the active model's base input/output/cache-read rates.
pub fn model_pricing_text(model: &Model) -> String {
    match model.spec.pricing.as_ref() {
        Some(pricing) => format!(
            "{}/{}/{} (input/output/cache-read; cache-write {})",
            format_token_rate(pricing.input),
            format_token_rate(pricing.output),
            format_token_rate(pricing.cache_read),
            format_token_rate(pricing.cache_write_5m),
        ),
        None => "unavailable (no configured rates)".to_owned(),
    }
}

fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut output = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            output.push(',');
        }
        output.push(character);
    }
    output
}

fn usage_cost_cell(tokens: u64, cost: Option<u64>) -> String {
    format!(
        "{}/{}",
        grouped(tokens),
        cost.map(|cost| cost.to_string())
            .unwrap_or_else(|| "—".to_owned())
    )
}

fn add_usage(total: &mut Usage, turn: Usage) {
    total.input_tokens = total.input_tokens.saturating_add(turn.input_tokens);
    total.cache_read_tokens = total
        .cache_read_tokens
        .saturating_add(turn.cache_read_tokens);
    total.cache_write_tokens = total
        .cache_write_tokens
        .saturating_add(turn.cache_write_tokens);
    total.cache_write_1h_tokens = total
        .cache_write_1h_tokens
        .saturating_add(turn.cache_write_1h_tokens);
    total.output_tokens = total.output_tokens.saturating_add(turn.output_tokens);
    total.reasoning_tokens = total.reasoning_tokens.saturating_add(turn.reasoning_tokens);
    total.total_tokens = total.total_tokens.saturating_add(turn.total_tokens);
}

/// Session facts from the active durable branch and the session-global ledger.
/// Read-only and usable while a Run owns the writer through a read-only reopen.
/// Diagnostics report written by `/debug`, mirroring the reference layout: the
/// terminal size, every rendered line with its visible width, then the agent
/// messages as JSONL. Bounded by the caller's frame and session sizes.
pub fn debug_report_text(
    terminal: Option<(u16, u16)>,
    rendered: Option<&[String]>,
    messages: &[octet_ai::Message],
) -> String {
    let mut report = String::from("octet debug output\n");
    match terminal {
        Some((columns, rows)) => {
            report.push_str(&format!("Terminal: {columns}x{rows}\n"));
        }
        None => report.push_str("Terminal: unknown\n"),
    }
    match rendered {
        Some(lines) => {
            report.push_str(&format!("Total lines: {}\n\n", lines.len()));
            report.push_str("=== All rendered lines with visible widths ===\n");
            for (index, line) in lines.iter().enumerate() {
                report.push_str(&format!(
                    "[{index}] (w={}) {}\n",
                    sexy_tui_rs::visible_width(line),
                    serde_json::Value::String(line.clone())
                ));
            }
        }
        None => {
            report.push_str("Total lines: unavailable (no renderer frame)\n\n");
            report.push_str("=== All rendered lines with visible widths ===\n");
        }
    }
    report.push_str("\n=== Agent messages (JSONL) ===\n");
    for message in messages {
        match serde_json::to_string(message) {
            Ok(encoded) => report.push_str(&encoded),
            Err(_) => report.push_str("{\"error\":\"unencodable message\"}"),
        }
        report.push('\n');
    }
    report
}

/// Maximum bytes retained from one local shell escape's command or output in
/// the durable record. The live transcript keeps the complete bounded capture;
/// the durable record stays small enough that one command cannot dominate the
/// model context.
pub const SHELL_ESCAPE_RECORD_BYTES: usize = 32 * 1024;

/// One finished local shell escape (`!command` / `!!command`).
///
/// `!command` results are appended as an ordinary user message so the next
/// model turn sees them; `!!command` results use a non-model-visible session
/// entry, so the execution is still durably accounted for while the model
/// context stays exactly as the user requested.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellEscapeRecord {
    command: String,
    output: String,
    exit_code: i32,
    /// `true` for `!!command`: this record never enters model context.
    excluded: bool,
}

impl ShellEscapeRecord {
    /// Bound one shell result for the durable record.
    pub fn new(
        command: impl Into<String>,
        output: impl Into<String>,
        exit_code: i32,
        excluded: bool,
    ) -> Self {
        Self {
            command: bounded_record_text(&command.into(), SHELL_ESCAPE_RECORD_BYTES),
            output: bounded_record_text(&output.into(), SHELL_ESCAPE_RECORD_BYTES),
            exit_code,
            excluded,
        }
    }

    #[cfg(test)]
    pub fn output(&self) -> &str {
        &self.output
    }

    #[cfg(test)]
    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    pub fn excluded(&self) -> bool {
        self.excluded
    }

    /// Prefix the reader typed, so the transcript and the record agree.
    pub fn prefix(&self) -> &'static str {
        if self.excluded {
            "!!"
        } else {
            "!"
        }
    }

    /// Model-visible text for an included (`!`) result. Never used for `!!`.
    pub fn context_text(&self) -> String {
        format!(
            "$ {}\nexit {}\n{}",
            self.command,
            self.exit_code,
            self.output.trim_end()
        )
    }

    /// Transcript/accounting text; the exclusion decision is explicit so a
    /// later reader cannot mistake an excluded result for model context.
    pub fn transcript_text(&self) -> String {
        format!(
            "{}$ {}\nexit {}\n{}{}",
            self.prefix(),
            self.command,
            self.exit_code,
            if self.excluded {
                "[excluded from model context]\n"
            } else {
                ""
            },
            self.output.trim_end()
        )
    }
}

fn bounded_record_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    const MARKER: &str = "\n... truncated for the durable record ...";
    let mut end = limit.saturating_sub(MARKER.len()).min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = value[..end].to_owned();
    bounded.push_str(MARKER);
    bounded
}

/// Readable label for the endpoint's declared streaming transport.
pub fn transport_label(model: &Model) -> &'static str {
    match model.endpoint.transport {
        octet_ai::EndpointTransport::Http => "http",
        octet_ai::EndpointTransport::WebSocketPreferred => "websocket-preferred",
    }
}

/// Effective values for the `/settings` surface.
///
/// Captured as plain facts so the active-run path can render the same report
/// without borrowing the application that the run owns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsSurface {
    pub default_model: Option<String>,
    pub reasoning: String,
    pub theme: Option<String>,
    pub transport: &'static str,
    pub endpoint: String,
    pub show_images: bool,
}

impl SettingsSurface {
    /// Capture the effective settings from the application.
    pub fn capture(app: &App) -> Self {
        Self {
            default_model: app.config.model.as_ref().map(|model| model.0.clone()),
            reasoning: reasoning_label(&app.reasoning),
            theme: app.config.theme.clone(),
            transport: transport_label(&app.model),
            endpoint: app.model.endpoint.id.0.clone(),
            show_images: app.config.show_images,
        }
    }
}

/// Effective `/settings` summary.
///
/// Every line is a fact the running product actually holds. Transport and
/// editor padding are route/theme declarations rather than persisted user
/// preferences, and project trust is deliberately not a setting at all.
pub fn settings_text(surface: &SettingsSurface) -> String {
    let default_model = surface
        .default_model
        .as_deref()
        .unwrap_or("(chosen at startup or by the session)");
    format!(
        "octet settings\n\n\
         Default model      {default_model}\n\
         Default reasoning  {}\n\
         Theme              {}\n\
         Transport          {} (declared by the {} route; not a user preference)\n\
         Inline images      {}\n\
         Editor padding     compiled theme layout (no persisted override)\n\n\
         Project trust is deliberately not persisted here: workspace trust comes from --workspace-trusted or the interactive startup prompt.\n\n\
         Change: /settings theme <auto|light|dark> · /settings images <on|off> · /settings default model <id> · /settings default reasoning <level>",
        surface.reasoning,
        surface.theme.as_deref().unwrap_or("auto"),
        surface.transport,
        surface.endpoint,
        if surface.show_images { "on" } else { "off" },
    )
}

/// Render the ordered cycling scope and what a persistence round-trip keeps.
pub fn scoped_models_text(scope: Option<&[ScopedModel]>, available: &[(String, String)]) -> String {
    let mut text = String::from("Model cycling scope\n");
    match scope {
        None => text.push_str(&format!(
            "  (unrestricted) all {} available models cycle in catalog order\n",
            available.len()
        )),
        Some([]) => text.push_str("  (empty) no model is a cycling target\n"),
        Some(scope) => {
            text.push_str(&format!(
                "  {} of {} available models, in this exact order:\n",
                scope.len(),
                available.len()
            ));
            for (index, entry) in scope.iter().enumerate() {
                let level = entry
                    .reasoning
                    .as_deref()
                    .map(|level| format!(":{level}"))
                    .unwrap_or_default();
                let availability = if available.iter().any(|(id, _)| id == &entry.id.0) {
                    ""
                } else {
                    " (unavailable on this route)"
                };
                text.push_str(&format!(
                    "  {:>2}. {}{level}{availability}\n",
                    index + 1,
                    entry.id.0,
                ));
            }
        }
    }
    text.push_str(
        "\nUse /scoped-models enable <id|provider/*>, disable <id|provider/*>, toggle <...>,\n\
         move <id> <up|down|top|bottom>, all, or clear. Cycling bindings follow this order.",
    );
    text
}

/// Persisted comma-separated pattern list for one ordered scope. `None` means
/// "clear the persisted scope", which is distinct from an empty scope.
pub fn scope_patterns_string(scope: &[ScopedModel]) -> Option<String> {
    (!scope.is_empty()).then(|| {
        scope
            .iter()
            .map(|entry| match entry.reasoning.as_deref() {
                Some(level) => format!("{}:{level}", entry.id.0),
                None => entry.id.0.clone(),
            })
            .collect::<Vec<_>>()
            .join(",")
    })
}

/// Enable or disable every model a model-id or provider glob selects.
/// Returns how many entries changed.
pub fn set_scope_target(
    scope: &mut Vec<ScopedModel>,
    available: &[(String, String)],
    target: &str,
    enable: bool,
) -> Result<usize, String> {
    let matched = available
        .iter()
        .filter(|(id, provider)| crate::cli::parity::model_pattern_matches(target, id, provider))
        .collect::<Vec<_>>();
    if matched.is_empty() {
        return Err(format!(
            "no available model matches {target:?}; run /model to list the credential-scoped catalog"
        ));
    }
    if enable {
        let mut added = 0usize;
        for (id, _) in matched {
            if !scope.iter().any(|entry| entry.id.0 == id.as_str()) {
                scope.push(ScopedModel {
                    id: ModelId(id.clone()),
                    pattern: id.clone(),
                    reasoning: None,
                });
                added += 1;
            }
        }
        Ok(added)
    } else {
        let targets = matched
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>();
        let before = scope.len();
        scope.retain(|entry| !targets.contains(&entry.id.0.as_str()));
        Ok(before.saturating_sub(scope.len()))
    }
}

/// Enable the target unless every matching model is already in the scope, in
/// which case disable it. Returns whether the target is now enabled.
pub fn toggle_scope_target(
    scope: &mut Vec<ScopedModel>,
    available: &[(String, String)],
    target: &str,
) -> Result<bool, String> {
    let all_enabled = available
        .iter()
        .filter(|(id, provider)| crate::cli::parity::model_pattern_matches(target, id, provider))
        .all(|(id, _)| scope.iter().any(|entry| entry.id.0 == id.as_str()));
    set_scope_target(scope, available, target, !all_enabled)?;
    Ok(!all_enabled)
}

/// Move one scoped model inside the ordered scope. A boundary move is an
/// explicit error, never a silent no-op.
pub fn move_scope_model(
    scope: &mut Vec<ScopedModel>,
    model: &str,
    direction: ScopeMove,
) -> Result<(), String> {
    let index = scope
        .iter()
        .position(|entry| entry.id.0 == model)
        .or_else(|| {
            scope
                .iter()
                .position(|entry| entry.id.0.eq_ignore_ascii_case(model))
        })
        .ok_or_else(|| {
            format!("{model:?} is not in the current scope; enable it before moving it")
        })?;
    match direction {
        ScopeMove::Up if index == 0 => Err(format!("{model:?} is already first")),
        ScopeMove::Up => {
            scope.swap(index, index - 1);
            Ok(())
        }
        ScopeMove::Down if index + 1 == scope.len() => Err(format!("{model:?} is already last")),
        ScopeMove::Down => {
            scope.swap(index, index + 1);
            Ok(())
        }
        ScopeMove::Top if index == 0 => Err(format!("{model:?} is already first")),
        ScopeMove::Top => {
            let entry = scope.remove(index);
            scope.insert(0, entry);
            Ok(())
        }
        ScopeMove::Bottom if index + 1 == scope.len() => Err(format!("{model:?} is already last")),
        ScopeMove::Bottom => {
            let entry = scope.remove(index);
            scope.push(entry);
            Ok(())
        }
    }
}

pub fn session_text(session: &Session) -> String {
    let mut messages = 0usize;
    let mut cursor = session.head();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(&id) else {
            break;
        };
        messages += usize::from(matches!(entry.value, EntryValue::Message(_)));
        cursor = entry.parent.clone();
    }
    let mut usage = Usage::default();
    for record in session.usage_records() {
        add_usage(&mut usage, record.usage);
    }
    let id = session
        .path()
        .file_stem()
        .and_then(|id| id.to_str())
        .unwrap_or("(unknown)");
    let head = session
        .head()
        .map(|id| id.0)
        .unwrap_or_else(|| "(empty)".into());
    format!(
        "Session: {id}\nFile: {}\nTitle: {}\nHead: {head}\nEntries: {}\nActive-branch messages: {messages}\nCheckpoints: {}\nUsage records: {}\nTokens: {} input · {} cache-read · {} cache-write · {} output\nCost: {}{}",
        session.path().display(), active_branch_title(session), session.entries().len(),
        session.checkpoints().len(), session.usage_records().len(),
        usage.input_tokens, usage.cache_read_tokens,
        usage.cache_write_tokens.saturating_add(usage.cache_write_1h_tokens), usage.output_tokens,
        format_microdollars(session.total_cost_microdollars()),
        if session.has_uncertain_usage() || session.has_unpriced_usage() { " (known subtotal only; usage or pricing uncertain)" } else { "" },
    )
}

/// Detailed cumulative spend report. Usage records are durable and therefore
/// this formatter works identically for a live or replayed session.
pub fn cost_text(session: &Session, model: &Model) -> String {
    let records = session.usage_records();
    let turn_count = records
        .iter()
        .filter(|record| matches!(record.kind, UsageRecordKind::AssistantTurn { .. }))
        .count();
    let mut lines = vec![format!(
        "Session cost · {} across {turn_count} turn{}",
        format_microdollars(session.total_cost_microdollars()),
        if turn_count == 1 { "" } else { "s" }
    )];
    if session.has_uncertain_usage() || session.has_unpriced_usage() {
        lines.push("Known subtotal only: usage or pricing is uncertain; this is not complete session spend.".to_owned());
    }
    if records.is_empty() {
        lines.push("".to_owned());
        lines.push("No completed priced model calls yet.".to_owned());
    } else {
        lines.extend([
            "".to_owned(),
            "  Turn  Model             Input tok/µ$  CacheR tok/µ$  CacheW tok/µ$  Output tok/µ$  Reason tok/µ$  Total µ$"
                .to_owned(),
            "  ────  ────────────────  ───────────  ─────────────  ─────────────  ─────────────  ─────────────  ────────"
                .to_owned(),
        ]);
    }

    let mut assistant_turn = 0usize;
    let mut totals = octet_ai::Usage::default();
    let mut cost_totals = Cost::default();
    let mut has_priced_record = false;
    for record in records {
        add_usage(&mut totals, record.usage);
        if let Some(cost) = record.cost {
            has_priced_record = true;
            cost_totals.input = cost_totals.input.saturating_add(cost.input);
            cost_totals.cache_read = cost_totals.cache_read.saturating_add(cost.cache_read);
            cost_totals.cache_write = cost_totals.cache_write.saturating_add(cost.cache_write);
            cost_totals.output = cost_totals.output.saturating_add(cost.output);
            cost_totals.reasoning = cost_totals.reasoning.saturating_add(cost.reasoning);
        }
        let turn = match record.kind {
            UsageRecordKind::AssistantTurn { .. } => {
                assistant_turn += 1;
                assistant_turn.to_string()
            }
            UsageRecordKind::DelegatedAgent { ref agent_id, .. } => {
                format!("sub:{}", agent_id.trim_start_matches("agent-"))
            }
            UsageRecordKind::Compaction => "cmp".to_owned(),
            UsageRecordKind::CacheWarm => "warm".to_owned(),
            UsageRecordKind::RejectedResponsesTurn => "rejected".to_owned(),
            UsageRecordKind::TerminalGate { returned } => match returned {
                Some(true) => "gate:R".to_owned(),
                Some(false) => "gate:C".to_owned(),
                None => "gate:?".to_owned(),
            },
        };
        let model_name = record
            .model
            .as_ref()
            .map(|model| model.0.as_str())
            .unwrap_or("unknown");
        let model_name = model_name
            .chars()
            .rev()
            .take(16)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<String>();
        let output = record
            .usage
            .output_tokens
            .saturating_sub(record.usage.reasoning_tokens);
        let cost = record.cost;
        let total_cost = record
            .cost_microdollars
            .map(|cost| cost.to_string())
            .unwrap_or_else(|| "—".to_owned());
        lines.push(format!(
            "  {turn:<4}  {model_name:<16}  {:>12}  {:>14}  {:>14}  {:>14}  {:>14}  {:>8}",
            usage_cost_cell(record.usage.input_tokens, cost.map(|cost| cost.input)),
            usage_cost_cell(
                record.usage.cache_read_tokens,
                cost.map(|cost| cost.cache_read)
            ),
            usage_cost_cell(
                record.usage.cache_write_tokens,
                cost.map(|cost| cost.cache_write)
            ),
            usage_cost_cell(output, cost.map(|cost| cost.output)),
            usage_cost_cell(
                record.usage.reasoning_tokens,
                cost.map(|cost| cost.reasoning)
            ),
            total_cost,
        ));
    }
    if !records.is_empty() {
        let output = totals.output_tokens.saturating_sub(totals.reasoning_tokens);
        lines.extend([
            "  ────  ────────────────  ──────  ──────  ──────  ──────  ──────  ────────".to_owned(),
            format!(
                "  Total                   {:>12}  {:>14}  {:>14}  {:>14}  {:>14}  {:>8}",
                usage_cost_cell(
                    totals.input_tokens,
                    has_priced_record.then_some(cost_totals.input),
                ),
                usage_cost_cell(
                    totals.cache_read_tokens,
                    has_priced_record.then_some(cost_totals.cache_read),
                ),
                usage_cost_cell(
                    totals.cache_write_tokens,
                    has_priced_record.then_some(cost_totals.cache_write),
                ),
                usage_cost_cell(output, has_priced_record.then_some(cost_totals.output)),
                usage_cost_cell(
                    totals.reasoning_tokens,
                    has_priced_record.then_some(cost_totals.reasoning),
                ),
                format_microdollars(session.total_cost_microdollars()),
            ),
        ]);
    }
    lines.extend([
        "".to_owned(),
        format!("Model: {} · {}", model.spec.id.0, model_pricing_text(model)),
    ]);
    lines.join("\n")
}

/// Prompt-cache effectiveness and material miss report for the active branch.
pub fn cache_text(session: &Session) -> String {
    let (stats, misses) = analyze_session_cache(session);
    let hit_rate = stats.hit_rate_basis_points();
    let model_changes = misses.iter().filter(|miss| miss.model_changed).count();
    let idle_timeouts = misses
        .iter()
        .filter(|miss| miss.idle_past_short_ttl)
        .count();
    let hit_summary = hit_rate
        .map(|basis_points| {
            format!(
                "{basis_points} bp ({:.1}%) across {} analyzable turns",
                f64::from(basis_points) / 100.0,
                stats.assistant_turns
            )
        })
        .unwrap_or_else(|| {
            format!(
                "unavailable (no cache activity observed) across {} turns",
                stats.assistant_turns
            )
        });
    let mut lines = vec![
        format!("Cache effectiveness · {hit_summary}"),
        format!(
            "Reusable prefix: {} tokens · missed: {} tokens · estimated waste: {}",
            grouped(stats.reusable_prefix_tokens),
            grouped(stats.missed_reusable_tokens),
            format_microdollars(stats.missed_cost_microdollars)
        ),
        "".to_owned(),
    ];
    if !misses.is_empty() {
        lines.extend([
            "  Assistant entry    Expected  Cached  Missed  Waste      Cause".to_owned(),
            "  ────────────────  ────────  ──────  ──────  ─────────  ─────────────────".to_owned(),
        ]);
        for miss in misses {
            let mut causes = Vec::new();
            if miss.model_changed {
                causes.push("model changed");
            }
            if miss.idle_past_short_ttl {
                causes.push("idle timeout");
            }
            if causes.is_empty() {
                causes.push("cache miss");
            }
            lines.push(format!(
                "  {:<16}  {:>8}  {:>6}  {:>6}  {:>9}  {}",
                miss.assistant.0,
                grouped(miss.expected_reusable_tokens),
                grouped(miss.cache_read_tokens),
                grouped(miss.missed_reusable_tokens),
                miss.missed_cost_microdollars
                    .map(format_microdollars)
                    .unwrap_or_else(|| "—".to_owned()),
                causes.join(", "),
            ));
        }
        lines.push("".to_owned());
    }
    lines.extend([
        format!(
            "Cumulative reusable prefix: {} tokens",
            grouped(stats.reusable_prefix_tokens)
        ),
        format!(
            "Total cache reads:          {} tokens",
            grouped(stats.cache_read_tokens)
        ),
        format!(
            "Material misses:            {} ({} tokens wasted)",
            stats.miss_count,
            grouped(stats.missed_reusable_tokens)
        ),
        format!(
            "Estimated waste cost:       {}",
            format_microdollars(stats.missed_cost_microdollars)
        ),
        "".to_owned(),
        format!(
            "Diagnostics: {model_changes} model-change misses, {idle_timeouts} idle-timeout misses, {} priced misses",
            stats.priced_miss_count
        ),
    ]);
    lines.join("\n")
}

/// Detailed status text suitable for the `/status` overlay.
///
/// The security block states octet's model plainly: it is a local agent, not an
/// OS sandbox. Controlled forces built-in file access to the workspace;
/// UnsafeHost may retain configured current-user path access. Neither mode
/// confines an admitted child process.
pub fn status_text(app: &App, queued: Option<&Reconfig>) -> String {
    let context_estimate = estimate_next_request_tokens(app, &[]);
    let cache_stats = analyze_session_cache_stats(app.agent.session());
    status_text_with_metrics(app, queued, context_estimate, &cache_stats)
}

pub(crate) fn status_text_with_metrics(
    app: &App,
    queued: Option<&Reconfig>,
    context_estimate: u64,
    cache_stats: &CacheStats,
) -> String {
    let session = app.agent.session();
    let session_id = session
        .path()
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("(unknown)");
    let queue = queued
        .map(|item| format!("{item:?}"))
        .unwrap_or_else(|| "none".to_owned());
    let sandbox = &app.config.sandbox;
    let (model_turns, tool_calls) = session_activity_counts(session);
    let active_skills = session
        .head_ref()
        .and_then(|head| session.resolve_active_skills(head).ok())
        .map(|state| state.active_skills.len())
        .unwrap_or(0);
    let discovered_skills = app.skills.descriptors().len();
    let context = token_count(context_estimate);
    let context_window = token_count(context_window(&app.model));
    let display = ModelDisplayMetadata::resolve(&app.model.spec);
    let provider = app
        .catalog
        .endpoint_label(&app.model.endpoint.id)
        .unwrap_or(&app.model.endpoint.id.0);
    let pricing = model_pricing_text(&app.model);
    let cache_rate = cache_stats
        .hit_rate_basis_points()
        .map(|basis_points| format!("{basis_points} bp"))
        .unwrap_or_else(|| "unavailable".to_owned());
    let cost_limit = app
        .config
        .max_cost_microdollars
        .map(format_microdollars_cents)
        .unwrap_or_else(|| "disabled".to_owned());
    let reasoning = match app.reasoning_mode {
        octet_ai::ReasoningMode::Standard => reasoning_label(&app.reasoning),
        octet_ai::ReasoningMode::Pro => format!("pro · {}", reasoning_label(&app.reasoning)),
    };
    let cost_warning = app
        .config
        .cost_warning_microdollars
        .map(format_microdollars_cents)
        .unwrap_or_else(|| "disabled".to_owned());
    format!(
        "Provider       {}\nModel          {}\nDisplay model  {}\nAPI model      {}\nEndpoint       {}\nProtocol       {:?}\nTransport      {:?}\nReasoning      {}\n{}\nPricing        {}\nContext        ~{} / {} (estimated)\n\
         Workspace      {}\nSession        {} — {}\nSession cost   {} ({})\nCost guardrails limit {} · turn warning {}\nCache hit rate  {}\nModel turns    {}\nTool calls     {}\nSkills         {} active / {} discovered\n\n\
         Extensions     {}\n\n\
         Security model: local agent with workspace trust gates\nEffect policy: {}\nBuilt-in file paths: {}\nFile edits: {}\nFile write: {}\n\
         Remote media reads: {}\nProcess execution: {}\nShell execution: {}\nOS isolation: none\n\
         Process privileges: current user\nRepository trust: {}\nQueued reconfiguration: {}",
        provider,
        app.model.spec.id.0,
        display.name,
        app.model.spec.api_name,
        app.model.endpoint.base_url,
        app.model.spec.protocol,
        app.model.endpoint.transport,
        reasoning,
        fast_status_text(&app.model, app.agent.service_tier()),
        pricing,
        context,
        context_window,
        app.config.workspace.display(),
        session_id,
        active_branch_title(session),
        if session.has_uncertain_usage() || session.has_unpriced_usage() {
            format!(
                "{} known subtotal — usage or pricing uncertain",
                format_microdollars(session.total_cost_microdollars())
            )
        } else {
            format_microdollars(session.total_cost_microdollars())
        },
        session.total_cost_microdollars(),
        cost_limit,
        cost_warning,
        cache_rate,
        model_turns,
        tool_calls,
        active_skills,
        discovered_skills,
        app.executable_extensions.status_summary(),
        effect_policy(app.config.effect_policy),
        path_access(sandbox.allow_external_paths),
        gate(sandbox.allow_edit),
        gate(sandbox.allow_write),
        gate(sandbox.allow_remote_read),
        gate(sandbox.allow_process && sandbox.allow_shell),
        gate(sandbox.allow_process && sandbox.allow_shell),
        if app.config.workspace_trusted {
            "trusted (project config/context/skills enabled)"
        } else {
            "untrusted (project config/context/skills ignored)"
        },
        queue,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Row 2d.10: `/session` is the durable-fact surface, so every field it
    /// claims must come from the session (file, id, head, message count, token
    /// buckets, cost) and unknown exposure must be named rather than folded into
    /// an exact-looking total.
    #[test]
    fn session_detail_reports_file_identity_messages_tokens_cost_and_uncertainty() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("detail-session.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("first question".into())],
            })))
            .unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(
                octet_ai::AssistantMessage {
                    content: vec![AssistantPart::Text("first answer".into())],
                    model: octet_ai::ModelId("fixture-model".into()),
                    protocol: Protocol::OpenAiChat,
                },
            )))
            .unwrap();
        let text = session_text(&session);
        assert!(text.contains("Session: detail-session"), "{text}");
        assert!(
            text.contains(&format!("File: {}", path.display())),
            "{text}"
        );
        assert!(text.contains("Head: 002"), "{text}");
        assert!(text.contains("Entries: 2"), "{text}");
        assert!(text.contains("Active-branch messages: 2"), "{text}");
        assert!(text.contains("Checkpoints: 0"), "{text}");
        assert!(
            text.contains("Tokens: 0 input · 0 cache-read · 0 cache-write · 0 output"),
            "{text}"
        );
        assert!(text.contains("Cost: $0.000000"), "{text}");
        assert!(
            !text.contains("uncertain"),
            "a fully priced, settled session is not uncertain: {text}"
        );

        // Unknown exposure must never read as an exact bill.
        session
            .record_usage_uncertainty(
                octet_ai::EndpointId("fixture-endpoint".into()),
                octet_ai::ModelId("fixture-model".into()),
                "assistant_turn",
            )
            .unwrap();
        let text = session_text(&session);
        assert!(
            text.contains("known subtotal only; usage or pricing uncertain"),
            "{text}"
        );
    }

    #[test]
    fn debug_is_hidden_but_parses_and_reports_every_rendered_line() {
        assert_eq!(parse("/debug"), Command::Debug);
        assert!(matches!(parse("/debug extra"), Command::Unknown(_)));
        // The reference hides this command from discovery; the popup must not
        // advertise it even though it is accepted.
        assert!(slash_suggestions("/deb").is_empty());
        assert!(complete_slash_command("/deb").is_none());

        let lines = vec![
            "plain".to_owned(),
            "wide 漢字".to_owned(),
            "quote\"and\\slash".to_owned(),
        ];
        let report = debug_report_text(Some((120, 40)), Some(&lines), &[]);
        assert!(report.contains("Terminal: 120x40"));
        assert!(report.contains("Total lines: 3"));
        assert!(report.contains("[1] (w=9) \"wide 漢字\""), "{report}");
        assert!(
            report.contains(r#"[2] (w=15) "quote\"and\\slash""#),
            "{report}"
        );
        assert!(report.contains("=== Agent messages (JSONL) ==="));

        let message = Message::User(octet_ai::UserMessage {
            content: vec![octet_ai::UserPart::Text("hello".into())],
        });
        let report = debug_report_text(None, None, std::slice::from_ref(&message));
        assert!(report.contains("Terminal: unknown"));
        assert!(
            report.contains("unavailable (no renderer frame)"),
            "{report}"
        );
        assert!(
            report.lines().any(|line| line.contains("\"hello\"")),
            "{report}"
        );
    }

    #[test]
    fn settings_and_scoped_models_parse_exactly_and_reject_malformed_forms() {
        assert_eq!(parse("/settings"), Command::Settings(SettingsCommand::Show));
        assert_eq!(
            parse("/settings theme"),
            Command::Settings(SettingsCommand::Theme(None))
        );
        assert_eq!(
            parse("/settings theme light"),
            Command::Settings(SettingsCommand::Theme(Some("light".into())))
        );
        assert_eq!(
            parse("/settings images off"),
            Command::Settings(SettingsCommand::Images(Some(false)))
        );
        assert_eq!(
            parse("/settings default model custom/alpha-model"),
            Command::Settings(SettingsCommand::DefaultModel(Some(
                "custom/alpha-model".into()
            )))
        );
        assert_eq!(
            parse("/settings default reasoning high"),
            Command::Settings(SettingsCommand::DefaultReasoning(Some("high".into())))
        );
        assert_eq!(
            parse("/settings transport"),
            Command::Settings(SettingsCommand::Transport)
        );
        assert_eq!(
            parse("/settings padding"),
            Command::Settings(SettingsCommand::Padding)
        );
        // Project trust is deliberately not a setting this surface can change.
        assert!(matches!(
            parse("/settings default trust always"),
            Command::Unknown(_)
        ));
        assert!(matches!(
            parse("/settings images maybe"),
            Command::Unknown(_)
        ));
        assert!(matches!(parse("/settings bogus"), Command::Unknown(_)));

        assert_eq!(
            parse("/scoped-models"),
            Command::ScopedModels(ScopedModelsCommand::Show)
        );
        assert_eq!(
            parse("/scoped-models all"),
            Command::ScopedModels(ScopedModelsCommand::All)
        );
        assert_eq!(
            parse("/scoped-models clear"),
            Command::ScopedModels(ScopedModelsCommand::Clear)
        );
        assert_eq!(
            parse("/scoped-models toggle openai/*"),
            Command::ScopedModels(ScopedModelsCommand::Toggle("openai/*".into()))
        );
        assert_eq!(
            parse("/scoped-models move gpt-6-astra top"),
            Command::ScopedModels(ScopedModelsCommand::Move {
                model: "gpt-6-astra".into(),
                direction: ScopeMove::Top,
            })
        );
        assert!(matches!(
            parse("/scoped-models move gpt-6-astra sideways"),
            Command::Unknown(_)
        ));
        // Both commands are discoverable popup entries with real parser routes.
        for name in ["settings", "scoped-models"] {
            assert!(SLASH_COMMANDS.iter().any(|command| command.name == name));
        }
    }

    #[test]
    fn shell_escape_parser_distinguishes_included_and_excluded_commands() {
        assert_eq!(
            parse("!git status"),
            Command::Bash(BashEscape {
                command: "git status".into(),
                excluded: false,
            })
        );
        assert_eq!(
            parse("  !!rm -rf build  "),
            Command::Bash(BashEscape {
                command: "rm -rf build".into(),
                excluded: true,
            })
        );
        // Multi-line commands survive verbatim.
        assert_eq!(
            parse("!printf 'a\\nb'"),
            Command::Bash(BashEscape {
                command: "printf 'a\\nb'".into(),
                excluded: false,
            })
        );
        // `!` and `!!` alone are parsed so dispatch reports usage, not silence.
        assert_eq!(
            parse("!"),
            Command::Bash(BashEscape {
                command: String::new(),
                excluded: false,
            })
        );
        assert_eq!(
            parse("!!"),
            Command::Bash(BashEscape {
                command: String::new(),
                excluded: true,
            })
        );
        // Ordinary prose with an exclamation mark is untouched.
        assert!(matches!(parse("hello, world!"), Command::Unknown(_)));
    }

    #[test]
    fn shell_escape_record_bounds_labels_and_never_leaks_excluded_text_into_context() {
        let included = ShellEscapeRecord::new("git status", "clean", 0, false);
        assert_eq!(included.prefix(), "!");
        assert!(!included.excluded());
        assert!(included.context_text().contains("$ git status"));
        assert!(included.context_text().contains("exit 0"));
        assert!(included.context_text().contains("clean"));
        assert!(!included
            .transcript_text()
            .contains("excluded from model context"));

        let excluded = ShellEscapeRecord::new("git log", "deadbeef", 1, true);
        assert_eq!(excluded.prefix(), "!!");
        assert!(excluded.excluded());
        assert!(excluded
            .transcript_text()
            .contains("[excluded from model context]"));

        // Oversized output is truncated on a character boundary and says so.
        let long = "漢".repeat(SHELL_ESCAPE_RECORD_BYTES);
        let record = ShellEscapeRecord::new("cat big", long, 0, false);
        assert!(record.output().len() <= SHELL_ESCAPE_RECORD_BYTES);
        assert!(record
            .output()
            .ends_with("truncated for the durable record ..."));
        assert!(record.output().is_char_boundary(record.output().len()));
    }

    #[test]
    fn scoped_scope_targets_toggle_move_and_persist_in_requested_order() {
        use octet_ai::ModelId;
        let available = vec![
            ("custom/alpha-model".to_owned(), "custom-openai".to_owned()),
            ("gpt-6-astra".to_owned(), "openai".to_owned()),
            ("gpt-6-luna".to_owned(), "openai".to_owned()),
        ];
        let mut scope = Vec::<ScopedModel>::new();
        // A provider glob enables every model of that provider, in catalog order.
        assert_eq!(
            set_scope_target(&mut scope, &available, "openai/*", true),
            Ok(2)
        );
        assert_eq!(
            scope
                .iter()
                .map(|entry| entry.id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-6-astra", "gpt-6-luna"]
        );
        // A toggle flips a provider selection explicitly and never silently
        // no-ops on an unmatched target.
        assert_eq!(
            toggle_scope_target(&mut scope, &available, "openai/*"),
            Ok(false)
        );
        assert!(scope.is_empty());
        assert_eq!(
            toggle_scope_target(&mut scope, &available, "openai/*"),
            Ok(true)
        );
        assert_eq!(
            set_scope_target(&mut scope, &available, "custom/*", true),
            Ok(1)
        );
        assert_eq!(
            scope
                .iter()
                .map(|entry| entry.id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-6-astra", "gpt-6-luna", "custom/alpha-model"]
        );
        // Reorder moves exactly one entry and refuses a boundary no-op.
        assert_eq!(
            move_scope_model(&mut scope, "custom/alpha-model", ScopeMove::Top),
            Ok(())
        );
        assert!(move_scope_model(&mut scope, "custom/alpha-model", ScopeMove::Up).is_err());
        assert_eq!(
            move_scope_model(&mut scope, "gpt-6-luna", ScopeMove::Up),
            Ok(())
        );
        assert_eq!(
            move_scope_model(&mut scope, "custom/alpha-model", ScopeMove::Bottom),
            Ok(())
        );
        assert_eq!(
            scope
                .iter()
                .map(|entry| entry.id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-6-luna", "gpt-6-astra", "custom/alpha-model"]
        );
        // Persisted patterns are the exact ordered id list; a level rides along.
        scope[0].reasoning = Some("high".into());
        assert_eq!(
            scope_patterns_string(&scope).as_deref(),
            Some("gpt-6-luna:high,gpt-6-astra,custom/alpha-model")
        );
        assert_eq!(scope_patterns_string(&[]), None);
        let text = scoped_models_text(Some(&scope), &available);
        assert!(text.contains("gpt-6-luna:high"), "{text}");
        assert!(
            text.contains("3 of 3 available models, in this exact order"),
            "{text}"
        );
        let mut absent = scope.clone();
        absent.push(ScopedModel {
            id: ModelId("gone".into()),
            pattern: "gone".into(),
            reasoning: None,
        });
        assert!(scoped_models_text(Some(&absent), &available)
            .contains("gone (unavailable on this route)"));
        assert!(
            scoped_models_text(None, &available).contains("(unrestricted) all 3 available models")
        );
        assert!(set_scope_target(&mut scope, &available, "nope/*", true).is_err());
    }

    #[test]
    fn settings_text_reports_defaults_theme_transport_images_and_no_trust_default() {
        let surface = SettingsSurface {
            default_model: Some("gpt-4o-mini".into()),
            reasoning: "high".into(),
            theme: Some("dark".into()),
            transport: "websocket-preferred",
            endpoint: "codex".into(),
            show_images: true,
        };
        let text = settings_text(&surface);
        for expected in [
            "Default model      gpt-4o-mini",
            "Default reasoning  high",
            "Theme              dark",
            "Transport          websocket-preferred (declared by the codex route; not a user preference)",
            "Inline images      on",
            "Editor padding     compiled theme layout (no persisted override)",
            "Project trust is deliberately not persisted here",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in {text}");
        }
        // Unset defaults are named, never rendered as an empty value.
        let empty = SettingsSurface {
            default_model: None,
            reasoning: "off".into(),
            theme: None,
            transport: "http",
            endpoint: "custom".into(),
            show_images: false,
        };
        let text = settings_text(&empty);
        assert!(
            text.contains("Default model      (chosen at startup or by the session)"),
            "{text}"
        );
        assert!(text.contains("Theme              auto"), "{text}");
        assert!(text.contains("Inline images      off"), "{text}");
    }

    #[test]
    fn changelog_parser_discovery_and_local_help() {
        for input in ["/changelog", " /changelog  ", "/chang"] {
            assert_eq!(parse(input), Command::Changelog);
            assert!(reject_tui_changelog(input)
                .unwrap_err()
                .to_string()
                .contains("interactive TUI"));
        }
        assert!(matches!(parse("/changelog extra"), Command::Unknown(_)));
        // `/checkout` was withdrawn, so this prefix is now unambiguous.
        assert_eq!(parse("/ch"), Command::Changelog);
        assert_eq!(complete_slash_command("/chang"), Some("/changelog".into()));
        let suggestions = slash_suggestions("/chang");
        assert_eq!(suggestions.len(), 1);
        assert!(!suggestions[0].accepts_argument);
        let help = help_text(Path::new("."), Some("changelog"));
        assert!(help.contains("/changelog") && help.contains("bundled release notes"));
        assert!(help_text(Path::new("."), None).contains("/changelog"));
        assert!(reject_tui_changelog("Explain the changelog").is_ok());
    }

    #[test]
    fn changelog_bundle_matches_current_version_and_canonical_source() {
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            CURRENT_CHANGELOG.lines().next(),
            Some(format!("# octet {version}").as_str())
        );
        // Published packages have no repository docs tree. The package-local
        // include above must still compile; a checkout additionally guards drift.
        let canonical = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../docs/releases/v{version}.md"));
        if canonical.is_file() {
            assert_eq!(
                CURRENT_CHANGELOG,
                std::fs::read_to_string(canonical).unwrap()
            );
        }
    }

    #[test]
    fn parses_the_complete_v1_command_grammar() {
        assert_eq!(parse("/login"), Command::Login(None));
        assert_eq!(parse("/setup"), Command::Setup);
        assert_eq!(parse("/setu"), Command::Setup);
        assert!(matches!(parse("/setup key"), Command::Unknown(_)));
        assert_eq!(
            parse("/logout openai-codex"),
            Command::Logout(Some("openai-codex".into()))
        );
        assert_eq!(
            parse("/model gpt-4o-mini"),
            Command::Model(Some("gpt-4o-mini".into()))
        );
        assert_eq!(parse("/thinking"), Command::Thinking(None));
        assert_eq!(parse("/theme"), Command::Theme(None));
        assert_eq!(parse("/theme light"), Command::Theme(Some("light".into())));
        assert!(matches!(parse("/theme neon"), Command::Theme(Some(_))));
        assert_eq!(parse("/verbose on"), Command::Verbose(Some(true)));
        assert_eq!(parse("/verbose off"), Command::Verbose(Some(false)));
        assert_eq!(parse("/answer"), Command::Answer(None));
        assert_eq!(
            parse("/answer summarize the verified findings concisely"),
            Command::Answer(Some("summarize the verified findings concisely".into()))
        );
        assert_eq!(parse("/compact"), Command::Compact);
        assert_eq!(
            parse("/compact preserve the API contract\nand test evidence"),
            Command::CompactWithInstructions("preserve the API contract\nand test evidence".into())
        );
        assert_eq!(parse("/auto-compact"), Command::AutoCompact(None));
        assert_eq!(
            parse("/auto-compact off"),
            Command::AutoCompact(Some(AutoCompactSetting::Mode(CompactionMode::Disabled)))
        );
        assert_eq!(
            parse("/auto-compact native"),
            Command::AutoCompact(Some(AutoCompactSetting::Mode(
                CompactionMode::NativeResponses
            )))
        );
        assert_eq!(
            parse("/auto-compact 85%"),
            Command::AutoCompact(Some(AutoCompactSetting::ThresholdPercent(85)))
        );
        assert_eq!(parse("/reload"), Command::Reload);
        assert_eq!(parse("/new"), Command::New);
        assert_eq!(parse("/resume id"), Command::Resume(Some("id".into())));
        assert_eq!(parse("/fork"), Command::Fork);
        assert_eq!(parse("/clone"), Command::Clone);
        assert_eq!(parse("/status"), Command::Status);
        assert_eq!(parse("/context"), Command::Context);
        assert_eq!(parse("/help"), Command::Help(None));
        assert_eq!(parse("/help status"), Command::Help(Some("status".into())));
        assert_eq!(parse("/cost"), Command::Cost);
        assert_eq!(parse("/hotkeys"), Command::Hotkeys);
        assert_eq!(parse("/copy"), Command::Copy);
        assert_eq!(parse("/session"), Command::Session);
        assert_eq!(parse("/session info"), Command::Session);
        assert!(matches!(parse("/session unknown"), Command::Unknown(_)));
        assert!(matches!(parse("/copy extra"), Command::Unknown(_)));
        assert_eq!(parse("/cache"), Command::Cache);
        assert_eq!(parse("/update"), Command::Update);
        assert_eq!(parse("/prompt"), Command::Prompt(None));
        assert_eq!(
            parse("/prompt review staged changes"),
            Command::Prompt(Some("review staged changes".into()))
        );
        assert_eq!(parse("/exit"), Command::Exit);
        assert_eq!(parse("/skills"), Command::Skills(SkillsSubcommand::List));
        assert_eq!(
            parse("/skills list"),
            Command::Skills(SkillsSubcommand::List)
        );
        assert_eq!(
            parse("/sk active"),
            Command::Skills(SkillsSubcommand::Active)
        );
        assert_eq!(
            parse("/skills search rust review"),
            Command::Skills(SkillsSubcommand::Search("rust review".into()))
        );
        assert_eq!(
            parse("/skills load audit"),
            Command::Skills(SkillsSubcommand::Load("audit".into()))
        );
        assert_eq!(
            parse("/skills reload"),
            Command::Skills(SkillsSubcommand::Reload)
        );
        assert_eq!(
            parse("/skills off audit"),
            Command::Skills(SkillsSubcommand::Off("audit".into()))
        );
        assert_eq!(
            parse("/extensions"),
            Command::Extensions(ExtensionsSubcommand::Menu)
        );
        assert_eq!(
            parse("/extensions list"),
            Command::Extensions(ExtensionsSubcommand::Menu)
        );
        assert_eq!(
            parse("/extensions status"),
            Command::Extensions(ExtensionsSubcommand::Status)
        );
        assert_eq!(
            parse("/extensions inspect agent-session:abc"),
            Command::Extensions(ExtensionsSubcommand::Inspect {
                reference: "agent-session:abc".into(),
            })
        );
        assert_eq!(
            parse("/extensions action octet-subagents stop-worker"),
            Command::Extensions(ExtensionsSubcommand::Action {
                extension: "octet-subagents".into(),
                action: "stop-worker".into(),
            })
        );
    }

    #[test]
    fn slash_suggestions_filter_and_tab_complete_unique_prefixes() {
        assert_eq!(slash_suggestions("/").len(), SLASH_COMMANDS.len());
        assert_eq!(slash_suggestions("/mod")[0].usage, "/model [id]");
        for prefix in ["/log", "/logi", "/setu"] {
            let names = slash_suggestions(prefix)
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>();
            assert!(
                names.contains(&"login") && names.contains(&"setup"),
                "{prefix}: {names:?}"
            );
            assert_eq!(
                names[0],
                if prefix.starts_with("/log") {
                    "login"
                } else {
                    "setup"
                }
            );
            assert_eq!(complete_slash_command(prefix), None);
        }
        assert_eq!(
            slash_suggestions("/login")
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            ["login"]
        );
        assert_eq!(
            slash_suggestions("/setup")
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            ["setup"]
        );
        assert_eq!(slash_suggestions("/th").len(), 2);
        assert!(slash_suggestions("/model ").is_empty());
        assert_eq!(complete_slash_command("/mod"), Some("/model ".to_owned()));
        assert_eq!(
            complete_slash_command("/thi"),
            Some("/thinking ".to_owned())
        );
        assert_eq!(
            complete_slash_command("/status"),
            Some("/status".to_owned())
        );
    }

    #[test]
    fn popup_registry_includes_self_help_without_removed_commands() {
        assert!(SLASH_COMMANDS.iter().any(|command| command.name == "help"));
        for removed in ["cycle-model", "docs", "sessions", "tool"] {
            assert!(SLASH_COMMANDS.iter().all(|command| command.name != removed));
        }
        assert!(SLASH_COMMANDS
            .iter()
            .all(|command| command.name != "Session"));
    }

    #[test]
    fn every_discovered_builtin_has_an_executable_parser_route() {
        for command in SLASH_COMMANDS {
            let invocation = match command.name {
                "name" => "/name release audit".to_owned(),
                "export" => "/export audit.md".to_owned(),
                name => format!("/{name}"),
            };
            assert!(
                !matches!(parse(&invocation), Command::Unknown(_)),
                "popup advertises /{} but its representative invocation {invocation:?} has no parser route",
                command.name
            );
        }
    }

    #[test]
    fn parses_unambiguous_command_prefixes() {
        assert_eq!(parse("/mod"), Command::Model(None));
        assert_eq!(
            parse("/mo gpt-4o-mini"),
            Command::Model(Some("gpt-4o-mini".into()))
        );
        assert_eq!(parse("/comp"), Command::Compact);
        // /c and /t each match multiple commands, so they remain unknown.
        assert!(matches!(parse("/c"), Command::Unknown(_)));
        assert!(matches!(parse("/t"), Command::Unknown(_)));
    }

    #[test]
    fn rejects_unknown_or_malformed_commands() {
        assert!(matches!(parse("hello"), Command::Unknown(_)));
        assert!(matches!(parse("/new extra"), Command::Unknown(_)));
        assert!(matches!(parse("/auto-compact 0%"), Command::Unknown(_)));
        assert!(matches!(parse("/auto-compact 101%"), Command::Unknown(_)));
        for removed in ["/cycle-model", "/docs", "/sessions", "/tool"] {
            assert!(matches!(parse(removed), Command::Unknown(_)));
        }
    }

    fn app_for_status() -> (tempfile::TempDir, App) {
        use crate::app::bootstrap::{bootstrap, build_app, LaunchSelection, SessionSelection};
        use crate::config::{CompactionPolicy, Config, Mode, ResumeSelector, SandboxPolicy};
        use octet_ai::{ModelId, ReasoningConfig};

        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            workspace: directory.path().to_owned(),
            invocation_cwd: directory.path().to_owned(),
            model: Some(ModelId("gpt-4o-mini".into())),
            model_explicit: false,
            reasoning: None,
            reasoning_explicit: false,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
            reasoning_mode_explicit: false,
            cache_retention: octet_ai::CacheRetention::Short,
            effect_policy: octet_agent::EffectPolicy::Controlled,
            sandbox: SandboxPolicy {
                allow_external_paths: false,
                ..SandboxPolicy::default()
            },
            theme: None,
            system_prompt: None,
            theme_paths: vec![],
            color: crate::config::ColorMode::Auto,
            mouse: crate::config::MouseMode::Auto,
            plain: false,
            show_images: false,
            session_dir: directory.path().join("sessions"),
            compaction: CompactionPolicy::default(),
            max_cost_microdollars: None,
            cost_warning_microdollars: None,
            max_turns: Some(40),
            show_reasoning_in_print: false,
            initial_prompt: None,
            prompt_template: None,
            debug_prompt: false,
            prompt_paths: vec![],
            mode: Mode::Interactive,
            resume: ResumeSelector::New,
            skill_paths: vec![],
            extension_paths: vec![],
            enabled_extensions: vec![],
            extension_activation_overridden: false,
            trusted_extensions: vec![],
            invocation_trusted_extensions: vec![],
            experimental_streamable_http_mcp: false,
            extension_flag_values: Default::default(),
            tools: crate::config::ToolPolicy::default(),
            telemetry: None,
            context_files: true,
            offline: true,
            workspace_trusted: true,
        };
        let boot = bootstrap(config).unwrap();
        let app = build_app(
            boot,
            LaunchSelection {
                model: ModelId("gpt-4o-mini".into()),
                session: SessionSelection::CreateNew(directory.path().join("session.jsonl")),
                reasoning: ReasoningConfig::Off,
                reasoning_mode: octet_ai::ReasoningMode::Standard,
            },
            "system".into(),
        )
        .unwrap();
        (directory, app)
    }

    #[test]
    fn unpriced_usage_reports_remain_uncertain_on_a_priced_model_and_reopen() {
        let (_directory, mut app) = app_for_status();
        assert!(app.model.spec.pricing.is_some());
        let endpoint = app.model.endpoint.id.clone();
        let model = app.model.spec.id.clone();
        // Known zero is still an exact price; absent pricing is different.
        app.agent
            .session_mut()
            .record_compaction_usage(
                endpoint.clone(),
                model.clone(),
                Usage::default(),
                Some(Cost::default()),
            )
            .unwrap();
        assert!(!app.agent.session().has_unpriced_usage());
        assert!(!status_text(&app, None).contains("known subtotal"));
        app.agent
            .session_mut()
            .record_compaction_usage(
                endpoint,
                model,
                Usage {
                    input_tokens: 5,
                    output_tokens: 2,
                    total_tokens: 7,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
        for reopened in [false, true] {
            if reopened {
                app = crate::app::bootstrap::rebuild_app(app, None, None, None, None).unwrap();
            }
            let session = app.agent.session();
            assert!(session.has_unpriced_usage());
            assert!(
                !session.has_uncertain_usage(),
                "do not invent an unknown-token attempt"
            );
            assert!(
                app.model.spec.pricing.is_some(),
                "active catalog pricing cannot price a historical receipt"
            );
            assert!(status_text(&app, None).contains("known subtotal"));
            assert!(session_text(session).contains("known subtotal"));
            assert!(cost_text(session, &app.model).contains("Known subtotal only"));
            assert!(session.usage_uncertainty_records().is_empty());
        }
    }

    #[test]
    fn status_references_real_runtime_features() {
        let (_directory, app) = app_for_status();
        let queued = Reconfig::NewSession;
        let status = status_text(&app, Some(&queued));
        for expected in [
            "Provider       openai",
            "Model          gpt-4o-mini",
            "Reasoning      off",
            "Workspace",
            "Session",
            "Context",
            "Model turns",
            "Tool calls",
            "Security model: local agent with workspace trust gates",
            "Effect policy: controlled (workspace mutation and unsafe bash calls need approval; other ambient host effects denied)",
            "Built-in file paths: workspace-only guard",
            "File edits: enabled",
            "Process execution: enabled",
            "Shell execution: enabled",
            "OS isolation: none",
            "Process privileges: current user",
            "Repository trust: trusted (project config/context/skills enabled)",
            "NewSession",
        ] {
            assert!(
                status.contains(expected),
                "missing {expected:?} in {status}"
            );
        }
        let expected_skills = format!(
            "Skills         0 active / {} discovered",
            app.skills.descriptors().len()
        );
        assert!(
            status.contains(&expected_skills),
            "missing {expected_skills:?} in {status}"
        );
    }

    /// A Codex-declared route with an explicit effective window, built on the
    /// existing bootstrap fixture so no second catalog is invented.
    fn codex_route(model_id: &str, context_window: u64) -> octet_ai::Model {
        let (_directory, app) = app_for_status();
        let mut model = app.model.clone();
        std::sync::Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiResponses;
        std::sync::Arc::make_mut(&mut model.spec).id = octet_ai::ModelId(model_id.into());
        std::sync::Arc::make_mut(&mut model.spec)
            .limits
            .context_window = context_window;
        std::sync::Arc::make_mut(&mut model.endpoint)
            .runtime
            .responses_profile = octet_ai::ResponsesRuntimeProfile::Codex;
        model
    }

    #[test]
    fn codex_context_surface_is_absent_for_every_other_route() {
        let (_directory, app) = app_for_status();
        assert_eq!(
            CodexContextSurface::capture(&app.model, true),
            None,
            "a non-Codex route must not offer a Codex context-window surface"
        );
    }

    #[test]
    fn codex_context_surface_reports_the_deliberate_cap_and_why() {
        // astra advertises 872K, which the deliberate 272K cap reduces.
        let surface = CodexContextSurface::capture(&codex_route("gpt-6-astra", 272_000), true)
            .expect("Codex route");
        assert_eq!(surface.effective_window(), 272_000);
        assert!(!surface.has_uncertain_usage());
        let clamp = surface
            .clamp()
            .expect("the deliberate cap must be reported");
        assert_eq!(clamp.advertised_context_window, 872_000);
        assert_eq!(clamp.effective_context_window, 272_000);
        let message = clamp.message();
        assert!(message.contains("double-priced"), "{message}");
        assert!(message.contains("websocket"), "{message}");
        let summary = surface.summary_lines().join("\n");
        // Labelled windows, matching the session note's house style.
        assert!(summary.contains("advertised 872K"), "{summary}");
        assert!(summary.contains("effective 272K"), "{summary}");
    }

    /// The effort menu is user-facing prose. It must never render an internal
    /// API path, function call, or operation identifier, and every window it
    /// quotes must be labelled.
    #[test]
    fn the_effort_menu_summary_never_renders_internal_identifiers() {
        for (model_id, window) in [
            ("gpt-6-astra", 272_000u64),
            ("gpt-5.6-luna", 372_000),
            ("gpt-6-astra", 872_000),
            ("gpt-5.6-luna", 1_000_000),
        ] {
            for entitled in [false, true] {
                let surface =
                    CodexContextSurface::capture(&codex_route(model_id, window), entitled)
                        .expect("Codex route");
                let summary = surface.summary_lines().join("\n");
                for leak in [
                    "Session::",
                    "record_usage_uncertainty",
                    "codex-context-above-272k",
                    "uncertain_usage_operation",
                    "::",
                    "()",
                    "crates/",
                    "fn ",
                    "CodexContext",
                ] {
                    assert!(
                        !summary.contains(leak),
                        "{model_id}/{window}/entitled={entitled}: internal identifier {leak:?} \
                         leaked: {summary}"
                    );
                }
                // Every quoted window is labelled, never a bare number.
                assert!(
                    !summary.contains("272000") && !summary.contains("372000"),
                    "unlabelled window: {summary}"
                );
                assert!(!summary.contains('$'), "exact cost figure: {summary}");
                // The deliberate cap is described as a decision, never a defect.
                for wrong in ["bug", "regression", "broken", "incorrect"] {
                    assert!(!summary.contains(wrong), "{wrong:?} in {summary}");
                }
                assert!(
                    !summary.contains("no ceiling") && !summary.contains("?"),
                    "placeholder-style blob: {summary}"
                );
            }
        }
    }

    #[test]
    fn above_the_standard_tier_cost_is_uncertain_never_an_exact_figure() {
        // `gpt-5.6-luna` is the documented 372K family.
        let surface = CodexContextSurface::capture(&codex_route("gpt-5.6-luna", 372_000), true)
            .expect("Codex route");
        assert!(surface.has_uncertain_usage());
        assert_eq!(
            surface.uncertain_usage_operation(),
            Some(crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION)
        );
        let summary = surface.summary_lines().join("\n");
        assert!(summary.contains("UNCERTAIN"), "{summary}");
        assert!(summary.contains("double-priced"), "{summary}");
        assert!(summary.contains("websocket"), "{summary}");
        assert!(
            !summary.contains('$'),
            "no exact-looking cost figure may be rendered above the standard tier: {summary}"
        );
        // The operation id is an internal diagnostic; it is recorded by the
        // session, never rendered. `the_effort_menu_summary_never_renders_...`
        // asserts that for every Codex family and entitlement.
        assert!(!summary.contains("codex-context-above-272k"), "{summary}");
    }

    #[test]
    fn a_raise_fails_closed_without_the_entitlement_or_the_acknowledgement() {
        let unentitled = CodexContextSurface::capture(&codex_route("gpt-6-astra", 272_000), false)
            .expect("Codex route");
        assert_eq!(unentitled.raise_target(), None);
        let refused = unentitled.raise(872_000, true).unwrap_err();
        assert!(refused.contains("Pro or ProLite"), "{refused}");
        assert!(
            unentitled
                .summary_lines()
                .join("\n")
                .contains("Pro or ProLite"),
            "the unentitled surface must say what a raise needs"
        );

        let entitled = CodexContextSurface::capture(&codex_route("gpt-6-astra", 272_000), true)
            .expect("Codex route");
        assert_eq!(entitled.raise_target(), Some(872_000));
        let unacknowledged = entitled.raise(872_000, false).unwrap_err();
        assert!(unacknowledged.contains("double-priced"), "{unacknowledged}");
        assert!(unacknowledged.contains("websocket"), "{unacknowledged}");
        assert_eq!(entitled.raise(872_000, true), Ok(872_000));
        // Above the model's entitlement the request is refused outright.
        let above = entitled.raise(4_000_000, true).unwrap_err();
        assert!(above.contains("872000"), "{above}");
        assert!(entitled
            .raise_instruction(872_000)
            .contains("--codex-context-window 872000"));
    }
}
