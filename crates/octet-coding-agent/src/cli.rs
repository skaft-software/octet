#![allow(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{
    Arg, ArgAction, Args, Command, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum,
};
use octet_agent::extension_process::{ExtensionFlag, ExtensionFlagType};
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
pub(crate) mod parity;

use config_diagnostics::{
    read_layer, report_config_diagnostics, ConfigSourceKind, LoadedConfigLayer,
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
    /// Launch the loopback-only octet Serve application.
    ///
    /// Default builds dispatch to the installed extension runtime; builds with
    /// the `serve` feature run the embedded implementation.
    Serve {
        /// Do not open the graphical client in the default browser.
        #[arg(long)]
        no_open: bool,
        /// Loopback TCP port. Zero asks the operating system for a free port.
        #[arg(long, default_value_t = 31415)]
        port: u16,
        /// Directory containing a development graphical shell.
        #[arg(long, value_name = "DIR")]
        web_root: Option<PathBuf>,
        /// Name for the first provisional session created by this Serve launch.
        /// Empty or whitespace-only input means "no name".
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
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
    /// Workspace root override.
    #[arg(long)]
    pub workspace: Option<PathBuf>,
    /// Terminal theme: auto, light, dark, or a discovered TOML theme name.
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
    /// trusts selected extensions. Does not enable them or bypass safe mode.
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
    /// Require approval for every bash call and keep host effects controlled.
    #[arg(long = "safe-mode", alias = "safe", conflicts_with = "effect_policy")]
    pub safe_mode: bool,
    /// Host-owned tool-effect admission profile: controlled,
    /// controlled_bash_approval, or unsafe_host.
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

#[derive(Clone, Debug)]
struct RegisteredExtensionFlag {
    extension: String,
    declaration: ExtensionFlag,
    argument_id: String,
    negative_argument_id: Option<String>,
}

type ExtensionFlagValues = BTreeMap<String, BTreeMap<String, serde_json::Value>>;

#[derive(Default)]
struct ExtensionFlagBootstrap {
    workspace: Option<PathBuf>,
    extension_dirs: Vec<PathBuf>,
    enable_extensions: Vec<String>,
    trust_extensions: Vec<String>,
    workspace_trusted: bool,
    safe_mode: bool,
    effect_policy: Option<String>,
}

fn collect_bootstrap_list(args: &[OsString], index: &mut usize, target: &mut Vec<String>) -> bool {
    let mut found = false;
    while *index < args.len() {
        let Some(value) = args[*index].to_str() else {
            return false;
        };
        if value == "--" || value.starts_with('-') {
            break;
        }
        target.extend(value.split(',').map(str::to_owned));
        *index += 1;
        found = true;
    }
    found
}

/// Extract only the options that select trusted extension manifests. This pass
/// intentionally does not use clap recovery: clap stops processing after an
/// unknown dynamic flag, which could hide later static activation options.
fn extension_flag_bootstrap(args: &[OsString]) -> Option<ExtensionFlagBootstrap> {
    let mut result = ExtensionFlagBootstrap::default();
    let mut index = 1;
    while index < args.len() {
        let value = args[index].to_str()?;
        if value == "--" {
            break;
        }
        if value == "--safe-mode" || value == "--safe" {
            result.safe_mode = true;
            index += 1;
            continue;
        }
        if let Some(policy) = value.strip_prefix("--effect-policy=") {
            result.effect_policy = Some(policy.to_owned());
            index += 1;
            continue;
        }
        if value == "--workspace-trusted" || value == "--trust-workspace" {
            result.workspace_trusted = true;
            index += 1;
            continue;
        }
        if let Some(path) = value.strip_prefix("--workspace=") {
            result.workspace = Some(PathBuf::from(path));
            index += 1;
            continue;
        }
        if let Some(path) = value.strip_prefix("--extension-dir=") {
            result.extension_dirs.push(PathBuf::from(path));
            index += 1;
            continue;
        }
        if let Some(names) = value.strip_prefix("--enable-extension=") {
            result
                .enable_extensions
                .extend(names.split(',').map(str::to_owned));
            index += 1;
            continue;
        }
        if let Some(names) = value.strip_prefix("--trust-extension=") {
            result
                .trust_extensions
                .extend(names.split(',').map(str::to_owned));
            index += 1;
            continue;
        }
        match value {
            "--effect-policy" => {
                index += 1;
                result.effect_policy = Some(args.get(index)?.to_str()?.to_owned());
                index += 1;
            }
            "--workspace" => {
                index += 1;
                result.workspace = Some(PathBuf::from(args.get(index)?.to_str()?));
                index += 1;
            }
            "--extension-dir" => {
                index += 1;
                result
                    .extension_dirs
                    .push(PathBuf::from(args.get(index)?.to_str()?));
                index += 1;
            }
            "--enable-extension" => {
                index += 1;
                if !collect_bootstrap_list(args, &mut index, &mut result.enable_extensions) {
                    return None;
                }
            }
            "--trust-extension" => {
                index += 1;
                if !collect_bootstrap_list(args, &mut index, &mut result.trust_extensions) {
                    return None;
                }
            }
            _ => index += 1,
        }
    }
    Some(result)
}

fn bootstrap_extension_config(args: &[OsString], cwd: &Path) -> Option<Config> {
    let bootstrap = extension_flag_bootstrap(args)?;
    let cli = Cli {
        workspace: bootstrap.workspace,
        extension_dirs: bootstrap.extension_dirs,
        enable_extensions: bootstrap.enable_extensions,
        trust_extensions: bootstrap.trust_extensions,
        workspace_trusted: bootstrap.workspace_trusted,
        safe_mode: bootstrap.safe_mode,
        effect_policy: bootstrap.effect_policy,
        ..Cli::default()
    };
    build_config_for_extension_flags(cli, cwd).ok()
}

/// Build enough layered configuration to select manifests without duplicating
/// user-visible configuration diagnostics during the real final parse.
fn build_config_for_extension_flags(cli: Cli, cwd: &Path) -> anyhow::Result<Config> {
    let global = global_config_path();
    build_config_with_global_path_and_diagnostics(cli, cwd, global.as_deref(), false)
}

fn collect_static_long_options(command: &Command, options: &mut BTreeSet<String>) {
    for argument in command.get_arguments() {
        if let Some(long) = argument.get_long() {
            options.insert(long.to_owned());
        }
        if let Some(aliases) = argument.get_all_aliases() {
            options.extend(aliases.into_iter().map(str::to_owned));
        }
    }
    for subcommand in command.get_subcommands() {
        collect_static_long_options(subcommand, options);
    }
}

fn register_extension_flags(
    declarations: Vec<(String, ExtensionFlag)>,
) -> anyhow::Result<Vec<RegisteredExtensionFlag>> {
    // clap adds these built-ins while building the command, after
    // `get_arguments()` exposes derive-declared arguments.
    let mut occupied = BTreeSet::from(["help".to_owned(), "version".to_owned()]);
    collect_static_long_options(&Cli::command(), &mut occupied);
    let mut registered = Vec::with_capacity(declarations.len());
    for (extension, declaration) in declarations {
        let argument_id = format!("extension-flag::{extension}::{}", declaration.name);
        let negative_argument_id = (declaration.kind == ExtensionFlagType::Boolean)
            .then(|| format!("{argument_id}::negative"));
        let mut spellings = vec![declaration.name.clone()];
        if declaration.kind == ExtensionFlagType::Boolean {
            spellings.push(format!("no-{}", declaration.name));
        }
        for spelling in spellings {
            if !occupied.insert(spelling.clone()) {
                anyhow::bail!(
                    "extension CLI flag --{spelling} from {extension:?} conflicts with an existing option"
                );
            }
        }
        registered.push(RegisteredExtensionFlag {
            extension,
            declaration,
            argument_id,
            negative_argument_id,
        });
    }
    Ok(registered)
}

fn extension_flag_command(registered: &[RegisteredExtensionFlag]) -> Command {
    let mut command = Cli::command();
    for flag in registered {
        let help = flag
            .declaration
            .description
            .clone()
            .unwrap_or_else(|| format!("Extension {} option", flag.extension));
        let argument = match flag.declaration.kind {
            ExtensionFlagType::Boolean => Arg::new(flag.argument_id.clone())
                .long(flag.declaration.name.clone())
                .action(ArgAction::SetTrue)
                .help(help)
                .conflicts_with(
                    flag.negative_argument_id
                        .as_ref()
                        .expect("boolean flags have an inverse ID"),
                ),
            ExtensionFlagType::String => Arg::new(flag.argument_id.clone())
                .long(flag.declaration.name.clone())
                .action(ArgAction::Set)
                .value_name("STRING")
                .help(help),
            ExtensionFlagType::Integer => Arg::new(flag.argument_id.clone())
                .long(flag.declaration.name.clone())
                .action(ArgAction::Set)
                .value_name("INTEGER")
                .allow_negative_numbers(true)
                .help(help),
        };
        command = command.arg(argument);
        if let Some(negative_id) = &flag.negative_argument_id {
            command = command.arg(
                Arg::new(negative_id.clone())
                    .long(format!("no-{}", flag.declaration.name))
                    .action(ArgAction::SetFalse)
                    .help(format!("Disable extension {} option", flag.extension))
                    .conflicts_with(&flag.argument_id),
            );
        }
    }
    command
}

fn supplied_by_command_line(matches: &clap::ArgMatches, id: &str) -> bool {
    matches
        .value_source(id)
        .is_some_and(|source| source == clap::parser::ValueSource::CommandLine)
}

fn resolve_extension_flag_values(
    matches: &clap::ArgMatches,
    registered: &[RegisteredExtensionFlag],
) -> anyhow::Result<ExtensionFlagValues> {
    let mut values: ExtensionFlagValues = BTreeMap::new();
    for flag in registered {
        let value = match flag.declaration.kind {
            ExtensionFlagType::Boolean if supplied_by_command_line(matches, &flag.argument_id) => {
                serde_json::Value::Bool(true)
            }
            ExtensionFlagType::Boolean
                if flag
                    .negative_argument_id
                    .as_deref()
                    .is_some_and(|id| supplied_by_command_line(matches, id)) =>
            {
                serde_json::Value::Bool(false)
            }
            ExtensionFlagType::Boolean => flag.declaration.default.clone(),
            ExtensionFlagType::String => matches
                .get_one::<String>(&flag.argument_id)
                .cloned()
                .map(serde_json::Value::String)
                .unwrap_or_else(|| flag.declaration.default.clone()),
            ExtensionFlagType::Integer => match matches.get_one::<String>(&flag.argument_id) {
                Some(raw) => {
                    let value = raw.parse::<i64>().map_err(|_| {
                        anyhow::anyhow!(
                            "extension CLI flag --{} requires an integer",
                            flag.declaration.name
                        )
                    })?;
                    serde_json::Value::Number(value.into())
                }
                None => flag.declaration.default.clone(),
            },
        };
        octet_agent::extension_process::validate_extension_flag_value(&flag.declaration, &value)
            .map_err(anyhow::Error::from)?;
        values
            .entry(flag.extension.clone())
            .or_default()
            .insert(flag.declaration.name.clone(), value);
    }
    Ok(values)
}

fn invocation_has_top_level_subcommand(
    args: &[OsString],
    registered: &[RegisteredExtensionFlag],
) -> bool {
    let command = Cli::command();
    let mut dynamic_flags = BTreeMap::<String, ExtensionFlagType>::new();
    for flag in registered {
        dynamic_flags.insert(flag.declaration.name.clone(), flag.declaration.kind);
        if flag.negative_argument_id.is_some() {
            dynamic_flags.insert(
                format!("no-{}", flag.declaration.name),
                ExtensionFlagType::Boolean,
            );
        }
    }

    let mut index = 1;
    while index < args.len() {
        let Some(value) = args[index].to_str() else {
            return false;
        };
        if value == "--" {
            return false;
        }
        if let Some(long) = value.strip_prefix("--") {
            let (name, inline_value) = long
                .split_once('=')
                .map_or((long, false), |(name, _)| (name, true));
            if let Some(argument) = static_long_argument(&command, name) {
                index += 1;
                if !inline_value {
                    consume_static_values(
                        args,
                        &mut index,
                        static_argument_value_maximum(argument),
                    );
                }
                continue;
            }
            if let Some(kind) = dynamic_flags.get(name) {
                index += 1;
                if !inline_value && *kind != ExtensionFlagType::Boolean {
                    let Some(next) = args.get(index).and_then(|value| value.to_str()) else {
                        continue;
                    };
                    let can_consume = match *kind {
                        ExtensionFlagType::Boolean => false,
                        ExtensionFlagType::String => next != "--" && !next.starts_with('-'),
                        ExtensionFlagType::Integer => {
                            next != "--" && (!next.starts_with('-') || next.parse::<i64>().is_ok())
                        }
                    };
                    if can_consume {
                        index += 1;
                    }
                }
                continue;
            }
            // An unregistered option is invalid to the old static parser. It
            // cannot safely consume a following word, so keep scanning for a
            // definite subcommand that must retain static behavior.
            index += 1;
            continue;
        }
        if value == "-p" {
            index += 1;
            continue;
        }
        if value.starts_with('-') {
            index += 1;
            continue;
        }
        return command.find_subcommand(value).is_some();
    }
    false
}

fn parse_static_or_exit(args: Vec<OsString>) -> Cli {
    match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    }
}

/// Parse the normal runtime command after adding trusted manifest-declared
/// flags. Discovery reads only bounded manifests and never launches extensions.
pub(crate) fn parse_with_extension_flags(
    args: Vec<OsString>,
    cwd: &Path,
) -> anyhow::Result<(Cli, ExtensionFlagValues)> {
    let static_args = args.clone();
    let declarations = bootstrap_extension_config(&args, cwd)
        .map(|config| crate::extensions::selected_extension_flag_declarations(&config))
        .unwrap_or_default();
    let registered = register_extension_flags(declarations)?;
    if invocation_has_top_level_subcommand(&args, &registered) {
        return Ok((parse_static_or_exit(static_args), BTreeMap::new()));
    }
    let mut command = extension_flag_command(&registered);
    let matches = match command.try_get_matches_from_mut(args) {
        Ok(matches) => matches,
        Err(error) => error.exit(),
    };
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => error.exit(),
    };
    if cli.command.is_some() || cli.login.is_some() || cli.logout.is_some() {
        // Dynamic flags are not part of early-exit command contracts. If an
        // unknown option occurred before one, preserve the old static parser's
        // behavior rather than quietly accepting it on that path.
        return Ok((parse_static_or_exit(static_args), BTreeMap::new()));
    }
    let values = resolve_extension_flag_values(&matches, &registered)?;
    Ok((cli, values))
}

fn static_long_argument<'a>(command: &'a Command, name: &str) -> Option<&'a Arg> {
    command.get_arguments().find(|argument| {
        argument.get_long() == Some(name)
            || argument
                .get_all_aliases()
                .is_some_and(|aliases| aliases.into_iter().any(|alias| alias == name))
    })
}

fn static_argument_value_maximum(argument: &Arg) -> usize {
    // clap exposes `None` for derive's implicit arity. A value-taking action
    // still consumes one value in that case.
    argument.get_num_args().map_or_else(
        || {
            if argument.get_action().takes_values() {
                1
            } else {
                0
            }
        },
        |range| range.max_values(),
    )
}

fn consume_static_values(args: &[OsString], index: &mut usize, maximum: usize) {
    let mut consumed = 0;
    while *index < args.len() && consumed < maximum {
        let Some(value) = args[*index].to_str() else {
            break;
        };
        if value == "--" || value.starts_with('-') {
            break;
        }
        *index += 1;
        consumed += 1;
    }
}

fn has_static_early_exit_option(args: &[OsString]) -> bool {
    for argument in args.iter().skip(1) {
        let Some(value) = argument.to_str() else {
            continue;
        };
        if value == "--" {
            break;
        }
        if value == "--version"
            || value.starts_with("--version=")
            || value == "-V"
            || value == "--login"
            || value.starts_with("--login=")
            || value == "--logout"
            || value.starts_with("--logout=")
        {
            return true;
        }
    }
    false
}

/// Dynamic extension flags are intentionally absent from early-exit commands.
///
/// This scans known static options rather than handing the raw invocation to
/// clap with error recovery. Recovery stops at an unknown dynamic flag and can
/// therefore misclassify later activation options or a positional prompt as a
/// subcommand.
pub(crate) fn uses_runtime_extension_flag_parser(args: &[OsString]) -> bool {
    if has_static_early_exit_option(args) {
        return false;
    }
    let command = Cli::command();
    let mut index = 1;
    while index < args.len() {
        let Some(value) = args[index].to_str() else {
            return true;
        };
        if value == "--" {
            return true;
        }
        if let Some(long) = value.strip_prefix("--") {
            let (name, inline_value) = long
                .split_once('=')
                .map_or((long, false), |(name, _)| (name, true));
            let Some(argument) = static_long_argument(&command, name) else {
                // It may be a dynamic flag. Keep parsing on the runtime path
                // rather than guessing how many following values it consumes.
                return true;
            };
            index += 1;
            if !inline_value {
                consume_static_values(args, &mut index, static_argument_value_maximum(argument));
            }
            continue;
        }
        if value == "-p" {
            index += 1;
            continue;
        }
        if value.starts_with('-') {
            // The remaining short forms are either help (which should show
            // registered flags) or invalid. Let the final parser decide.
            return true;
        }
        if command
            .get_subcommands()
            .any(|subcommand| subcommand.get_name() == value)
        {
            return false;
        }
        // The first non-option that is not a command is the normal prompt.
        return true;
    }
    true
}

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
    theme: Option<String>,
    color: Option<String>,
    mouse: Option<String>,
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
        override_some!(theme);
        override_some!(color);
        override_some!(mouse);
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
        Ok(builtin.key())
    } else if crate::resource_resolver::valid_resource_name(choice)
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
        theme: env_value("OCTET_THEME"),
        color: env_value("OCTET_COLOR"),
        mouse: env_value("OCTET_MOUSE"),
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
    let color = match cli.color.as_deref().or(values.color.as_deref()) {
        Some(value) => ColorMode::parse(value)?,
        None => ColorMode::Auto,
    };
    let mouse = match cli.mouse.as_deref().or(values.mouse.as_deref()) {
        Some(value) => config::MouseMode::parse(value)?,
        None => config::MouseMode::Auto,
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
        effect_policy,
        sandbox,
        theme: cli.theme.or(values.theme),
        system_prompt,
        theme_paths: cli.theme_dirs,
        color,
        mouse,
        plain: cli.plain || values.plain.unwrap_or(false),
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
mod tests {
    use super::*;

    fn cwd() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn base() -> Cli {
        Cli {
            command: None,
            message: None,
            additional_messages: Vec::new(),
            parity: Default::default(),
            login: None,
            logout: None,
            headless: false,
            mode: None,
            print: false,
            continue_: false,
            resume: None,
            fork: None,
            model: None,
            reasoning: None,
            reasoning_mode: None,
            cache_retention: None,
            workspace: None,
            theme: None,
            theme_dirs: vec![],
            color: None,
            mouse: None,
            plain: false,
            show_images: false,
            show_reasoning: false,
            max_turns: None,
            session_dir: None,
            prompt_template: None,
            debug_prompt: false,
            telemetry: None,
            prompt_templates: vec![],
            skill_dirs: vec![],
            extension_dirs: vec![],
            enable_extensions: vec![],
            trust_extensions: vec![],
            experimental_streamable_http_mcp: false,
            workspace_trusted: false,
            safe_mode: false,
            effect_policy: None,
            tools: None,
            powershell: false,
            exclude_tools: vec![],
            no_tools: false,
            no_edit: false,
            no_write: false,
            no_process: false,
            no_shell: false,
            allow_shell: false,
            allow_remote_read: false,
            shell_path: None,
            no_context_files: false,
            offline: false,
            strict_config: false,
            bash_timeout_secs: None,
            max_output_bytes: None,
            system_prompt: None,
        }
    }

    fn config_with_empty_global(cli: Cli, directory: &Path) -> anyhow::Result<Config> {
        build_config_with_global_path(cli, directory, Some(&directory.join("missing-global.toml")))
    }

    #[test]
    fn gpt_6_astra_uses_the_generic_cli_model_path() {
        let directory = cwd();
        let cli = Cli::try_parse_from(["octet", "--model", "gpt-6-astra", "--offline"]).unwrap();
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(config.model, Some(octet_ai::ModelId("gpt-6-astra".into())));
        assert!(config.model_explicit);

        let catalog = octet_ai::ModelCatalog::builtin().unwrap();
        let model = catalog.resolve(config.model.as_ref().unwrap()).unwrap();
        assert_eq!(model.spec.api_name, "gpt-6-astra");
        assert_eq!(model.endpoint.id.0, "openai");
    }

    #[test]
    fn codex_context_window_flag_parses_and_fails_closed_above_the_cap() {
        let parsed = Cli::try_parse_from([
            "octet",
            "--codex-context-window",
            "500000",
            "--codex-context-window-acknowledge-cost-cliff",
        ])
        .unwrap();
        assert!(parsed.parity.validate().is_ok());
        let override_ = parsed.parity.codex_context_override().unwrap();
        assert_eq!(override_.requested_tokens, Some(500_000));
        assert!(override_.acknowledge_cost_cliff);

        // Garbage is refused by clap before validation runs.
        assert!(Cli::try_parse_from(["octet", "--codex-context-window", "garbage"]).is_err());
        // Zero, oversized and above-cap-without-acknowledgement fail validation.
        let zero = Cli::try_parse_from(["octet", "--codex-context-window", "0"]).unwrap();
        assert!(zero.parity.validate().is_err(), "zero must fail closed");
        let oversized =
            Cli::try_parse_from(["octet", "--codex-context-window", "2000000"]).unwrap();
        assert!(
            oversized.parity.validate().is_err(),
            "above every entitlement must fail"
        );
        let unacknowledged =
            Cli::try_parse_from(["octet", "--codex-context-window", "500000"]).unwrap();
        let error = unacknowledged.parity.validate().unwrap_err();
        assert!(error.to_string().contains("double-priced"), "{error}");
        assert!(error.to_string().contains("websocket"), "{error}");
        // The acknowledgement flag requires the window flag.
        assert!(
            Cli::try_parse_from(["octet", "--codex-context-window-acknowledge-cost-cliff"])
                .is_err()
        );
        // No flag leaves the deliberate default untouched (no override).
        let absent = Cli::try_parse_from(["octet", "--offline"]).unwrap();
        assert!(absent.parity.validate().is_ok());
        assert!(absent.parity.codex_context_override().is_none());
    }

    fn extension_flag(
        name: &str,
        kind: ExtensionFlagType,
        default: serde_json::Value,
    ) -> ExtensionFlag {
        ExtensionFlag {
            name: name.to_owned(),
            kind,
            default,
            description: Some(format!("{name} extension option")),
        }
    }

    #[test]
    fn extension_flag_bootstrap_preserves_authority_options_after_dynamic_flags() {
        for safe in ["--safe-mode", "--safe"] {
            let args = [
                "octet",
                "--fixture-option",
                "--enable-extension",
                "fixture",
                safe,
            ]
            .map(OsString::from);
            let bootstrap = extension_flag_bootstrap(&args).unwrap();
            assert!(bootstrap.safe_mode);
            assert_eq!(bootstrap.enable_extensions, ["fixture"]);
            assert!(bootstrap.trust_extensions.is_empty());
        }
        for policy in ["unsafe_host", "controlled", "controlled_bash_approval"] {
            for options in [
                vec!["--effect-policy".to_owned(), policy.to_owned()],
                vec![format!("--effect-policy={policy}")],
            ] {
                let mut args = vec![OsString::from("octet"), OsString::from("--fixture-option")];
                args.extend(options.into_iter().map(OsString::from));
                let bootstrap = extension_flag_bootstrap(&args).unwrap();
                assert_eq!(bootstrap.effect_policy.as_deref(), Some(policy));
                assert!(!bootstrap.safe_mode);
            }
        }
        let args = ["octet", "--", "--safe-mode", "--effect-policy=controlled"].map(OsString::from);
        let bootstrap = extension_flag_bootstrap(&args).unwrap();
        assert!(!bootstrap.safe_mode);
        assert!(bootstrap.effect_policy.is_none());
    }

    #[test]
    fn extension_flags_parse_types_defaults_inverses_and_help() {
        let registered = register_extension_flags(vec![
            (
                "flag-fixture".to_owned(),
                extension_flag(
                    "fixture-enabled",
                    ExtensionFlagType::Boolean,
                    serde_json::json!(true),
                ),
            ),
            (
                "flag-fixture".to_owned(),
                extension_flag(
                    "fixture-label",
                    ExtensionFlagType::String,
                    serde_json::json!("default"),
                ),
            ),
            (
                "flag-fixture".to_owned(),
                extension_flag(
                    "fixture-count",
                    ExtensionFlagType::Integer,
                    serde_json::json!(2),
                ),
            ),
        ])
        .expect("register fixture flags");

        let default_matches = extension_flag_command(&registered)
            .try_get_matches_from(["octet"])
            .expect("parse default extension flags");
        let default_values =
            resolve_extension_flag_values(&default_matches, &registered).expect("resolve defaults");
        assert_eq!(
            default_values["flag-fixture"]["fixture-enabled"],
            serde_json::json!(true)
        );

        let matches = extension_flag_command(&registered)
            .try_get_matches_from([
                "octet",
                "--fixture-enabled",
                "--fixture-label",
                "custom",
                "--fixture-count",
                "-7",
            ])
            .expect("parse extension flags");
        assert!(Cli::from_arg_matches(&matches)
            .expect("project dynamic matches into the static CLI")
            .command
            .is_none());
        assert_eq!(
            resolve_extension_flag_values(&matches, &registered).expect("resolve values"),
            BTreeMap::from([(
                "flag-fixture".to_owned(),
                BTreeMap::from([
                    ("fixture-count".to_owned(), serde_json::json!(-7)),
                    ("fixture-enabled".to_owned(), serde_json::json!(true)),
                    ("fixture-label".to_owned(), serde_json::json!("custom")),
                ]),
            )])
        );

        let inverse_matches = extension_flag_command(&registered)
            .try_get_matches_from(["octet", "--no-fixture-enabled"])
            .expect("parse inverse boolean");
        let inverse_values = resolve_extension_flag_values(&inverse_matches, &registered)
            .expect("resolve inverse boolean");
        assert_eq!(
            inverse_values["flag-fixture"]["fixture-enabled"],
            serde_json::json!(false)
        );
        assert_eq!(
            inverse_values["flag-fixture"]["fixture-label"],
            serde_json::json!("default")
        );
        assert_eq!(
            inverse_values["flag-fixture"]["fixture-count"],
            serde_json::json!(2)
        );

        let mut command = extension_flag_command(&registered);
        let help = command.render_long_help().to_string();
        assert!(help.contains("--fixture-enabled"));
        assert!(help.contains("--no-fixture-enabled"));
        assert!(help.contains("--fixture-label <STRING>"));
        assert!(help.contains("--fixture-count <INTEGER>"));
    }

    #[test]
    fn extension_flags_reject_static_and_cross_extension_collisions() {
        for reserved in ["workspace", "help", "version"] {
            let static_collision = register_extension_flags(vec![(
                "fixture".to_owned(),
                extension_flag(reserved, ExtensionFlagType::String, serde_json::json!(".")),
            )])
            .expect_err("static option collision");
            assert!(static_collision.to_string().contains("conflicts"));
        }

        let dynamic_collision = register_extension_flags(vec![
            (
                "first".to_owned(),
                extension_flag(
                    "shared",
                    ExtensionFlagType::Boolean,
                    serde_json::json!(false),
                ),
            ),
            (
                "second".to_owned(),
                extension_flag("shared", ExtensionFlagType::String, serde_json::json!("")),
            ),
        ])
        .expect_err("cross-extension collision");
        assert!(dynamic_collision.to_string().contains("--shared"));
    }

    #[test]
    fn runtime_subcommand_scan_keeps_dynamic_flags_off_early_exit_paths() {
        let registered = register_extension_flags(vec![
            (
                "fixture".to_owned(),
                extension_flag(
                    "fixture-enabled",
                    ExtensionFlagType::Boolean,
                    serde_json::json!(false),
                ),
            ),
            (
                "fixture".to_owned(),
                extension_flag(
                    "fixture-label",
                    ExtensionFlagType::String,
                    serde_json::json!("default"),
                ),
            ),
            (
                "fixture".to_owned(),
                extension_flag(
                    "fixture-count",
                    ExtensionFlagType::Integer,
                    serde_json::json!(0),
                ),
            ),
        ])
        .expect("register fixture flags");
        let os = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();

        assert!(!invocation_has_top_level_subcommand(
            &os(&["octet", "--fixture-label", "migrate"]),
            &registered,
        ));
        assert!(invocation_has_top_level_subcommand(
            &os(&["octet", "--fixture-enabled", "migrate", "--help"]),
            &registered,
        ));
        assert!(invocation_has_top_level_subcommand(
            &os(&["octet", "--fixture-count", "-7", "migrate"]),
            &registered,
        ));
        assert!(invocation_has_top_level_subcommand(
            &os(&["octet", "--workspace", "/tmp", "migrate", "pi"]),
            &registered,
        ));
        assert!(invocation_has_top_level_subcommand(
            &os(&["octet", "--unknown-extension-flag", "migrate", "--help"]),
            &registered,
        ));
    }

    #[test]
    fn runtime_flag_parser_bypasses_only_true_early_exit_invocations() {
        let os = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();
        assert!(!uses_runtime_extension_flag_parser(&os(&[
            "octet", "--login", "codex"
        ])));
        assert!(!uses_runtime_extension_flag_parser(&os(&[
            "octet",
            "--workspace",
            "/tmp",
            "migrate",
            "pi",
        ])));
        assert!(!uses_runtime_extension_flag_parser(&os(&[
            "octet",
            "--version"
        ])));
        assert!(uses_runtime_extension_flag_parser(&os(&[
            "octet",
            "--extension-flag",
            "migrate",
        ])));
        assert!(!uses_runtime_extension_flag_parser(&os(&[
            "octet",
            "--extension-flag",
            "migrate",
            "--login",
            "codex",
        ])));
        assert!(uses_runtime_extension_flag_parser(&os(&[
            "octet", "--", "--login"
        ])));
        assert!(!uses_runtime_extension_flag_parser(&os(&[
            "octet", "migrate"
        ])));
    }

    #[test]
    fn cache_retention_can_disable_prompt_caching() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.cache_retention = Some("none".into());
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(config.cache_retention, octet_ai::CacheRetention::None);
    }

    #[test]
    fn colour_policy_resolves_from_cli() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.color = Some("never".into());
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(config.color, ColorMode::Never);
    }

    #[test]
    fn mouse_policy_defaults_to_terminal_ownership() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(config.mouse, config::MouseMode::Auto);
        assert!(!config.mouse.application_owned());

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.mouse = Some("app".into());
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(config.mouse, config::MouseMode::App);
        assert!(config.mouse.application_owned());
    }

    #[test]
    fn shell_path_and_bash_timeout_resolve_from_cli() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.shell_path = Some(PathBuf::from("/opt/homebrew/bin/bash"));
        cli.bash_timeout_secs = Some(45);
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(
            config.sandbox.shell_path,
            Some(PathBuf::from("/opt/homebrew/bin/bash"))
        );
        assert_eq!(config.sandbox.bash_timeout_secs, 45);
    }

    #[test]
    fn policy_value_source_prefers_environment_over_config() {
        assert_eq!(
            policy_value_source(false, false),
            PolicyValueSource::Default
        );
        assert_eq!(policy_value_source(false, true), PolicyValueSource::Config);
        assert_eq!(
            policy_value_source(true, false),
            PolicyValueSource::Environment
        );
        assert_eq!(
            policy_value_source(true, true),
            PolicyValueSource::Environment
        );
    }

    #[test]
    fn effective_policy_provenance_tracks_config_and_cli_precedence() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(
            &global,
            r#"
effect_policy = "controlled"
allow_external_paths = false
allow_edit = false
allow_write = false
allow_process = false
allow_shell = false
allow_remote_read = true
shell_path = "/opt/config/bash"
bash_timeout_secs = 30
max_output_bytes = 4096
"#,
        )
        .unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.allow_shell = true;
        cli.bash_timeout_secs = Some(45);
        cli.max_output_bytes = Some(8192);

        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        let policy = config
            .sandbox
            .effective_tool_policy(&config.workspace, config.effect_policy);

        assert_eq!(
            policy.workspace_confinement.source,
            PolicyValueSource::Config
        );
        assert!(policy.workspace_confinement.value);
        assert_eq!(policy.effect_policy.value, EffectPolicy::Controlled);
        assert_eq!(policy.effect_policy.source, PolicyValueSource::Config);
        assert!(!policy.allow_edit.value);
        assert_eq!(policy.allow_edit.source, PolicyValueSource::Config);
        assert!(!policy.allow_write.value);
        assert_eq!(policy.allow_write.source, PolicyValueSource::Config);
        assert_eq!(policy.allow_process.source, PolicyValueSource::Cli);
        assert!(policy.allow_process.value);
        assert_eq!(policy.allow_shell.source, PolicyValueSource::Cli);
        assert!(policy.allow_shell.value);
        assert_eq!(policy.shell_path.source, PolicyValueSource::Config);
        assert_eq!(
            policy.shell_path.value.selection,
            octet_agent::ShellSelection::Configured
        );
        assert_eq!(policy.bash_timeout_ms.source, PolicyValueSource::Cli);
        assert_eq!(policy.bash_timeout_ms.value, 45_000);
        assert_eq!(policy.max_output_bytes.source, PolicyValueSource::Cli);
        assert_eq!(policy.max_output_bytes.value, 8192);
        assert_eq!(policy.allow_remote_read.source, PolicyValueSource::Config);
        assert!(policy.allow_remote_read.value);
    }

    #[test]
    fn telemetry_path_resolves_relative_to_invocation_directory() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.telemetry = Some(PathBuf::from("metrics/run.jsonl"));
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(
            config.telemetry,
            Some(
                directory
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join("metrics/run.jsonl")
            )
        );
    }

    #[test]
    fn remote_reads_are_default_off_and_require_user_level_opt_in() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert!(!config.sandbox.allow_remote_read);

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.allow_remote_read = true;
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert!(config.sandbox.allow_remote_read);

        let global = directory.path().join("global.toml");
        std::fs::write(&global, "allow_remote_read = true\n").unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert!(config.sandbox.allow_remote_read);
    }

    #[test]
    fn experimental_streamable_http_mcp_is_cli_only() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        assert!(
            !config_with_empty_global(cli, directory.path())
                .unwrap()
                .experimental_streamable_http_mcp
        );

        let global = directory.path().join("global.toml");
        std::fs::write(&global, "experimental_streamable_http_mcp = true\n").unwrap();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        std::fs::write(
            directory.path().join(".octet/config.toml"),
            "experimental_streamable_http_mcp = true\n",
        )
        .unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        assert!(
            !build_config_with_global_path(cli, directory.path(), Some(&global))
                .unwrap()
                .experimental_streamable_http_mcp,
            "global and trusted-project configuration cannot grant the experimental transport"
        );

        assert!(
            Cli::try_parse_from(["octet", "--experimental-streamable-http-m"]).is_err(),
            "the process-owner gate must not accept an abbreviated spelling"
        );
        let mut cli = Cli::try_parse_from(["octet", "--experimental-streamable-http-mcp"]).unwrap();
        cli.workspace = Some(directory.path().into());
        assert!(
            config_with_empty_global(cli, directory.path())
                .unwrap()
                .experimental_streamable_http_mcp
        );
    }

    #[test]
    fn project_config_cannot_grant_remote_network_authority_and_offline_revokes_it() {
        let directory = cwd();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        std::fs::write(
            directory.path().join(".octet/config.toml"),
            "allow_remote_read = true\n",
        )
        .unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert!(!config.sandbox.allow_remote_read);

        let global = directory.path().join("global.toml");
        std::fs::write(&global, "allow_remote_read = true\noffline = true\n").unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert!(config.offline);
        assert!(!config.sandbox.allow_remote_read);
    }

    #[test]
    fn effect_policy_layers_respect_project_tightening_and_cli_override() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, "effect_policy = 'controlled'\n").unwrap();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        let project = directory.path().join(".octet/config.toml");
        std::fs::write(&project, "effect_policy = 'unsafe_host'\n").unwrap();

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.effect_policy, EffectPolicy::Controlled);
        assert_eq!(
            config
                .sandbox
                .effective_tool_policy(&config.workspace, config.effect_policy)
                .effect_policy
                .source,
            PolicyValueSource::Config
        );

        std::fs::write(&project, "effect_policy = 'controlled_bash_approval'\n").unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(
            config.effect_policy,
            EffectPolicy::ControlledBashApproval,
            "a trusted project may tighten the global profile"
        );

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        cli.effect_policy = Some("unsafe_host".into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.effect_policy, EffectPolicy::UnsafeHost);
        assert_eq!(
            config
                .sandbox
                .effective_tool_policy(&config.workspace, config.effect_policy)
                .effect_policy
                .source,
            PolicyValueSource::Cli
        );
    }

    #[test]
    fn environment_effect_policy_layer_overrides_config() {
        let mut values = ConfigLayer {
            effect_policy: Some("controlled".into()),
            ..ConfigLayer::default()
        };
        values.merge(ConfigLayer {
            effect_policy: Some("unsafe_host".into()),
            ..ConfigLayer::default()
        });

        assert_eq!(values.effect_policy.as_deref(), Some("unsafe_host"));
        assert_eq!(
            policy_value_source(true, values.effect_policy.is_some()),
            PolicyValueSource::Environment
        );
    }

    #[test]
    fn effect_policy_is_full_access_by_default_and_yolo_is_removed() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(config.effect_policy, octet_agent::EffectPolicy::UnsafeHost);
        assert!(config.sandbox.allow_external_paths);
        assert!(config.enabled_extensions.is_empty());
        assert!(config.trusted_extensions.is_empty());
        assert!(config.invocation_trusted_extensions.is_empty());
        assert!(Cli::try_parse_from(["octet", "--yolo"]).is_err());
    }

    #[test]
    fn safe_mode_uses_the_controlled_approval_profile() {
        let directory = cwd();

        let mut cli = Cli::try_parse_from(["octet", "--safe-mode"]).unwrap();
        assert!(cli.safe_mode);
        cli.workspace = Some(directory.path().into());
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(
            config.effect_policy,
            octet_agent::EffectPolicy::ControlledBashApproval
        );

        let cli = Cli::try_parse_from(["octet", "--safe"]).unwrap();
        assert!(cli.safe_mode);
        assert!(
            Cli::try_parse_from(["octet", "--safe-mode", "--effect-policy", "unsafe_host",])
                .is_err()
        );
    }

    #[tokio::test]
    async fn safe_mode_extension_selection_preserves_every_bash_approval() {
        let directory = cwd();
        for explicit_trust in [false, true] {
            let mut cli = base();
            cli.safe_mode = true;
            cli.enable_extensions.push("fixture".into());
            if explicit_trust {
                cli.trust_extensions.push("fixture".into());
            }
            let config = config_with_empty_global(cli, directory.path()).unwrap();
            assert_eq!(config.effect_policy, EffectPolicy::ControlledBashApproval);
            assert!(config.sandbox.process_execution_allowed());
            assert!(!config.sandbox.allow_external_paths);
            let broker = octet_agent::EffectBroker::new(config.effect_policy);
            for command in ["ls", "printf changed > file.txt"] {
                let intent = octet_agent::EffectIntent::new(
                    "principal",
                    "run",
                    1,
                    "call",
                    "bash",
                    octet_agent::ToolEffect::HostProcess,
                    serde_json::json!({"command": command}),
                )
                .unwrap();
                assert!(matches!(
                    broker.authorize(&intent, None).await,
                    Err(octet_agent::EffectBrokerError::ApprovalUnavailable { .. })
                ));
            }
            let extension_intent = octet_agent::EffectIntent::new(
                "principal",
                "run",
                1,
                "extension-call",
                "fixture",
                octet_agent::ToolEffect::Extension,
                serde_json::json!({}),
            )
            .unwrap();
            assert!(matches!(
                broker.authorize(&extension_intent, None).await,
                Err(octet_agent::EffectBrokerError::Denied { .. })
            ));
        }
    }

    #[test]
    fn invalid_effect_policy_is_secret_safe() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.effect_policy = Some("sensitive-invalid-policy-value".into());

        let error = config_with_empty_global(cli, directory.path())
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid effect policy"));
        assert!(!error.contains("sensitive-invalid-policy-value"));
    }

    #[test]
    fn safe_mode_forces_workspace_only_paths() {
        let directory = cwd();
        assert!(SandboxPolicy::default().allow_external_paths);

        let global = directory.path().join("global.toml");
        std::fs::write(&global, "allow_external_paths = true\n").unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.effect_policy, octet_agent::EffectPolicy::UnsafeHost);
        assert!(config.sandbox.allow_external_paths);

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.safe_mode = true;
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(
            config.effect_policy,
            octet_agent::EffectPolicy::ControlledBashApproval
        );
        assert!(!config.sandbox.allow_external_paths);
    }

    #[test]
    fn legacy_host_authority_config_does_not_select_a_policy() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, "unsafe_host_effects = false\n").unwrap();

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.effect_policy, octet_agent::EffectPolicy::UnsafeHost);
    }

    #[test]
    fn print_mode_requires_prompt_text() {
        let directory = cwd();
        let mut cli = base();
        cli.print = true;
        cli.model = Some("m".into());
        cli.workspace = Some(directory.path().into());
        assert!(config_with_empty_global(cli, directory.path()).is_err());
    }

    #[test]
    fn print_mode_builds_print_config() {
        let directory = cwd();
        let mut cli = base();
        cli.message = Some("hi".into());
        cli.print = true;
        cli.model = Some("m".into());
        cli.workspace = Some(directory.path().into());
        cli.show_reasoning = true;
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert!(matches!(config.mode, Mode::Print { prompt } if prompt == "hi"));
        assert!(config.show_reasoning_in_print);
    }

    #[test]
    fn continue_sets_resume_selector_and_interactive_mode() {
        let directory = cwd();
        let mut cli = base();
        cli.continue_ = true;
        cli.workspace = Some(directory.path().into());
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert!(matches!(config.resume, ResumeSelector::Continue));
        assert!(matches!(config.mode, Mode::Interactive));
    }

    #[test]
    fn clap_parses_fork_and_rejects_resume_conflicts() {
        let parsed = Cli::try_parse_from(["octet", "--fork", "source-id"]).unwrap();
        assert_eq!(parsed.fork, Some(Some("source-id".into())));
        assert!(Cli::try_parse_from(["octet", "--fork", "--resume"]).is_err());
        assert!(Cli::try_parse_from(["octet", "--fork", "--continue"]).is_err());
    }

    #[test]
    fn fork_without_an_id_is_distinct_from_fork_by_id() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.fork = Some(None);
        assert!(matches!(
            config_with_empty_global(cli, directory.path())
                .unwrap()
                .resume,
            ResumeSelector::Fork(None)
        ));

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.fork = Some(Some("session-id".into()));
        assert!(matches!(
            config_with_empty_global(cli, directory.path())
                .unwrap()
                .resume,
            ResumeSelector::Fork(Some(id)) if id == "session-id"
        ));
    }

    #[test]
    fn unset_reasoning_is_distinct_from_explicit_off() {
        let directory = cwd();
        let config = config_with_empty_global(base(), directory.path()).unwrap();
        assert_eq!(config.reasoning, None);
        assert!(!config.reasoning_explicit);

        let mut cli = base();
        cli.reasoning = Some("off".into());
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(config.reasoning, Some(octet_ai::ReasoningConfig::Off));
        assert!(config.reasoning_explicit);
    }

    #[test]
    fn reasoning_is_parsed_and_invalid_values_fail() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.reasoning = Some("off".into());
        assert!(config_with_empty_global(cli, directory.path()).is_ok());

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.reasoning = Some("budget=2048".into());
        assert!(config_with_empty_global(cli, directory.path()).is_ok());

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.reasoning_mode = Some("pro".into());
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(config.reasoning_mode, octet_ai::ReasoningMode::Pro);
        assert!(config.reasoning_mode_explicit);

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.reasoning = Some("nonsense".into());
        assert!(config_with_empty_global(cli, directory.path()).is_err());

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.reasoning_mode = Some("turbo".into());
        assert!(config_with_empty_global(cli, directory.path()).is_err());
    }

    #[test]
    fn resume_without_an_id_is_distinct_from_resume_by_id() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.resume = Some(None);
        assert!(matches!(
            config_with_empty_global(cli, directory.path())
                .unwrap()
                .resume,
            ResumeSelector::Resume(None)
        ));
    }

    #[test]
    fn strict_config_flag_is_parsed() {
        let cli = Cli::try_parse_from(["octet", "--strict-config"]).unwrap();
        assert!(cli.strict_config);
    }

    #[test]
    fn cli_overrides_project_which_overrides_global() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(
            &global,
            "model = 'global'\ntheme = 'global-theme'\nmax_turns = 7\n",
        )
        .unwrap();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        std::fs::write(
            directory.path().join(".octet/config.toml"),
            "model = 'project'\ntheme = 'project-theme'\nmax_turns = 9\nallow_external_paths = false\n",
        )
        .unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        cli.model = Some("cli".into());
        cli.max_turns = Some(11);
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.model.as_ref().unwrap().0, "cli");
        assert!(config.model_explicit);
        assert!(!config.reasoning_explicit);
        assert_eq!(config.theme.as_deref(), Some("project-theme"));
        assert_eq!(config.max_turns, Some(11));
        assert!(!config.sandbox.allow_external_paths);
    }

    #[test]
    fn telemetry_uses_cli_then_project_then_global_precedence() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, "telemetry = 'global.jsonl'\n").unwrap();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        std::fs::write(
            directory.path().join(".octet/config.toml"),
            "telemetry = 'project.jsonl'\n",
        )
        .unwrap();

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        cli.telemetry = Some("cli.jsonl".into());
        let canonical = directory.path().canonicalize().unwrap();
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.telemetry, Some(canonical.join("cli.jsonl")));

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.telemetry, Some(canonical.join("project.jsonl")));

        std::fs::remove_file(directory.path().join(".octet/config.toml")).unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.telemetry, Some(canonical.join("global.jsonl")));
    }

    #[test]
    fn system_prompt_layered_precedence_prefers_cli_over_project_then_global() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, "system_prompt = 'global'\n").unwrap();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        std::fs::write(
            directory.path().join(".octet/config.toml"),
            "system_prompt = 'project'\n",
        )
        .unwrap();

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        cli.system_prompt = Some("cli".into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.system_prompt.as_deref(), Some("cli"));

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.system_prompt.as_deref(), Some("project"));
    }

    #[test]
    fn system_prompt_explicit_empty_cli_value_is_preserved() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, "system_prompt = 'global'\n").unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.system_prompt = Some("".into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.system_prompt.as_deref(), Some(""));
    }

    #[test]
    fn parse_system_prompt_flag_without_value() {
        let cli =
            Cli::try_parse_from(["octet", "--system-prompt", "--print", "--prompt", "review"])
                .unwrap();
        assert!(cli.system_prompt.is_some());
        assert_eq!(cli.system_prompt.as_deref(), Some(""));
    }

    #[test]
    fn trusted_project_may_tighten_but_never_relax_global_authority() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, "allow_write = false\nallow_edit = true\n").unwrap();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        std::fs::write(
            directory.path().join(".octet/config.toml"),
            "allow_write = true\nallow_edit = false\n",
        )
        .unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert!(!config.sandbox.allow_write);
        assert!(!config.sandbox.allow_edit);
    }

    #[test]
    fn trusted_project_may_enable_but_cannot_trust_an_executable_extension() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(
            &global,
            "enabled_extensions = ['user-tool']\ntrusted_extensions = ['user-tool']\n",
        )
        .unwrap();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        std::fs::write(
            directory.path().join(".octet/config.toml"),
            "enabled_extensions = ['project-tool']\ntrusted_extensions = ['project-tool']\n",
        )
        .unwrap();

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();

        assert_eq!(config.enabled_extensions, vec!["project-tool"]);
        assert!(config.extension_activation_overridden);
        assert_eq!(config.trusted_extensions, vec!["user-tool"]);
        assert!(config.invocation_trusted_extensions.is_empty());
    }

    #[test]
    fn activation_menu_revalidates_a_project_override_added_after_startup() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, "enabled_extensions = ['global-tool']\n").unwrap();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert!(!config.extension_activation_overridden);
        assert!(extension_activation_menu_authoritative(&config).unwrap());

        std::fs::write(
            directory.path().join(".octet/config.toml"),
            "enabled_extensions = ['project-tool']\n",
        )
        .unwrap();
        assert!(!extension_activation_menu_authoritative(&config).unwrap());

        std::fs::write(
            directory.path().join(".octet/config.toml"),
            "not valid = [\n",
        )
        .unwrap();
        assert!(extension_activation_menu_authoritative(&config).is_err());
    }

    #[test]
    fn unavailable_home_never_loads_project_config_as_global_config() {
        let directory = cwd();
        std::fs::create_dir_all(directory.path().join(".octet")).unwrap();
        std::fs::write(
            directory.path().join(".octet/config.toml"),
            "enabled_extensions = ['project-tool']\ntrusted_extensions = ['project-tool']\n",
        )
        .unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());

        let config = build_config_with_global_path(cli, directory.path(), None).unwrap();

        assert!(config.enabled_extensions.is_empty());
        assert!(!config.extension_activation_overridden);
        assert!(config.trusted_extensions.is_empty());
        assert!(config.invocation_trusted_extensions.is_empty());
    }

    #[test]
    fn relative_home_is_not_a_global_config_root() {
        assert_eq!(global_config_path_from_home(None), None);
        assert_eq!(global_config_path_from_home(Some(".".into())), None);
        let absolute_home = std::env::temp_dir().join("octet-home");
        assert_eq!(
            global_config_path_from_home(Some(absolute_home.clone())),
            Some(absolute_home.join(".octet/config.toml"))
        );
    }

    #[test]
    fn cli_activation_marks_the_interactive_user_config_menu_non_authoritative() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, "enabled_extensions = ['global-tool']\n").unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.enable_extensions.push("cli-tool".into());

        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();

        assert_eq!(config.enabled_extensions, ["cli-tool", "global-tool"]);
        assert!(config.extension_activation_overridden);
    }

    #[test]
    fn extension_name_lists_are_normalized_and_deduplicated() {
        let names =
            normalize_extension_names(split_names("Git-Tools, local-model,git-tools".to_owned()))
                .unwrap();
        assert_eq!(names, vec!["git-tools", "local-model"]);
    }

    #[test]
    fn persistent_extension_trust_grants_preserve_exact_source_paths() {
        let grants = normalize_extension_trust_grants([
            "Git-Tools".to_owned(),
            "git-tools@/workspace/.octet/extensions/git-tools/extension.toml".to_owned(),
            "git-tools@/Volumes/dev@home/git-tools/extension.toml".to_owned(),
            " Git-Tools ".to_owned(),
        ])
        .unwrap();
        assert_eq!(
            grants,
            vec![
                "git-tools",
                "git-tools@/Volumes/dev@home/git-tools/extension.toml",
                "git-tools@/workspace/.octet/extensions/git-tools/extension.toml",
            ]
        );
    }

    #[test]
    fn persistent_source_trust_rejects_relative_paths() {
        let error = normalize_extension_trust_grants([
            "git-tools@.octet/extensions/git-tools/extension.toml".to_owned(),
        ])
        .unwrap_err();
        assert!(error.to_string().contains("absolute path"));
    }

    #[test]
    fn cli_extension_trust_is_kept_one_shot() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(&global, "trusted_extensions = ['global-tool']\n").unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.trust_extensions = vec!["Project-Tool".into()];

        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();

        assert_eq!(config.trusted_extensions, vec!["global-tool"]);
        assert_eq!(config.invocation_trusted_extensions, vec!["project-tool"]);
    }

    #[test]
    fn no_edit_and_explicit_allowlists_match_the_provider_tool_surface() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.no_edit = true;
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert!(!config.sandbox.allow_edit);
        assert!(!config.sandbox.allow_write);
        assert!(!config.tools.enabled("edit"));
        assert!(!config.tools.enabled("write"));

        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.tools = Some(vec!["read".into(), "search".into()]);
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        assert_eq!(
            config.tools.names().collect::<Vec<_>>(),
            vec!["read", "search"]
        );
    }

    #[test]
    fn cost_and_compaction_settings_merge_from_layered_toml() {
        let global: ConfigLayer = toml::from_str(
            "max_cost_microdollars = 100\ncost_warning_microdollars = 25\n[compaction]\nenabled = false\nmax_active_tokens = 272000\ncompact_model = 'cheap'",
        )
        .unwrap();
        let project: ConfigLayer = toml::from_str(
            "cost_warning_microdollars = 40\n[compaction]\nmax_active_tokens = 200000\nkeep_recent_tokens = 2",
        )
        .unwrap();
        let mut merged = global;
        merged.merge(project);
        assert_eq!(merged.max_cost_microdollars, Some(100));
        assert_eq!(merged.cost_warning_microdollars, Some(40));
        let compaction = merged.compaction.unwrap();
        assert_eq!(compaction.enabled, Some(false));
        assert_eq!(compaction.compact_model.as_deref(), Some("cheap"));
        assert_eq!(compaction.max_active_tokens, Some(200_000));
        assert_eq!(compaction.keep_recent_tokens, Some(2));
    }

    #[test]
    fn explicit_compaction_mode_and_legacy_enabled_map_without_silent_fallback() {
        let directory = cwd();
        let global = directory.path().join("global.toml");
        std::fs::write(
            &global,
            "[compaction]\nmode = 'native-responses'\nmax_active_tokens = 0\n",
        )
        .unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.compaction.mode, CompactionMode::NativeResponses);
        assert_eq!(config.compaction.max_active_tokens, Some(0));

        std::fs::write(&global, "[compaction]\nenabled = true\n").unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.compaction.mode, CompactionMode::Local);

        std::fs::write(&global, "[compaction]\nenabled = false\n").unwrap();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        let config = build_config_with_global_path(cli, directory.path(), Some(&global)).unwrap();
        assert_eq!(config.compaction.mode, CompactionMode::Disabled);
    }

    // --- extension activation persistence ---

    #[test]
    fn persist_extension_activation_changes_only_the_selected_user_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep this comment\nenabled_extensions = [\"octet-browse\", \"octet-ssh\"]\ntrusted_extensions = [\"octet-browse\", \"octet-ssh\"]\n",
        )
        .unwrap();

        assert_eq!(
            persist_extension_enabled_to_path("octet-web-search", true, &path).unwrap(),
            vec!["octet-browse", "octet-ssh", "octet-web-search"]
        );
        assert_eq!(
            persist_extension_enabled_to_path("octet-browse", false, &path).unwrap(),
            vec!["octet-ssh", "octet-web-search"]
        );

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        let enabled = parsed["enabled_extensions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(enabled, ["octet-ssh", "octet-web-search"]);
        assert_eq!(
            parsed["trusted_extensions"].as_array().unwrap().len(),
            2,
            "trust is an independent decision"
        );
        assert!(content.contains("# keep this comment"), "{content}");
    }

    #[cfg(unix)]
    #[test]
    fn atomic_config_update_preserves_existing_permissions_and_uses_private_new_files() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("existing.toml");
        std::fs::write(&existing, "enabled_extensions = []\n").unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o640)).unwrap();
        persist_extension_enabled_to_path("octet-ssh", true, &existing).unwrap();
        assert_eq!(
            std::fs::metadata(&existing).unwrap().permissions().mode() & 0o777,
            0o640
        );

        let new = dir.path().join("new.toml");
        persist_extension_enabled_to_path("octet-ssh", true, &new).unwrap();
        assert_eq!(
            std::fs::metadata(&new).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let staging_files = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
            .count();
        assert_eq!(staging_files, 0, "atomic staging files must be removed");
    }

    #[test]
    fn atomic_config_publish_rejects_a_non_locking_external_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "enabled_extensions = [\"octet-browse\"]\n";
        let external =
            "enabled_extensions = [\"octet-ssh\"]\ntrusted_extensions = [\"octet-ssh\"]\n";
        std::fs::write(&path, original).unwrap();
        let expected = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, external).unwrap();

        let error = write_config_atomically(&path, "enabled_extensions = []\n", Some(&expected))
            .unwrap_err();

        assert!(error.to_string().contains("changed"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), external);
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            !name.contains(".tmp-") && !name.contains(".octet-tmp-")
        }));
    }

    #[test]
    fn concurrent_config_update_fails_without_rewriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = "enabled_extensions = [\"octet-browse\"]\n";
        std::fs::write(&path, original).unwrap();
        let lock = config_update_lock(&path).unwrap();

        let error = persist_extension_enabled_to_path("octet-ssh", true, &path).unwrap_err();
        assert!(
            error.to_string().contains("another config update"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        drop(lock);

        assert_eq!(
            persist_extension_enabled_to_path("octet-ssh", true, &path).unwrap(),
            ["octet-browse", "octet-ssh"]
        );
    }

    #[test]
    fn persist_extension_activation_rejects_a_non_array_without_rewriting() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let invalid = "enabled_extensions = \"octet-browse\"\n";
        std::fs::write(&path, invalid).unwrap();

        assert!(persist_extension_enabled_to_path("octet-ssh", true, &path).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), invalid);
    }

    // --- persist_bool_key_to_path / remove_key_from_path ---

    #[test]
    fn bool_settings_persist_as_toml_booleans_and_removable_keys_disappear() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        persist_bool_key_to_path("show_images", true, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let document = content.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            document["show_images"].as_bool(),
            Some(true),
            "boolean settings must not round-trip as strings: {content}"
        );
        persist_bool_key_to_path("show_images", false, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let document = content.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(document["show_images"].as_bool(), Some(false));

        // A string-valued key round-trips, then removal restores the rest of
        // the document untouched.
        persist_key_to_path("models", "a:high,b", &path).unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("models = \"a:high,b\""));
        remove_key_from_path("models", &path).unwrap();
        let remaining = std::fs::read_to_string(&path).unwrap();
        assert!(!remaining.contains("models"), "{remaining}");
        assert!(remaining.contains("show_images = false"), "{remaining}");
        // Removing a key from a missing file is already the desired state.
        remove_key_from_path("models", &dir.path().join("absent.toml")).unwrap();
    }

    // --- persist_model_to_path ---

    fn read_model_from_config(path: &std::path::Path) -> Option<String> {
        let source = std::fs::read_to_string(path).unwrap();
        for line in source.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                continue;
            }
            if let Some(after) = trimmed.strip_prefix("model") {
                let after = after.trim_start();
                if let Some(val) = after.strip_prefix('=') {
                    return Some(val.trim().trim_matches('"').to_string());
                }
            }
        }
        None
    }

    #[test]
    fn persist_model_creates_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        persist_model_to_path("gpt-4o-mini", &path).unwrap();
        assert_eq!(
            read_model_from_config(&path).as_deref(),
            Some("gpt-4o-mini")
        );
    }

    #[test]
    fn persist_model_updates_existing_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "model = \"old-model\"\ntheme = \"dusk\"\n").unwrap();
        persist_model_to_path("new-model", &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("model = \"new-model\""), "{content}");
        assert!(
            content.contains("theme = \"dusk\""),
            "theme line preserved: {content}"
        );
    }

    #[test]
    fn persist_model_appends_when_no_model_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"dusk\"\n").unwrap();
        persist_model_to_path("gpt-4o-mini", &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("model = \"gpt-4o-mini\""), "{content}");
    }

    #[test]
    fn persist_theme_choice_preserves_unrelated_user_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# keep this comment\nmodel = \"gpt-4o-mini\"\ntheme = \"auto\"\n[compaction]\nkeep_recent_tokens = 8\n",
        )
        .unwrap();

        for choice in ["light", "dark", "auto", "MyTheme"] {
            persist_theme_to_path(theme_choice_key(choice).unwrap(), &path).unwrap();

            let content = std::fs::read_to_string(&path).unwrap();
            let parsed: toml::Value = toml::from_str(&content).unwrap();
            assert_eq!(parsed["theme"].as_str(), Some(choice));
            assert_eq!(parsed["model"].as_str(), Some("gpt-4o-mini"));
            assert_eq!(
                parsed["compaction"]["keep_recent_tokens"].as_integer(),
                Some(8)
            );
            assert!(content.contains("# keep this comment"), "{content}");
        }
    }

    #[test]
    fn theme_choice_persistence_rejects_unsafe_and_reserved_file_names() {
        assert_eq!(theme_choice_key(" DARK ").unwrap(), "dark");
        assert_eq!(theme_choice_key("MyTheme").unwrap(), "MyTheme");
        for name in ["../other", "a/b", "default", "foo.toml", "", "bad name"] {
            assert!(theme_choice_key(name).is_err(), "accepted {name:?}");
        }
    }

    #[test]
    fn theme_onboarding_only_applies_to_fresh_interactive_installs() {
        let directory = cwd();
        let mut cli = base();
        cli.workspace = Some(directory.path().into());
        cli.workspace_trusted = true;
        let config = config_with_empty_global(cli, directory.path()).unwrap();
        let missing_global = directory.path().join("missing-user-config.toml");
        assert!(should_offer_theme_onboarding_at(
            &config,
            Some(&missing_global)
        ));

        std::fs::write(&missing_global, "theme = \"auto\"\n").unwrap();
        assert!(!should_offer_theme_onboarding_at(
            &config,
            Some(&missing_global)
        ));

        let mut configured = config.clone();
        configured.theme = Some("legacy-theme".into());
        assert!(!should_offer_theme_onboarding_at(
            &configured,
            Some(&directory.path().join("still-missing.toml"))
        ));

        let mut plain = config;
        plain.plain = true;
        assert!(!should_offer_theme_onboarding_at(
            &plain,
            Some(&directory.path().join("another-missing.toml"))
        ));

        let mut print = plain.clone();
        print.plain = false;
        print.mode = Mode::Print {
            prompt: "hello".into(),
        };
        assert!(!should_offer_theme_onboarding_at(
            &print,
            Some(&directory.path().join("print-missing.toml"))
        ));

        let mut rpc = print;
        rpc.mode = Mode::Rpc;
        assert!(!should_offer_theme_onboarding_at(
            &rpc,
            Some(&directory.path().join("rpc-missing.toml"))
        ));
    }

    #[test]
    fn recognized_terminal_appearance_environment_counts_as_configured() {
        for value in ["auto", "dark", "light", "unknown", "universal"] {
            assert!(terminal_appearance_environment_is_configured_value(Some(
                value
            )));
        }
        assert!(!terminal_appearance_environment_is_configured_value(Some(
            "neon"
        )));
        assert!(!terminal_appearance_environment_is_configured_value(None));
    }

    #[test]
    fn persist_model_skips_commented_model_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# model = \"commented-out\"\ntheme = \"dusk\"\n").unwrap();
        persist_model_to_path("active-model", &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        // The TOML-based parser does not preserve comments since they are
        // not part of the parsed representation. The commented line is
        // intentionally dropped in exchange for structurally correct updates
        // that never corrupt multi-line values or cause partial-key collisions.
        assert!(
            content.contains("model = \"active-model\""),
            "new entry set: {content}"
        );
        assert!(
            content.contains("theme = \"dusk\""),
            "existing key preserved: {content}"
        );
    }

    #[test]
    fn persist_model_preserves_multiline_values_and_partial_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "model_alias = \"keep\"\nnotes = [\n  \"first\",\n  \"second\",\n]\n[compaction]\nkeep_recent_tokens = 4\n",
        )
        .unwrap();

        persist_model_to_path("active-model", &path).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: toml::Value = toml::from_str(&content).unwrap();
        assert_eq!(parsed["model"].as_str(), Some("active-model"));
        assert_eq!(parsed["model_alias"].as_str(), Some("keep"));
        assert_eq!(parsed["notes"].as_array().unwrap().len(), 2);
        assert_eq!(
            parsed["compaction"]["keep_recent_tokens"].as_integer(),
            Some(4)
        );
    }

    #[test]
    fn persist_model_rejects_invalid_toml_without_rewriting_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let invalid = "model = [\n";
        std::fs::write(&path, invalid).unwrap();

        assert!(persist_model_to_path("active-model", &path).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), invalid);
    }

    #[test]
    fn persist_model_escapes_special_characters() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        // Backslash and double-quote must be escaped in TOML basic strings.
        persist_model_to_path("model\\with\"quotes", &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("model = "), "{content}");
        // Round-trip: the written TOML must parse back to the original id.
        let parsed: std::collections::BTreeMap<String, toml::Value> =
            toml::from_str(&content).unwrap();
        assert_eq!(
            parsed.get("model").unwrap().as_str().unwrap(),
            "model\\with\"quotes"
        );
    }

    #[test]
    fn doctor_command_parses_without_a_prompt() {
        let cli = Cli::try_parse_from(["octet", "--offline", "doctor"]).unwrap();
        assert!(cli.message.is_none());
        assert!(matches!(cli.command, Some(TopLevelCommand::Doctor)));
        assert!(cli.offline);
    }

    #[test]
    fn setup_command_parses_explicit_non_interactive_inputs() {
        let cli = Cli::try_parse_from([
            "octet",
            "setup",
            "--endpoint",
            "https://models.example.test/v1/",
            "--api-key-env",
            "EXAMPLE_API_KEY",
            "--manual-model",
            "example-model",
            "--yes",
        ])
        .unwrap();
        assert!(cli.message.is_none());
        assert!(matches!(
            cli.command,
            Some(TopLevelCommand::Setup { options })
                if options.preset.is_none()
                    && options.endpoint.as_deref() == Some("https://models.example.test/v1/")
                    && options.api_key_env.as_deref() == Some("EXAMPLE_API_KEY")
                    && options.manual_model.as_deref() == Some("example-model")
                    && options.yes
        ));

        let cli = Cli::try_parse_from([
            "octet",
            "setup",
            "--preset",
            "lm-studio",
            "--offline",
            "--manual-model",
            "local-model",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(TopLevelCommand::Setup { options })
                if options.preset == Some(SetupPreset::LmStudio)
                    && options.offline
                    && options.manual_model.as_deref() == Some("local-model")
        ));
    }

    #[test]
    fn sessions_subcommands_do_not_consume_the_positional_prompt() {
        let cli = Cli::try_parse_from(["octet", "sessions", "inspect", "abc-123"]).unwrap();
        assert!(cli.message.is_none());
        assert!(matches!(
            cli.command,
            Some(TopLevelCommand::Sessions {
                command: SessionCommand::Inspect { ref id }
            }) if id == "abc-123"
        ));
    }

    #[test]
    fn extension_package_commands_parse_without_a_prompt() {
        let cli = Cli::try_parse_from(["octet", "extension", "install", "octet-serve"]).unwrap();
        assert!(cli.message.is_none());
        assert!(matches!(
            cli.command,
            Some(TopLevelCommand::Extension {
                command: ExtensionCommand::Install {
                    name: Some(ref name),
                    path: None,
                }
            }) if name == "octet-serve"
        ));

        let cli =
            Cli::try_parse_from(["octet", "extension", "install", "--path", "./serve.tar.gz"])
                .unwrap();
        assert!(matches!(
            cli.command,
            Some(TopLevelCommand::Extension {
                command: ExtensionCommand::Install {
                    name: None,
                    path: Some(_),
                }
            })
        ));

        let cli =
            Cli::try_parse_from(["octet", "extension", "update", "octet-web-search"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(TopLevelCommand::Extension {
                command: ExtensionCommand::Update {
                    name: Some(ref name),
                    path: None,
                }
            }) if name == "octet-web-search"
        ));
        let cli =
            Cli::try_parse_from(["octet", "extension", "update", "--path", "./bundle.tar.gz"])
                .unwrap();
        assert!(matches!(
            cli.command,
            Some(TopLevelCommand::Extension {
                command: ExtensionCommand::Update {
                    name: None,
                    path: Some(_),
                }
            })
        ));
    }

    #[test]
    fn pi_migration_dry_run_parses_without_a_prompt() {
        let cli = Cli::try_parse_from([
            "octet",
            "migrate",
            "pi",
            "--dry-run",
            "--json",
            "--project",
            "./workspace",
        ])
        .unwrap();
        assert!(cli.message.is_none());
        assert!(matches!(
            cli.command,
            Some(TopLevelCommand::Migrate {
                command: MigrationCommand::Pi {
                    dry_run: true,
                    json: true,
                    project: Some(_),
                    ..
                }
            })
        ));
    }

    #[test]
    fn pi_compatibility_command_is_not_exposed_and_bridge_options_are_rejected() {
        let mut command = Cli::command();
        assert!(command.find_subcommand("pi").is_none());
        assert!(command.find_subcommand("migrate").is_some());
        let help = command.render_long_help().to_string();
        assert!(!help
            .lines()
            .any(|line| line.split_whitespace().next() == Some("pi")));
        for arguments in [
            vec![
                "octet",
                "pi",
                "install",
                "./extension.ts",
                "--pi-package",
                "./pi",
            ],
            vec!["octet", "pi", "list", "--extension-root", "./extensions"],
            vec!["octet", "pi", "publish", "--plan", "./plan.json"],
        ] {
            let error = Cli::try_parse_from(arguments).unwrap_err();
            assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
        }
    }

    #[test]
    fn pi_is_an_ordinary_prompt_not_a_compatibility_command() {
        // Removing a subcommand does not reserve or reject ordinary prompt text.
        for arguments in [vec!["octet", "pi"], vec!["octet", "pi", "list"]] {
            let cli = Cli::try_parse_from(arguments).unwrap();
            assert!(cli.command.is_none());
            assert_eq!(cli.message.as_deref(), Some("pi"));
        }
    }

    #[test]
    fn pi_import_remains_available_without_the_compatibility_bridge() {
        let cli = Cli::try_parse_from(["octet", "migrate", "import", "pi", "--dry-run"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(TopLevelCommand::Migrate {
                command: MigrationCommand::Import {
                    command: crate::migrate::MigrationImportCommand::Pi { dry_run: true, .. },
                },
            })
        ));
        let cli = Cli::try_parse_from(["octet", "explain pi migration"]).unwrap();
        assert_eq!(cli.message.as_deref(), Some("explain pi migration"));
    }

    #[test]
    fn serve_command_parses_forwarded_loopback_options() {
        let cli = Cli::try_parse_from([
            "octet",
            "serve",
            "--no-open",
            "--port",
            "0",
            "--web-root",
            "./web",
        ])
        .unwrap();
        assert!(cli.message.is_none());
        assert!(matches!(
            cli.command,
            Some(TopLevelCommand::Serve {
                no_open: true,
                port: 0,
                web_root: Some(_),
                name: None,
            })
        ));
    }

    #[test]
    fn serve_command_accepts_a_startup_session_name() {
        let cli = Cli::try_parse_from(["octet", "serve", "--name", "  release review  "]).unwrap();
        match cli.command {
            Some(TopLevelCommand::Serve {
                no_open: false,
                port: 31415,
                web_root: None,
                name: Some(name),
            }) => assert_eq!(name, "  release review  "),
            other => panic!("unexpected parse result {other:?}"),
        }
    }

    #[test]
    fn serve_command_rejects_a_name_without_a_value() {
        assert!(Cli::try_parse_from(["octet", "serve", "--name"]).is_err());
    }

    #[test]
    fn debug_prompt_is_an_explicit_prompt_template_diagnostic() {
        let cli = Cli::try_parse_from(["octet", "--print", "--prompt", "review", "--debug-prompt"])
            .unwrap();
        assert_eq!(cli.prompt_template.as_deref(), Some("review"));
        assert!(cli.debug_prompt);
    }
}
