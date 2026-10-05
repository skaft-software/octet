#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use octet_agent::{EffectPolicy, PolicyValueSource, ToolPolicyProvenance};
use octet_ai::ModelId;
use serde::Deserialize;

use crate::app::bootstrap::resolve_model_id;
use crate::batch::BatchCommand;
use crate::config::{
    self, ColorMode, CompactionMode, CompactionPolicy, Config, Mode, ResumeSelector, SandboxPolicy,
    ToolPolicy,
};
use crate::extension_package::ExtensionCommand;
use crate::migrate::MigrationCommand;
use crate::session_commands::SessionCommand;

pub(crate) mod catalog_publish;
mod config_diagnostics;
pub(crate) mod eval;
mod extension_flags;
pub(crate) mod parity;

pub(crate) use extension_flags::{parse_with_extension_flags, uses_runtime_extension_flag_parser};

use config_diagnostics::{
    read_layer, report_config_diagnostics, ConfigSourceKind, LoadedConfigLayer,
};

// The runtime flag parser moved to `extension_flags`; the suite reaches it
// through `use super::*`, so the names it needs are re-imported for tests only
// rather than left to warn as unused in the library build.
#[cfg(test)]
use {
    clap::{CommandFactory, FromArgMatches},
    extension_flags::{
        extension_flag_bootstrap, extension_flag_command, invocation_has_top_level_subcommand,
        register_extension_flags, resolve_extension_flag_values,
    },
    octet_agent::extension_process::{ExtensionFlag, ExtensionFlagType},
    std::ffi::OsString,
};

/// Deterministic, non-interactive setup input for one OpenAI-compatible endpoint.
#[derive(Clone, Debug, Args)]
pub struct SetupCommand {
    /// Setup profile. Select `lm-studio` to use its documented URL; an explicit
    /// --endpoint otherwise implies `openai-compatible`.
    #[arg(long, value_enum)]
    pub preset: Option<SetupPreset>,
    /// Explicit OpenAI-compatible versioned base URL. This is the only endpoint
    /// setup probes; no local or network scanning is performed.
    #[arg(long, value_name = "URL")]
    pub endpoint: Option<String>,
    /// Stable custom-registry provider ID (letters, digits, '-' and '_').
    #[arg(long, value_name = "ID")]
    pub provider: Option<String>,
    /// Human-facing provider label.
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,
    /// Select this ID from the discovered model inventory. Without it, setup
    /// uses the lexicographically first discovered model deterministically.
    #[arg(long, value_name = "ID", conflicts_with = "manual_model")]
    pub model: Option<String>,
    /// Store exactly this explicit model inventory without probing. Required for
    /// a new setup in offline mode.
    #[arg(long, value_name = "ID", conflicts_with = "model")]
    pub manual_model: Option<String>,
    /// Read a bearer credential from this environment variable at runtime. The
    /// variable value is never written to the custom provider registry.
    #[arg(long, value_name = "VAR", conflicts_with = "no_auth")]
    pub api_key_env: Option<String>,
    /// Explicitly use no authentication (the default for local profiles).
    #[arg(long, conflicts_with = "api_key_env")]
    pub no_auth: bool,
    /// Permit replacing an already configured provider with the same ID. A
    /// concurrent registry change still fails rather than being overwritten.
    #[arg(long)]
    pub replace: bool,
    /// Persist the reviewed setup and save its selected model preference.
    #[arg(long, conflicts_with = "cancel")]
    pub yes: bool,
    /// Stop after the deterministic review receipt without writing anything.
    #[arg(long, conflicts_with = "yes")]
    pub cancel: bool,
    /// Disable discovery network traffic for this setup only. Pair it with
    /// --manual-model to provide an explicit offline inventory.
    #[arg(long)]
    pub offline: bool,
}

/// Supported setup profiles. Native Ollama transport is deliberately not
/// advertised here; this flow uses only explicitly selected OpenAI-compatible
/// `/v1` endpoints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SetupPreset {
    /// LM Studio's documented keyless OpenAI-compatible endpoint.
    #[value(name = "lm-studio")]
    LmStudio,
    /// A user-supplied OpenAI-compatible endpoint.
    #[value(name = "openai-compatible")]
    OpenAiCompatible,
}

#[derive(Clone, Debug, Subcommand)]
pub enum TopLevelCommand {
    /// Inspect and manage durable local sessions.
    Sessions {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Integrate with the Herdr terminal workspace manager.
    Herdr {
        #[command(subcommand)]
        command: crate::herdr::HerdrCommand,
    },
    /// Install and manage extension packages.
    Extension {
        #[command(subcommand)]
        command: ExtensionCommand,
    },
    /// Inspect another coding-agent setup and plan a bounded migration.
    Migrate {
        #[command(subcommand)]
        command: MigrationCommand,
    },
    /// Check for, and install, a newer octet release.
    Update {
        /// Only report whether a newer release is available.
        #[arg(long)]
        check: bool,
    },
    /// Submit and inspect asynchronous OpenRouter Batch API jobs.
    Batch {
        #[command(subcommand)]
        command: BatchCommand,
    },
    /// Validate every publish gate and install a catalog document immutably.
    Catalog {
        #[command(subcommand)]
        command: catalog_publish::CatalogCommand,
    },
    /// Run isolated, fixture-backed evaluation suites and record their deltas.
    ///
    /// The harness injects its own loopback provider, writes its own artifact
    /// directory, and never makes a live or paid provider call.
    Eval {
        #[command(subcommand)]
        command: eval::EvalCommand,
    },
    /// Check local prerequisites, configured providers, and model visibility.
    Doctor,
    /// Configure one explicitly selected OpenAI-compatible provider without
    /// prompting. Review only by default; pass --yes to persist.
    Setup {
        #[command(flatten)]
        options: SetupCommand,
    },
}

/// Command-line launcher for `octet`.
#[derive(Debug, Default, Parser)]
#[command(
    name = "octet",
    version,
    about = "A local-first coding agent",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<TopLevelCommand>,
    /// An initial prompt. In interactive mode it is submitted after startup.
    #[arg(value_name = "PROMPT")]
    pub message: Option<String>,
    /// Additional prompts submitted sequentially in print/JSON mode.
    #[arg(value_name = "PROMPTS")]
    pub additional_messages: Vec<String>,
    #[command(flatten)]
    pub parity: parity::ParityOptions,
    /// Sign in to a subscription provider (`codex` or `copilot`) and exit.
    #[arg(long, value_name = "PROVIDER")]
    pub login: Option<String>,
    /// Sign out of a subscription provider (`codex` or `copilot`) and exit.
    #[arg(long, value_name = "PROVIDER")]
    pub logout: Option<String>,
    /// With `--login`, print the device URL/code without opening a browser.
    #[arg(long)]
    pub headless: bool,
    /// Frontend mode: interactive, json (session events), or rpc.
    #[arg(long, value_name = "MODE", conflicts_with = "print")]
    pub mode: Option<String>,
    /// Use headless print mode instead of the full-screen TUI.
    #[arg(long, short = 'p')]
    pub print: bool,
    /// Continue the newest session in this workspace.
    #[arg(long = "continue", conflicts_with = "resume")]
    pub continue_: bool,
    /// Resume a session by id, or open the session picker interactively.
    #[arg(
        long,
        value_name = "ID",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with = "continue_"
    )]
    pub resume: Option<Option<String>>,
    /// Fork a session by id, or open the session picker when omitted.
    #[arg(
        long,
        value_name = "ID",
        num_args = 0..=1,
        default_missing_value = "",
        conflicts_with_all = ["continue_", "resume"]
    )]
    pub fork: Option<Option<String>>,
    /// Model id override.
    #[arg(long)]
    pub model: Option<String>,
    /// Reasoning: off, minimal, low, medium, high, xhigh, max, ultra, or budget=N.
    #[arg(long)]
    pub reasoning: Option<String>,
    /// Deprecated persisted-session compatibility; Pro migrates to Ultra when V2 delegation is advertised.
    #[arg(long, value_name = "MODE", hide = true)]
    pub reasoning_mode: Option<String>,
    /// Prompt-cache retention: none, short, or long.
    #[arg(long, value_name = "POLICY")]
    pub cache_retention: Option<String>,
    /// Billable prompt-cache refreshes: off, streaming (default), or idle.
    #[arg(long, value_name = "MODE", value_parser = ["off", "streaming", "idle"])]
    pub cache_warming: Option<String>,
    /// Workspace root override.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// Terminal theme: auto, light, dark, Cards, Still, or a discovered TOML theme name.
    #[arg(long, value_name = "NAME")]
    pub theme: Option<String>,
    /// Add a TOML theme file or directory (repeatable).
    #[arg(long = "theme-dir", value_name = "FILE-OR-DIR")]
    pub theme_dirs: Vec<PathBuf>,
    /// Colour output policy: auto, always, or never.
    #[arg(long, value_name = "WHEN")]
    pub color: Option<String>,
    /// Use chronological ASCII output without cursor control.
    #[arg(long)]
    pub plain: bool,
    /// Opt in to bounded inline tool-result images on compatible interactive terminals.
    #[arg(long = "show-images")]
    pub show_images: bool,
    /// Mouse ownership: auto/terminal/off preserve native gestures; app
    /// captures wheel scrolling and drag selection for the semantic viewport.
    #[arg(long, value_name = "MODE")]
    pub mouse: Option<String>,
    /// Tern Surface Protocol rendering: auto negotiates native surfaces inside
    /// a Tern pane, on forces them, off always uses the terminal renderer.
    #[arg(long, value_name = "MODE")]
    pub tern: Option<String>,
    /// Emit reasoning deltas in print mode.
    #[arg(long)]
    pub show_reasoning: bool,
    /// Maximum model turns in one run.
    #[arg(long)]
    pub max_turns: Option<u64>,
    /// Persistent session directory override.
    #[arg(long)]
    pub session_dir: Option<PathBuf>,
    /// Expand a named prompt template around the positional prompt.
    #[arg(long = "prompt", value_name = "NAME")]
    pub prompt_template: Option<String>,
    /// Print or display the fully expanded named prompt and its content hash.
    #[arg(long)]
    pub debug_prompt: bool,
    /// Append privacy-preserving run and tool metrics to a JSONL file.
    #[arg(long, value_name = "PATH")]
    pub telemetry: Option<PathBuf>,
    /// Explicit prompt-template file or directory (repeatable, Pi compatible).
    #[arg(long = "prompt-template", value_name = "PATH")]
    pub prompt_templates: Vec<PathBuf>,
    /// Override the composed system prompt. Use `--system-prompt` to clear it.
    #[arg(
        long = "system-prompt",
        value_name = "PROMPT",
        num_args = 0..=1,
        default_missing_value = ""
    )]
    pub system_prompt: Option<String>,
    /// Additional directory paths to scan for agent skills.
    #[arg(long = "skill-dir", value_name = "DIR")]
    pub skill_dirs: Vec<PathBuf>,
    /// Additional directory paths to scan for executable extensions.
    #[arg(long = "extension-dir", value_name = "DIR")]
    pub extension_dirs: Vec<PathBuf>,
    /// Explicitly enable executable extensions by name (comma-separated).
    #[arg(
        long = "enable-extension",
        value_name = "NAMES",
        value_delimiter = ',',
        num_args = 1..
    )]
    pub enable_extensions: Vec<String>,
    /// Explicit invocation-only trust (comma-separated); full access already
    /// trusts selected extensions. Does not enable them; grants host process
    /// authority for the selected source even under safe mode.
    #[arg(
        long = "trust-extension",
        value_name = "NAMES",
        value_delimiter = ',',
        num_args = 1..
    )]
    pub trust_extensions: Vec<String>,
    /// Process-owner opt-in for experimental remote Streamable HTTP MCP. This
    /// one-shot gate is never read from configuration, environment, or sessions.
    #[arg(long = "experimental-streamable-http-mcp")]
    pub experimental_streamable_http_mcp: bool,
    /// Trust this workspace and load its project config, AGENTS.md, and skills.
    #[arg(long = "workspace-trusted", alias = "trust-workspace")]
    pub workspace_trusted: bool,
    /// Ask before every bash call and workspace change, and keep executable
    /// extensions stopped unless they have host authority. Not a sandbox.
    #[arg(long = "safe-mode", alias = "safe", conflicts_with = "effect_policy")]
    pub safe_mode: bool,
    /// How tool effects are admitted: unsafe_host (the default: full access,
    /// no sandbox and no approvals), controlled, or controlled_bash_approval.
    /// The effect broker enforces approvals only under the controlled profiles.
    #[arg(long, value_name = "POLICY")]
    pub effect_policy: Option<String>,
    /// Load only these tools (comma-separated).
    #[arg(long, value_name = "NAMES", value_delimiter = ',', num_args = 1..)]
    pub tools: Option<Vec<String>>,
    /// Add the Windows PowerShell tool to the model-visible allowlist.
    ///
    /// Opt-in only, additive to the default allowlist, and never a `bash`
    /// fallback: the entry stays inert on a host that cannot run it.
    #[arg(long, conflicts_with_all = ["tools", "no_tools"])]
    pub powershell: bool,
    /// Remove tools from the active set (comma-separated).
    #[arg(long, value_name = "NAMES", value_delimiter = ',', num_args = 1..)]
    pub exclude_tools: Vec<String>,
    /// Disable every built-in and skill tool.
    #[arg(long, conflicts_with = "tools")]
    pub no_tools: bool,
    /// Disable both file mutation tools (`edit` and `write`).
    #[arg(long)]
    pub no_edit: bool,
    /// Disable full-file creation and replacement.
    #[arg(long)]
    pub no_write: bool,
    /// Disable all command execution.
    #[arg(long)]
    pub no_process: bool,
    /// Disable all command execution (process execution is shell-equivalent authority).
    #[arg(long)]
    pub no_shell: bool,
    /// Explicitly enable command execution (overrides a disabling user setting).
    #[arg(long)]
    pub allow_shell: bool,
    /// Allow `read` to fetch public HTTPS image/audio URLs.
    #[arg(long, conflicts_with = "offline")]
    pub allow_remote_read: bool,
    /// Bash-compatible shell executable used by the `bash` tool.
    #[arg(long, value_name = "PATH")]
    pub shell_path: Option<PathBuf>,
    /// Do not load global or workspace AGENTS.md files.
    #[arg(long)]
    pub no_context_files: bool,
    /// Disable optional provider/model discovery network requests at startup.
    #[arg(long)]
    pub offline: bool,
    /// Treat unknown configuration keys as startup errors instead of warnings.
    #[arg(long)]
    pub strict_config: bool,
    /// Maximum `bash` tool execution time in seconds.
    #[arg(long, alias = "exec-timeout-secs")]
    pub bash_timeout_secs: Option<u64>,
    /// Maximum persisted tool output size in bytes.
    #[arg(long)]
    pub max_output_bytes: Option<usize>,
}

type ExtensionFlagValues = BTreeMap<String, BTreeMap<String, serde_json::Value>>;

#[derive(Clone, Debug, Default, Deserialize)]
struct CompactionLayer {
    #[serde(alias = "policy")]
    mode: Option<String>,
    /// Deprecated boolean spelling retained for existing configuration.
    enabled: Option<bool>,
    threshold_fraction: Option<f64>,
    max_active_tokens: Option<u64>,
    keep_recent_tokens: Option<u64>,
    /// Deprecated turn-count retention, mapped to 1,000 tokens per turn.
    keep_recent_turns: Option<usize>,
    compact_model: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct ConfigLayer {
    model: Option<String>,
    reasoning: Option<String>,
    effect_policy: Option<String>,
    reasoning_mode: Option<String>,
    cache_retention: Option<String>,
    /// Global only: a trusted project must not select extra billable requests.
    cache_warming: Option<String>,
    show_cache_miss_notices: Option<bool>,
    theme: Option<String>,
    color: Option<String>,
    mouse: Option<String>,
    tern: Option<String>,
    plain: Option<bool>,
    show_images: Option<bool>,
    /// User-level `/scoped-models` pattern list. Interactive cycling scope
    /// only; headless modes never consume it and a trusted project layer may
    /// not override it.
    models: Option<String>,
    allow_external_paths: Option<bool>,
    allow_edit: Option<bool>,
    allow_write: Option<bool>,
    allow_process: Option<bool>,
    allow_shell: Option<bool>,
    allow_remote_read: Option<bool>,
    shell_path: Option<PathBuf>,
    #[serde(alias = "exec_timeout_secs")]
    bash_timeout_secs: Option<u64>,
    max_output_bytes: Option<usize>,
    session_dir: Option<PathBuf>,
    max_turns: Option<u64>,
    max_cost_microdollars: Option<u64>,
    cost_warning_microdollars: Option<u64>,
    context_files: Option<bool>,
    offline: Option<bool>,
    strict_config: Option<bool>,
    /// Live-reload policy. User level only: `merge_project` never copies these
    /// keys, so a trusted project cannot arm automatic reload or change its
    /// cadence on the user's behalf.
    reload: Option<bool>,
    reload_poll_ms: Option<u64>,
    reload_debounce_ms: Option<u64>,
    reload_max_files: Option<usize>,
    /// Host re-exec opt-in. Separate from `reload` on purpose: resources and
    /// extensions may auto-reload by default, but replacing the running image
    /// (`execve` of a replaced `current_exe`) needs this explicit bool or an
    /// explicit `/reload --force`.
    reload_host: Option<bool>,
    telemetry: Option<PathBuf>,
    enabled_extensions: Option<Vec<String>>,
    trusted_extensions: Option<Vec<String>>,
    system_prompt: Option<String>,
    compaction: Option<CompactionLayer>,
}

impl ConfigLayer {
    fn merge(&mut self, newer: Self) {
        macro_rules! override_some {
            ($field:ident) => {
                if newer.$field.is_some() {
                    self.$field = newer.$field;
                }
            };
        }
        override_some!(model);
        override_some!(reasoning);
        override_some!(effect_policy);
        override_some!(reasoning_mode);
        override_some!(cache_retention);
        override_some!(cache_warming);
        override_some!(show_cache_miss_notices);
        override_some!(theme);
        override_some!(color);
        override_some!(mouse);
        override_some!(tern);
        override_some!(plain);
        override_some!(show_images);
        override_some!(models);
        override_some!(allow_external_paths);
        override_some!(allow_edit);
        override_some!(allow_write);
        override_some!(allow_process);
        override_some!(allow_shell);
        override_some!(allow_remote_read);
        override_some!(shell_path);
        override_some!(bash_timeout_secs);
        override_some!(max_output_bytes);
        override_some!(session_dir);
        override_some!(max_turns);
        override_some!(max_cost_microdollars);
        override_some!(cost_warning_microdollars);
        override_some!(context_files);
        override_some!(offline);
        override_some!(strict_config);
        override_some!(reload);
        override_some!(reload_poll_ms);
        override_some!(reload_debounce_ms);
        override_some!(reload_max_files);
        override_some!(reload_host);
        override_some!(telemetry);
        override_some!(enabled_extensions);
        override_some!(trusted_extensions);
        override_some!(system_prompt);
        match (self.compaction.as_mut(), newer.compaction) {
            (Some(current), Some(newer)) => {
                if newer.mode.is_some() {
                    current.mode = newer.mode;
                    current.enabled = None;
                } else if newer.enabled.is_some() {
                    current.enabled = newer.enabled;
                    current.mode = None;
                }
                if newer.threshold_fraction.is_some() {
                    current.threshold_fraction = newer.threshold_fraction;
                }
                if newer.max_active_tokens.is_some() {
                    current.max_active_tokens = newer.max_active_tokens;
                }
                if newer.keep_recent_tokens.is_some() {
                    current.keep_recent_tokens = newer.keep_recent_tokens;
                    current.keep_recent_turns = None;
                } else if newer.keep_recent_turns.is_some() {
                    current.keep_recent_turns = newer.keep_recent_turns;
                    current.keep_recent_tokens = None;
                }
                if newer.compact_model.is_some() {
                    current.compact_model = newer.compact_model;
                }
            }
            (None, Some(newer)) => self.compaction = Some(newer),
            _ => {}
        }
    }

    /// Merge a trusted project layer without allowing it to relax authority or
    /// resource floors established by the user's global configuration.
    fn merge_project(&mut self, mut project: Self) {
        fn tighten_bool(current: &mut Option<bool>, project: Option<bool>) {
            if let Some(project) = project {
                *current = Some(current.unwrap_or(true) && project);
            }
        }
        fn lower_u64(current: &mut Option<u64>, project: Option<u64>) {
            if let Some(project) = project {
                *current = Some(current.map_or(project, |current| current.min(project)));
            }
        }
        fn lower_usize(current: &mut Option<usize>, project: Option<usize>) {
            if let Some(project) = project {
                *current = Some(current.map_or(project, |current| current.min(project)));
            }
        }
        fn effect_policy_rank(value: &str) -> Option<u8> {
            match value.parse::<EffectPolicy>().ok()? {
                EffectPolicy::ControlledBashApproval => Some(0),
                EffectPolicy::Controlled => Some(1),
                EffectPolicy::UnsafeHost => Some(2),
            }
        }
        fn tighten_effect_policy(current: &mut Option<String>, project: Option<String>) {
            let Some(project) = project else {
                return;
            };
            let current_rank = current.as_deref().and_then(effect_policy_rank).unwrap_or(2);
            match effect_policy_rank(&project) {
                Some(project_rank) if project_rank <= current_rank => *current = Some(project),
                Some(_) => {}
                // Preserve malformed values so normal configuration validation
                // reports them instead of treating a project typo as absent.
                None => *current = Some(project),
            }
        }

        tighten_bool(
            &mut self.allow_external_paths,
            project.allow_external_paths.take(),
        );
        tighten_bool(&mut self.allow_edit, project.allow_edit.take());
        tighten_bool(&mut self.allow_write, project.allow_write.take());
        tighten_bool(&mut self.allow_process, project.allow_process.take());
        tighten_bool(&mut self.allow_shell, project.allow_shell.take());
        // Inline terminal images are opt-in at a user-controlled boundary. A
        // trusted project may turn them off but cannot make retained payloads
        // render in the user's terminal.
        if project.show_images.take() == Some(false) {
            self.show_images = Some(false);
        }
        // Remote reads are opt-in. A project may revoke a user grant, but may
        // never create network authority when the user/global layer omitted it.
        if project.allow_remote_read.take() == Some(false) {
            self.allow_remote_read = Some(false);
        }
        tighten_bool(&mut self.context_files, project.context_files.take());
        lower_u64(
            &mut self.bash_timeout_secs,
            project.bash_timeout_secs.take(),
        );
        lower_usize(&mut self.max_output_bytes, project.max_output_bytes.take());
        lower_u64(&mut self.max_turns, project.max_turns.take());
        lower_u64(
            &mut self.max_cost_microdollars,
            project.max_cost_microdollars.take(),
        );
        lower_u64(
            &mut self.cost_warning_microdollars,
            project.cost_warning_microdollars.take(),
        );
        // Offline and strict diagnostics are one-way safety settings for
        // project configuration.
        self.offline =
            Some(self.offline.unwrap_or(false) || project.offline.take().unwrap_or(false));
        if project.strict_config.take() == Some(true) {
            self.strict_config = Some(true);
        }
        // A trusted project may suggest activation, but executable trust is a
        // user-level decision and can never be granted by project config. The
        // interactive cycling scope is likewise a user-level preference.
        let trusted_extensions = self.trusted_extensions.clone();
        project.trusted_extensions = None;
        let scoped_models = self.models.clone();
        project.models = None;
        project.cache_warming = None;
        project.show_cache_miss_notices = None;
        tighten_effect_policy(&mut self.effect_policy, project.effect_policy.take());
        self.merge(project);
        self.trusted_extensions = trusted_extensions;
        self.models = scoped_models;
    }
}

pub fn global_config_path() -> Option<PathBuf> {
    global_config_path_from_home(dirs::home_dir())
}

fn global_config_path_from_home(home: Option<PathBuf>) -> Option<PathBuf> {
    home.filter(|home| home.is_absolute())
        .map(|home| home.join(".octet").join("config.toml"))
}

/// Owner-private diagnostics log written by the hidden `/debug` command.
///
/// Mirrors the release-notes convention of deriving from the same home
/// directory as the global config, so a relocated `HOME` relocates both.
pub fn debug_log_path() -> Option<PathBuf> {
    dirs::home_dir()
        .filter(|home| home.is_absolute())
        .map(|home| home.join(".octet").join("octet-debug.log"))
}

/// Persist the billable cache-warming mode to user configuration only.
pub fn persist_cache_warming(mode: octet_agent::CacheWarmMode) -> anyhow::Result<()> {
    let path = global_config_path().ok_or_else(|| {
        anyhow::anyhow!("cannot persist cache warming: user home directory is unavailable")
    })?;
    persist_key_to_path("cache_warming", config::cache_warming_label(mode), &path)
}

/// Resolve the native host's billable cache policy from user config and environment,
/// never from a run's workspace or session. Protocol 1 adds no authority field.
pub(crate) fn user_cache_warming_policy() -> anyhow::Result<(octet_agent::CacheWarmMode, bool)> {
    #[cfg(test)]
    {
        // Test hosts must not inherit the developer's real user preferences.
        Ok((octet_agent::CacheWarmMode::default(), false))
    }
    #[cfg(not(test))]
    {
        let global = match global_config_path() {
            Some(path) => read_layer(&path, ConfigSourceKind::Global)?.values,
            None => ConfigLayer::default(),
        };
        let mode = env_value("OCTET_CACHE_WARMING")
            .or(global.cache_warming)
            .as_deref()
            .map(config::parse_cache_warming)
            .transpose()?
            .unwrap_or_default();
        Ok((mode, global.show_cache_miss_notices.unwrap_or(false)))
    }
}

pub fn persist_model(model: &str) -> anyhow::Result<()> {
    let path = global_config_path().ok_or_else(|| {
        anyhow::anyhow!("cannot persist model: user home directory is unavailable")
    })?;
    persist_key_to_path("model", model, &path)
}

/// Persist the user-level interactive model scope as one comma-separated
/// `--models` pattern list. `None` removes the key so the next launch cycles
/// the complete catalog again.
///
/// This is the same structural, compare-and-swap writer every other user
/// preference uses; a trusted project layer can never override the value.
pub fn persist_scoped_models(patterns: Option<&str>) -> anyhow::Result<()> {
    let path = global_config_path().ok_or_else(|| {
        anyhow::anyhow!("cannot persist the model scope: user home directory is unavailable")
    })?;
    let patterns = patterns.map(str::trim).filter(|value| !value.is_empty());
    if let Some(patterns) = patterns {
        if patterns.chars().any(char::is_control) {
            anyhow::bail!("model scope patterns must not contain control characters");
        }
    }
    match patterns {
        Some(patterns) => persist_key_to_path("models", patterns, &path),
        None => remove_key_from_path("models", &path),
    }
}

/// Read the persisted interactive model scope without rebuilding a full
/// configuration. Interactive-only by construction: headless frontends never
/// consult it.
pub fn persisted_scoped_models() -> Option<String> {
    let path = global_config_path()?;
    let layer = read_layer(&path, ConfigSourceKind::Global).ok()?;
    layer
        .values
        .models
        .map(|patterns| patterns.trim().to_owned())
        .filter(|patterns| !patterns.is_empty())
}

/// Live-reload policy for the interactive frontend.
///
/// Read from the user level only (`reload`, `reload_poll_ms`,
/// `reload_debounce_ms`, `reload_max_files`, `reload_host`), because automatic
/// reload and its cadence are user decisions a trusted project must not make on
/// the user's behalf. Values are range-clamped, and a missing or unreadable
/// file leaves the defaults in place — the same fail-open shape
/// `persisted_scoped_models` uses, since `build_config` already reports a broken
/// configuration.
///
/// Enabled by default for resources and extensions: the interactive prompt arms
/// the supervisor, announces itself once in the transcript, and applies reloads
/// only at the idle prompt. `reload = false` disables it for good. The host
/// layer (`execve` of a changed `current_exe`) is **off** unless the user opted
/// in with `reload_host = true`; `/reload --force` can always take a host pass
/// now, and the deployed host may additionally require a confirmation for a
/// retargeted image (`reexec.rs`).
pub fn live_reload_settings() -> crate::reload::ReloadSettings {
    let Some(path) = global_config_path() else {
        return crate::reload::ReloadSettings::default();
    };
    let Ok(layer) = read_layer(&path, ConfigSourceKind::Global) else {
        return crate::reload::ReloadSettings::default();
    };
    let values = layer.values;
    crate::reload::ReloadSettings {
        enabled: values.reload.unwrap_or(true),
        host_enabled: values.reload_host.unwrap_or(false),
        poll_interval: values
            .reload_poll_ms
            .map(std::time::Duration::from_millis)
            .unwrap_or(crate::reload::DEFAULT_POLL_INTERVAL),
        debounce: values
            .reload_debounce_ms
            .map(std::time::Duration::from_millis)
            .unwrap_or(crate::reload::DEFAULT_DEBOUNCE),
        max_inspections_per_poll: values
            .reload_max_files
            .unwrap_or(crate::reload::DEFAULT_MAX_INSPECTIONS_PER_POLL),
    }
    .sanitized()
}

/// Persist the opt-in inline-image rendering preference as a real TOML
/// boolean, matching the `show_images: bool` configuration reader.
pub fn persist_show_images(enabled: bool) -> anyhow::Result<()> {
    let path = global_config_path().ok_or_else(|| {
        anyhow::anyhow!("cannot persist image display: user home directory is unavailable")
    })?;
    persist_bool_key_to_path("show_images", enabled, &path)
}

pub fn persist_reasoning(reasoning: &str) -> anyhow::Result<()> {
    let path = global_config_path().ok_or_else(|| {
        anyhow::anyhow!("cannot persist reasoning: user home directory is unavailable")
    })?;
    persist_key_to_path("reasoning", reasoning, &path)
}

/// Persist a built-in selector or a resource name already validated and loaded
/// by the interactive picker. Keep custom names case-sensitive for discovery.
pub fn persist_theme_choice(choice: &str) -> anyhow::Result<()> {
    let key = theme_choice_key(choice)?;
    let path = global_config_path().ok_or_else(|| {
        anyhow::anyhow!("cannot persist theme: user home directory is unavailable")
    })?;
    persist_key_to_path("theme", key, &path)
}

fn theme_choice_key(choice: &str) -> anyhow::Result<&str> {
    let choice = choice.trim();
    if let Some(builtin) = crate::tui::theme::TerminalThemeChoice::parse(choice) {
        return Ok(builtin.key());
    }
    // `Cards` and `Still` are reserved so a discovered file cannot shadow the
    // built-in, but the built-in itself is selected by that same name. The
    // reserved stem stays valid here; only `default` and a `.toml` spelling are
    // rejected, because the config loader resolves them to the fallback or to a
    // file rather than to the selector the user picked.
    if crate::tui::theme::is_compiled_file_theme_name(choice) {
        return Ok(crate::tui::theme::compiled_file_theme_name(choice).unwrap_or(choice));
    }
    if crate::resource_resolver::valid_resource_name(choice)
        && !choice.ends_with(".toml")
        && !crate::tui::theme::is_reserved_theme_name(choice)
    {
        Ok(choice)
    } else {
        anyhow::bail!("invalid theme selector {choice:?}")
    }
}

/// First-run appearance onboarding is only eligible for a genuinely fresh
/// interactive configuration. Existing global/project configs and legacy
/// theme selectors are left alone so upgrades never reopen the picker.
pub(crate) fn should_offer_theme_onboarding(config: &Config) -> bool {
    let Some(global) = global_config_path() else {
        return false;
    };
    should_offer_theme_onboarding_at(config, Some(&global))
}

fn should_offer_theme_onboarding_at(config: &Config, global: Option<&Path>) -> bool {
    matches!(config.mode, Mode::Interactive)
        && !config.plain
        && config.theme.is_none()
        && !terminal_appearance_environment_is_configured()
        && global.is_some_and(|path| !path.exists())
        && !project_config_path(&config.workspace).exists()
}

fn terminal_appearance_environment_is_configured() -> bool {
    terminal_appearance_environment_is_configured_value(
        std::env::var("OCTET_COLOR_SCHEME").ok().as_deref(),
    )
}

fn terminal_appearance_environment_is_configured_value(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "auto" | "dark" | "light" | "unknown" | "universal"
        )
    })
}

pub fn persist_reasoning_mode(mode: octet_ai::ReasoningMode) -> anyhow::Result<()> {
    let path = global_config_path().ok_or_else(|| {
        anyhow::anyhow!("cannot persist reasoning mode: user home directory is unavailable")
    })?;
    let value = match mode {
        octet_ai::ReasoningMode::Standard => "standard",
        octet_ai::ReasoningMode::Pro => "pro",
    };
    persist_key_to_path("reasoning_mode", value, &path)
}

/// Persist one executable extension's activation without copying merged
/// project, environment, or command-line activation into the user config.
pub fn persist_extension_enabled(name: &str, enabled: bool) -> anyhow::Result<Vec<String>> {
    let path = global_config_path().ok_or_else(|| {
        anyhow::anyhow!("cannot persist extension activation: user home directory is unavailable")
    })?;
    persist_extension_enabled_to_path(name, enabled, &path)
}

/// Revalidate that the user config remains the next-launch authority before an
/// interactive extension toggle mutates it. A newly added trusted-project layer
/// fails closed instead of making a global edit look durable when it is not.
pub fn extension_activation_menu_authoritative(config: &Config) -> anyhow::Result<bool> {
    if config.extension_activation_overridden {
        return Ok(false);
    }
    if !config.workspace_trusted {
        return Ok(true);
    }
    let project = read_layer(
        &project_config_path(&config.workspace),
        ConfigSourceKind::Project,
    )?;
    Ok(project.values.enabled_extensions.is_none())
}

fn persist_extension_enabled_to_path(
    name: &str,
    enabled: bool,
    path: &std::path::Path,
) -> anyhow::Result<Vec<String>> {
    let name = normalize_extension_name(name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _config_lock = config_update_lock(path)?;

    let original = match std::fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let content = original.as_deref().unwrap_or_default();
    let mut document = if content.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        content.parse::<toml_edit::DocumentMut>().map_err(|error| {
            anyhow::anyhow!("cannot update invalid config {}: {error}", path.display())
        })?
    };
    let mut names = std::collections::BTreeSet::new();
    if let Some(item) = document.get("enabled_extensions") {
        let values = item.as_array().ok_or_else(|| {
            anyhow::anyhow!(
                "cannot update config {}: enabled_extensions must be an array of names",
                path.display()
            )
        })?;
        for value in values {
            let value = value.as_str().ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot update config {}: enabled_extensions must contain only strings",
                    path.display()
                )
            })?;
            names.insert(normalize_extension_name(value)?);
        }
    }
    if enabled {
        names.insert(name);
    } else {
        names.remove(&name);
    }

    let mut values = toml_edit::Array::new();
    for name in &names {
        values.push(name.as_str());
    }
    document["enabled_extensions"] = toml_edit::value(values);
    write_config_atomically(path, &document.to_string(), original.as_deref())?;
    Ok(names.into_iter().collect())
}

/// Persist or revoke a source-bound host authority grant in user config.
/// Existing `trusted_extensions` values retain their meaning across upgrades.
pub fn persist_extension_host_authority(grant: &str, allowed: bool) -> anyhow::Result<Vec<String>> {
    let path = global_config_path().ok_or_else(|| {
        anyhow::anyhow!("cannot persist host authority: user home directory is unavailable")
    })?;
    persist_extension_host_authority_to_path(grant, allowed, &path)
}

/// Environment trust takes precedence over user config and cannot be changed
/// through the menu. Project config is never permitted to grant host authority.
pub fn extension_host_authority_menu_authoritative() -> bool {
    std::env::var_os("OCTET_TRUSTED_EXTENSIONS").is_none()
}

fn persist_extension_host_authority_to_path(
    grant: &str,
    allowed: bool,
    path: &std::path::Path,
) -> anyhow::Result<Vec<String>> {
    let grant = normalize_extension_trust_grants(vec![grant.to_owned()])?
        .into_iter()
        .next()
        .expect("one grant remains one normalized grant");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _config_lock = config_update_lock(path)?;
    let original = match std::fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let content = original.as_deref().unwrap_or_default();
    let mut document = content.parse::<toml_edit::DocumentMut>().map_err(|error| {
        anyhow::anyhow!("cannot update invalid config {}: {error}", path.display())
    })?;
    let mut grants = Vec::new();
    if let Some(item) = document.get("trusted_extensions") {
        let values = item.as_array().ok_or_else(|| {
            anyhow::anyhow!(
                "cannot update config {}: trusted_extensions must be an array",
                path.display()
            )
        })?;
        for value in values {
            grants.push(
                value
                    .as_str()
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "cannot update config {}: trusted_extensions must contain only strings",
                            path.display()
                        )
                    })?
                    .to_owned(),
            );
        }
    }
    let mut grants = normalize_extension_trust_grants(grants)?;
    if allowed {
        grants.push(grant);
    } else {
        let selected_source = extension_grant_source(&grant, path);
        grants.retain(|value| {
            value != &grant
                && (selected_source.is_none()
                    || extension_grant_source(value, path) != selected_source)
        });
    }
    grants.sort();
    grants.dedup();
    let mut values = toml_edit::Array::new();
    for value in &grants {
        values.push(value.as_str());
    }
    document["trusted_extensions"] = toml_edit::value(values);
    write_config_atomically(path, &document.to_string(), original.as_deref())?;
    Ok(grants)
}

/// Resolve grants using the same parent-only canonicalization as startup's
/// normalize_trusted_manifest_path: the manifest itself is not followed.
/// Bare grants identify only the global source, never every source of a name.
fn extension_grant_source(grant: &str, config_path: &Path) -> Option<(String, PathBuf)> {
    let (name, manifest) = match grant.split_once('@') {
        Some((name, path)) => (name, PathBuf::from(path)),
        None => (
            grant,
            config_path
                .parent()?
                .join("extensions")
                .join(grant)
                .join("extension.toml"),
        ),
    };
    if !manifest.is_absolute() || manifest.file_name()? != "extension.toml" {
        return None;
    }
    let parent = manifest.parent()?.canonicalize().ok()?;
    Some((name.to_owned(), parent.join("extension.toml")))
}

fn config_update_lock(path: &std::path::Path) -> anyhow::Result<std::fs::File> {
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("config path {} has no file name", path.display()))?;
    let mut lock_name = file_name.to_os_string();
    lock_name.push(".lock");
    let lock_path = path.with_file_name(lock_name);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(&lock_path)?;
    #[cfg(unix)]
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
        anyhow::anyhow!(
            "another config update is in progress for {}: {error}",
            path.display()
        )
    })?;
    Ok(file)
}

fn write_config_atomically(
    path: &std::path::Path,
    content: &str,
    expected: Option<&str>,
) -> anyhow::Result<()> {
    const MAX_CONFIG_BYTES: usize = 1024 * 1024;
    octet_agent::secure_fs::write_atomic_if_unchanged(
        path,
        expected.map(str::as_bytes),
        content.as_bytes(),
        MAX_CONFIG_BYTES,
    )
    .map_err(|error| {
        anyhow::anyhow!(
            "could not atomically update config {} without overwriting a concurrent edit: {error}",
            path.display()
        )
    })
}

/// Render one structural user-config key update without writing it.
///
/// Migration ingestion uses this same persistence transformation inside its
/// larger compare-and-swap transaction, while interactive callers retain the
/// ordinary locked write path below.
pub(crate) fn render_persisted_key_update(
    key: &str,
    value: &str,
    original: Option<&str>,
    path: &std::path::Path,
) -> anyhow::Result<String> {
    let content = original.unwrap_or_default();
    let mut document = if content.trim().is_empty() {
        toml_edit::DocumentMut::new()
    } else {
        content.parse::<toml_edit::DocumentMut>().map_err(|error| {
            anyhow::anyhow!("cannot update invalid config {}: {error}", path.display())
        })?
    };

    // Structural TOML editing avoids partial-key matches and orphaned lines
    // from multiline values while retaining the user's comments and layout.
    document[key] = toml_edit::value(value);
    Ok(document.to_string())
}

/// Render the normal global model persistence update without writing it.
pub(crate) fn render_model_persistence_update(
    original: Option<&str>,
    path: &std::path::Path,
    model: &str,
) -> anyhow::Result<String> {
    render_persisted_key_update("model", model, original, path)
}

fn persist_key_to_path(key: &str, value: &str, path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _config_lock = config_update_lock(path)?;

    let original = match std::fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let new_content = render_persisted_key_update(key, value, original.as_deref(), path)?;

    // Atomic write: write to a unique sibling temp file then rename over the
    // real path so a crash or concurrent config writer cannot leave a partial
    // or collide with this writer's staging file.
    write_config_atomically(path, &new_content, original.as_deref())
}

/// Persist one structural user-config boolean. `toml_edit::value(&str)` would
/// write a string, so boolean-valued settings must not reuse the string path.
fn persist_bool_key_to_path(key: &str, value: bool, path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _config_lock = config_update_lock(path)?;

    let original = match std::fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut document = original
        .as_deref()
        .unwrap_or_default()
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| {
            anyhow::anyhow!("cannot update invalid config {}: {error}", path.display())
        })?;
    document[key] = toml_edit::value(value);
    write_config_atomically(path, &document.to_string(), original.as_deref())
}

/// Remove one structural user-config key while retaining comments and layout.
/// A missing file is already in the desired state.
fn remove_key_from_path(key: &str, path: &std::path::Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _config_lock = config_update_lock(path)?;

    let original = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut document = original
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| {
            anyhow::anyhow!("cannot update invalid config {}: {error}", path.display())
        })?;
    document.remove(key);
    write_config_atomically(path, &document.to_string(), Some(&original))
}

#[cfg(test)]
fn persist_model_to_path(model: &str, path: &std::path::Path) -> anyhow::Result<()> {
    persist_key_to_path("model", model, path)
}

#[cfg(test)]
fn persist_theme_to_path(choice: &str, path: &std::path::Path) -> anyhow::Result<()> {
    persist_key_to_path("theme", choice, path)
}

fn project_config_path(workspace: &Path) -> PathBuf {
    workspace.join(".octet").join("config.toml")
}

fn split_names(value: String) -> Vec<String> {
    value.split(',').map(str::to_owned).collect()
}

fn normalize_extension_names(
    names: impl IntoIterator<Item = String>,
) -> anyhow::Result<Vec<String>> {
    let mut normalized = std::collections::BTreeSet::new();
    for name in names {
        normalized.insert(normalize_extension_name(&name)?);
    }
    Ok(normalized.into_iter().collect())
}

fn normalize_extension_name(name: &str) -> anyhow::Result<String> {
    let name = name.trim().to_ascii_lowercase();
    let mut characters = name.chars();
    let valid = name.len() <= 64
        && characters
            .next()
            .is_some_and(|character| character.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
        });
    if !valid {
        anyhow::bail!(
            "invalid extension name {name:?}; use a lowercase letter followed by lowercase letters, digits, or '-' (64 bytes maximum)"
        );
    }
    Ok(name)
}

/// Normalize persistent executable trust grants without erasing their source
/// binding. A bare name applies only to the global extension directory. A
/// project or explicit source uses `name@path/to/extension.toml`.
fn normalize_extension_trust_grants(
    grants: impl IntoIterator<Item = String>,
) -> anyhow::Result<Vec<String>> {
    let mut normalized = std::collections::BTreeSet::new();
    for grant in grants {
        let grant = grant.trim();
        let normalized_grant = if let Some((name, path)) = grant.split_once('@') {
            let name = normalize_extension_name(name)?;
            let path = path.trim();
            if path.is_empty()
                || path.len() > 8 * 1024
                || path.chars().any(char::is_control)
                || !Path::new(path).is_absolute()
            {
                anyhow::bail!(
                    "invalid extension trust path {path:?}; persistent source-bound grants require an absolute path to extension.toml"
                );
            }
            format!("{name}@{path}")
        } else {
            normalize_extension_name(grant)?
        };
        normalized.insert(normalized_grant);
    }
    Ok(normalized.into_iter().collect())
}

#[cfg(not(test))]
fn env_value(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

#[cfg(not(test))]
fn env_parse<T>(name: &str) -> anyhow::Result<Option<T>>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    env_value(name)
        .map(|value| {
            value
                .parse::<T>()
                .map_err(|error| anyhow::anyhow!("invalid {name}={value:?}: {error}"))
        })
        .transpose()
}

#[cfg(not(test))]
fn environment_layer() -> anyhow::Result<ConfigLayer> {
    let compaction_mode = env_value("OCTET_COMPACTION_MODE");
    let compaction_enabled = env_parse("OCTET_AUTO_COMPACT")?;
    let threshold_fraction = env_parse("OCTET_COMPACTION_THRESHOLD_FRACTION")?;
    let max_active_tokens = env_parse("OCTET_COMPACTION_MAX_ACTIVE_TOKENS")?;
    let keep_recent_tokens = env_parse("OCTET_COMPACTION_KEEP_RECENT_TOKENS")?;
    let keep_recent_turns = env_parse("OCTET_COMPACTION_KEEP_RECENT_TURNS")?;
    let compact_model = env_value("OCTET_COMPACT_MODEL");
    Ok(ConfigLayer {
        model: env_value("OCTET_MODEL"),
        reasoning: env_value("OCTET_REASONING"),
        effect_policy: env_value("OCTET_EFFECT_POLICY"),
        reasoning_mode: env_value("OCTET_REASONING_MODE"),
        cache_retention: env_value("OCTET_CACHE_RETENTION")
            .or_else(|| env_value("PI_CACHE_RETENTION")),
        cache_warming: env_value("OCTET_CACHE_WARMING"),
        show_cache_miss_notices: None,
        theme: env_value("OCTET_THEME"),
        color: env_value("OCTET_COLOR"),
        mouse: env_value("OCTET_MOUSE"),
        tern: env_value("OCTET_TERN").or_else(|| env_value("OCTET_TUI_TERN")),
        // The interactive `/scoped-models` scope is a user-configuration
        // concern: the environment layer deliberately provides none of it, so a
        // headless run can never inherit an interactive selection by accident.
        models: None,
        plain: env_parse("OCTET_PLAIN")?,
        show_images: env_parse("OCTET_SHOW_IMAGES")?,
        allow_external_paths: env_parse("OCTET_ALLOW_EXTERNAL_PATHS")?,
        allow_edit: env_parse("OCTET_ALLOW_EDIT")?,
        allow_write: env_parse("OCTET_ALLOW_WRITE")?,
        allow_process: env_parse("OCTET_ALLOW_PROCESS")?,
        allow_shell: env_parse("OCTET_ALLOW_SHELL")?,
        allow_remote_read: env_parse("OCTET_ALLOW_REMOTE_READ")?,
        shell_path: env_value("OCTET_SHELL_PATH").map(PathBuf::from),
        bash_timeout_secs: env_parse("OCTET_BASH_TIMEOUT_SECS")?
            .or(env_parse("OCTET_EXEC_TIMEOUT_SECS")?),
        max_output_bytes: env_parse("OCTET_MAX_OUTPUT_BYTES")?,
        session_dir: env_value("OCTET_SESSION_DIR").map(PathBuf::from),
        max_turns: env_parse("OCTET_MAX_TURNS")?,
        max_cost_microdollars: env_parse("OCTET_MAX_COST_MICRODOLLARS")?,
        cost_warning_microdollars: env_parse("OCTET_COST_WARNING_MICRODOLLARS")?,
        context_files: env_parse("OCTET_CONTEXT_FILES")?,
        offline: env_parse("OCTET_OFFLINE")?,
        strict_config: env_parse("OCTET_STRICT_CONFIG")?,
        telemetry: env_value("OCTET_TELEMETRY").map(PathBuf::from),
        // Live reload is an interactive, user-level policy: `merge_project`
        // never copies it and the environment layer never arms it, so a
        // project or a stray variable cannot turn automatic reload on — and
        // host re-exec in particular has no environment switch at all.
        reload: None,
        reload_poll_ms: None,
        reload_debounce_ms: None,
        reload_max_files: None,
        reload_host: None,
        enabled_extensions: env_value("OCTET_EXTENSIONS").map(split_names),
        trusted_extensions: env_value("OCTET_TRUSTED_EXTENSIONS").map(split_names),
        system_prompt: env_value("OCTET_SYSTEM_PROMPT"),
        compaction: (compaction_mode.is_some()
            || compaction_enabled.is_some()
            || threshold_fraction.is_some()
            || max_active_tokens.is_some()
            || keep_recent_tokens.is_some()
            || keep_recent_turns.is_some()
            || compact_model.is_some())
        .then_some(CompactionLayer {
            mode: compaction_mode,
            enabled: compaction_enabled,
            threshold_fraction,
            max_active_tokens,
            keep_recent_tokens,
            keep_recent_turns,
            compact_model,
        }),
    })
}

#[cfg(test)]
fn environment_layer() -> anyhow::Result<ConfigLayer> {
    // Unit tests must never inherit provider, credential, session, or policy
    // state from the developer's real process environment.
    Ok(ConfigLayer::default())
}

fn policy_value_source(environment_set: bool, config_set: bool) -> PolicyValueSource {
    if environment_set {
        PolicyValueSource::Environment
    } else if config_set {
        PolicyValueSource::Config
    } else {
        PolicyValueSource::Default
    }
}

fn build_config_with_global_path(
    cli: Cli,
    cwd: &Path,
    global_path: Option<&Path>,
) -> anyhow::Result<Config> {
    build_config_with_global_path_and_diagnostics(cli, cwd, global_path, true)
}

fn build_config_with_global_path_and_diagnostics(
    cli: Cli,
    cwd: &Path,
    global_path: Option<&Path>,
    report_diagnostics: bool,
) -> anyhow::Result<Config> {
    let invocation_cwd = cwd.canonicalize()?;
    let workspace = config::resolve_workspace(cli.workspace.as_deref(), &invocation_cwd)?;
    if !invocation_cwd.starts_with(&workspace) {
        anyhow::bail!(
            "invocation directory {} is outside workspace {}",
            invocation_cwd.display(),
            workspace.display()
        );
    }

    let model_explicit = cli.model.is_some();
    let reasoning_explicit = cli.reasoning.is_some();
    let reasoning_mode_explicit = cli.reasoning_mode.is_some();

    // A missing home directory disables global config. Never reinterpret the
    // invocation directory as user scope: that would let an untrusted project
    // smuggle executable trust through `./.octet/config.toml`.
    let global = match global_path {
        Some(path) => read_layer(path, ConfigSourceKind::Global)?,
        None => LoadedConfigLayer::default(),
    };
    let project = if cli.workspace_trusted {
        read_layer(&project_config_path(&workspace), ConfigSourceKind::Project)?
    } else {
        LoadedConfigLayer::default()
    };
    let environment = environment_layer()?;
    let policy_environment = environment.clone();
    let extension_activation_overridden = project.values.enabled_extensions.is_some()
        || environment.enabled_extensions.is_some()
        || !cli.enable_extensions.is_empty();
    let mut diagnostics = global.diagnostics;
    diagnostics.extend(project.diagnostics);
    let mut values = global.values;
    values.merge_project(project.values);
    values.merge(environment);
    if report_diagnostics {
        report_config_diagnostics(
            &diagnostics,
            cli.strict_config || values.strict_config.unwrap_or(false),
        )?;
    }

    let model = resolve_model_id(
        cli.model.clone().map(octet_ai::ModelId),
        values.model.clone().map(octet_ai::ModelId),
        None,
    );
    let reasoning = cli
        .reasoning
        .as_deref()
        .or(values.reasoning.as_deref())
        .map(config::parse_reasoning)
        .transpose()?;
    let reasoning_mode = match cli
        .reasoning_mode
        .as_deref()
        .or(values.reasoning_mode.as_deref())
    {
        Some(value) => config::parse_reasoning_mode(value)?,
        None => octet_ai::ReasoningMode::Standard,
    };
    let cache_retention = match cli
        .cache_retention
        .as_deref()
        .or(values.cache_retention.as_deref())
    {
        Some(value) => config::parse_cache_retention(value)?,
        None => octet_ai::CacheRetention::Short,
    };
    let cache_warming = cli
        .cache_warming
        .as_deref()
        .or(values.cache_warming.as_deref())
        .map(config::parse_cache_warming)
        .transpose()?
        .unwrap_or_default();
    let color = match cli.color.as_deref().or(values.color.as_deref()) {
        Some(value) => ColorMode::parse(value)?,
        None => ColorMode::Auto,
    };
    let mouse = match cli.mouse.as_deref().or(values.mouse.as_deref()) {
        Some(value) => config::MouseMode::parse(value)?,
        None => config::MouseMode::Auto,
    };
    let tern = match cli.tern.as_deref().or(values.tern.as_deref()) {
        Some(value) => config::TernMode::parse(value)?,
        None => config::TernMode::Auto,
    };
    let system_prompt = cli.system_prompt.or(values.system_prompt);
    let effect_policy_source = if cli.safe_mode || cli.effect_policy.is_some() {
        PolicyValueSource::Cli
    } else {
        policy_value_source(
            policy_environment.effect_policy.is_some(),
            values.effect_policy.is_some(),
        )
    };
    let effect_policy = if cli.safe_mode {
        EffectPolicy::ControlledBashApproval
    } else {
        cli.effect_policy
            .as_deref()
            .or(values.effect_policy.as_deref())
            .map(str::parse)
            .transpose()
            .map_err(|error: String| anyhow::anyhow!(error))?
            .unwrap_or(EffectPolicy::UnsafeHost)
    };

    let offline_source = policy_value_source(
        policy_environment.offline.is_some(),
        values.offline.is_some(),
    );
    let mut sandbox = SandboxPolicy {
        policy_provenance: ToolPolicyProvenance {
            effect_policy: effect_policy_source,
            workspace_confinement: policy_value_source(
                policy_environment.allow_external_paths.is_some(),
                values.allow_external_paths.is_some(),
            ),
            allow_edit: policy_value_source(
                policy_environment.allow_edit.is_some(),
                values.allow_edit.is_some(),
            ),
            allow_write: policy_value_source(
                policy_environment.allow_write.is_some(),
                values.allow_write.is_some(),
            ),
            allow_process: policy_value_source(
                policy_environment.allow_process.is_some(),
                values.allow_process.is_some(),
            ),
            allow_shell: policy_value_source(
                policy_environment.allow_shell.is_some(),
                values.allow_shell.is_some(),
            ),
            shell_path: policy_value_source(
                policy_environment.shell_path.is_some(),
                values.shell_path.is_some(),
            ),
            bash_timeout: policy_value_source(
                policy_environment.bash_timeout_secs.is_some(),
                values.bash_timeout_secs.is_some(),
            ),
            max_output_bytes: policy_value_source(
                policy_environment.max_output_bytes.is_some(),
                values.max_output_bytes.is_some(),
            ),
            allow_remote_read: policy_value_source(
                policy_environment.allow_remote_read.is_some(),
                values.allow_remote_read.is_some(),
            ),
        },
        ..SandboxPolicy::default()
    };
    if let Some(value) = values.allow_external_paths {
        sandbox.allow_external_paths = value;
    }
    if let Some(value) = values.allow_edit {
        sandbox.allow_edit = value;
    }
    if let Some(value) = values.allow_write {
        sandbox.allow_write = value;
    }
    if let Some(value) = values.allow_process {
        sandbox.allow_process = value;
    }
    if let Some(value) = values.allow_shell {
        sandbox.allow_shell = value;
    }
    if let Some(value) = values.allow_remote_read {
        sandbox.allow_remote_read = value;
    }
    if let Some(value) = values.shell_path {
        sandbox.shell_path = Some(value);
    }
    if let Some(value) = values.bash_timeout_secs {
        sandbox.bash_timeout_secs = value;
    }
    if let Some(value) = values.max_output_bytes {
        sandbox.max_output_bytes = value;
    }
    if cli.no_edit {
        sandbox.allow_edit = false;
        sandbox.allow_write = false;
        sandbox.policy_provenance.allow_edit = PolicyValueSource::Cli;
        sandbox.policy_provenance.allow_write = PolicyValueSource::Cli;
    }
    if cli.no_write {
        sandbox.allow_write = false;
        sandbox.policy_provenance.allow_write = PolicyValueSource::Cli;
    }
    if cli.no_process || cli.no_shell {
        // Arbitrary process execution has shell-equivalent authority; these
        // flags are aliases at the enforcement boundary.
        sandbox.allow_process = false;
        sandbox.allow_shell = false;
        sandbox.policy_provenance.allow_process = PolicyValueSource::Cli;
        sandbox.policy_provenance.allow_shell = PolicyValueSource::Cli;
    }
    if cli.allow_shell {
        sandbox.allow_process = true;
        sandbox.allow_shell = true;
        sandbox.policy_provenance.allow_process = PolicyValueSource::Cli;
        sandbox.policy_provenance.allow_shell = PolicyValueSource::Cli;
    }
    if cli.allow_remote_read {
        sandbox.allow_remote_read = true;
        sandbox.policy_provenance.allow_remote_read = PolicyValueSource::Cli;
    }
    if let Some(value) = cli.shell_path {
        sandbox.shell_path = Some(value);
        sandbox.policy_provenance.shell_path = PolicyValueSource::Cli;
    }
    if let Some(value) = cli.bash_timeout_secs {
        sandbox.bash_timeout_secs = value;
        sandbox.policy_provenance.bash_timeout = PolicyValueSource::Cli;
    }
    if let Some(value) = cli.max_output_bytes {
        sandbox.max_output_bytes = value;
        sandbox.policy_provenance.max_output_bytes = PolicyValueSource::Cli;
    }
    let offline = cli.offline || values.offline.unwrap_or(false);
    if offline {
        sandbox.allow_remote_read = false;
        sandbox.policy_provenance.allow_remote_read = if cli.offline {
            PolicyValueSource::Cli
        } else {
            offline_source
        };
    }
    if effect_policy != octet_agent::EffectPolicy::UnsafeHost {
        // External-path classification cannot remain stable between admission
        // and execution. Keep controlled operations workspace-relative so the
        // broker's workspace/host distinction fails closed.
        sandbox.allow_external_paths = false;
        sandbox.policy_provenance.workspace_confinement = effect_policy_source;
    }
    sandbox.bash_timeout_secs = sandbox.bash_timeout_secs.clamp(1, 3_600);
    sandbox.max_output_bytes = sandbox.max_output_bytes.clamp(1_024, 1024 * 1024);

    let mut tools = match cli.tools {
        Some(names) => ToolPolicy::only(names)?,
        None if cli.no_tools => ToolPolicy::only(Vec::new())?,
        None => ToolPolicy::default(),
    };
    if cli.powershell {
        // Additive opt-in: this registers one extra allowlist entry and never
        // replaces or degrades `bash`. The tool itself remains Windows-gated.
        tools.include("powershell")?;
        if !cfg!(windows) {
            crate::output::stderr_line(
                "warning: --powershell is inert on this host; the PowerShell tool requires Windows",
            );
        }
    }
    for name in &cli.exclude_tools {
        tools.exclude(name)?;
    }
    if cli.no_edit {
        tools.exclude("edit")?;
        tools.exclude("write")?;
    }
    if cli.no_write {
        tools.exclude("write")?;
    }
    if cli.no_process || cli.no_shell {
        tools.exclude("bash")?;
    }
    if !sandbox.allow_edit {
        tools.exclude("edit")?;
    }
    if !sandbox.allow_write {
        tools.exclude("write")?;
    }
    if !(sandbox.allow_process && sandbox.allow_shell) {
        tools.exclude("bash")?;
    }

    let mut compaction = CompactionPolicy::default();
    if let Some(layer) = values.compaction {
        compaction.mode = match (layer.mode, layer.enabled) {
            (Some(value), _) => CompactionMode::parse(&value)?,
            (None, Some(true)) => CompactionMode::Local,
            (None, Some(false)) => CompactionMode::Disabled,
            (None, None) => compaction.mode,
        };
        if let Some(value) = layer.threshold_fraction {
            if !value.is_finite() || value <= 0.0 || value > 1.0 {
                anyhow::bail!("compaction.threshold_fraction must be greater than 0 and at most 1");
            }
            compaction.threshold_fraction = value;
        }
        if let Some(value) = layer.max_active_tokens {
            compaction.max_active_tokens = Some(value);
        }
        if let Some(value) = layer.keep_recent_tokens {
            compaction.keep_recent_tokens = value.max(1);
        } else if let Some(value) = layer.keep_recent_turns {
            const LEGACY_TOKENS_PER_TURN: u64 = 1_000;
            compaction.keep_recent_tokens = u64::try_from(value)
                .unwrap_or(u64::MAX)
                .saturating_mul(LEGACY_TOKENS_PER_TURN)
                .max(1);
        }
        if let Some(value) = layer.compact_model {
            let value = value.trim();
            if value.is_empty() {
                anyhow::bail!("compaction.compact_model must not be empty");
            }
            compaction.compact_model = Some(ModelId(value.to_owned()));
        }
    }

    let mode = match cli.mode.as_deref() {
        Some(value) if value.eq_ignore_ascii_case("rpc") => Mode::Rpc,
        Some(value) if value.eq_ignore_ascii_case("json") => Mode::Print {
            prompt: cli.message.clone().unwrap_or_default(),
        },
        Some(value) if value.eq_ignore_ascii_case("interactive") => Mode::Interactive,
        Some(value) => {
            anyhow::bail!(
                "invalid frontend mode {value:?}; use interactive, json or rpc (or --print)"
            )
        }
        None if cli.print => {
            let prompt = cli.message.clone().unwrap_or_default();
            if prompt.is_empty() && cli.prompt_template.is_none() {
                anyhow::bail!("--print requires a prompt or --prompt <template>");
            }
            Mode::Print { prompt }
        }
        None => Mode::Interactive,
    };
    let resume = if cli.continue_ {
        ResumeSelector::Continue
    } else if let Some(id) = cli.resume {
        ResumeSelector::Resume(id.and_then(|id| {
            let id = id.trim().to_owned();
            (!id.is_empty()).then_some(id)
        }))
    } else if let Some(id) = cli.fork {
        ResumeSelector::Fork(id.and_then(|id| {
            let id = id.trim().to_owned();
            (!id.is_empty()).then_some(id)
        }))
    } else {
        ResumeSelector::New
    };

    let mut enabled_extensions = values.enabled_extensions.unwrap_or_default();
    enabled_extensions.extend(cli.enable_extensions);
    let enabled_extensions = normalize_extension_names(enabled_extensions)?;
    let trusted_extensions =
        normalize_extension_trust_grants(values.trusted_extensions.unwrap_or_default())?;
    let invocation_trusted_extensions = normalize_extension_names(cli.trust_extensions)?;

    Ok(Config {
        workspace,
        invocation_cwd: invocation_cwd.clone(),
        model,
        model_explicit,
        reasoning,
        reasoning_explicit,
        reasoning_mode,
        reasoning_mode_explicit,
        cache_retention,
        cache_warming,
        show_cache_miss_notices: values.show_cache_miss_notices.unwrap_or(false),
        effect_policy,
        sandbox,
        theme: cli.theme.or(values.theme),
        system_prompt,
        theme_paths: cli.theme_dirs,
        color,
        mouse,
        plain: cli.plain || values.plain.unwrap_or(false),
        tern,
        show_images: cli.show_images || values.show_images.unwrap_or(false),
        session_dir: cli
            .session_dir
            .or(values.session_dir)
            .unwrap_or_else(config::default_session_dir),
        compaction,
        max_cost_microdollars: values.max_cost_microdollars,
        cost_warning_microdollars: values.cost_warning_microdollars,
        max_turns: {
            let raw = cli.max_turns.or(values.max_turns).unwrap_or(0);
            if raw == 0 {
                None
            } else {
                Some(raw.max(1))
            }
        },
        show_reasoning_in_print: cli.show_reasoning,
        initial_prompt: matches!(mode, Mode::Interactive)
            .then_some(cli.message)
            .flatten(),
        prompt_template: cli.prompt_template,
        debug_prompt: cli.debug_prompt,
        prompt_paths: cli.prompt_templates,
        mode,
        resume,
        skill_paths: cli.skill_dirs,
        extension_paths: cli.extension_dirs,
        enabled_extensions,
        extension_activation_overridden,
        trusted_extensions,
        invocation_trusted_extensions,
        start_extension_processes: true,
        experimental_streamable_http_mcp: cli.experimental_streamable_http_mcp,
        extension_flag_values: Default::default(),
        tools,
        telemetry: cli.telemetry.or(values.telemetry).map(|path| {
            if path.is_absolute() {
                path
            } else {
                invocation_cwd.join(path)
            }
        }),
        context_files: !cli.no_context_files && values.context_files.unwrap_or(true),
        offline,
        workspace_trusted: cli.workspace_trusted,
    })
}

/// Convert parsed CLI arguments into layered process configuration.
pub fn build_config(cli: Cli, cwd: &Path) -> anyhow::Result<Config> {
    let global = global_config_path();
    build_config_with_global_path(cli, cwd, global.as_deref())
}

#[cfg(test)]
mod tests;
