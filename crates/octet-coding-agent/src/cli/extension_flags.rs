//! Runtime CLI flags declared by trusted extension manifests.
//!
//! Extension manifests can declare flags, but a manifest is only discovered
//! after configuration is layered, which is after clap would have rejected an
//! unknown option. This module owns that bootstrap order: a bounded pre-pass
//! extracts just the options that select trusted manifests
//! ([`extension_flag_bootstrap`]), builds enough layered configuration to
//! resolve them without duplicating user-visible diagnostics
//! ([`build_config_for_extension_flags`]), registers the declared spellings on a
//! copy of the static command ([`extension_flag_command`]), and only then hands
//! the argument vector to clap.
//!
//! The two `pub(crate)` entry points are the whole surface. Both scans are
//! deliberately hand-rolled rather than clap recovery: recovery stops at the
//! first unknown dynamic flag, which would misclassify a later activation
//! option or a positional prompt as a subcommand and silently take the wrong
//! parse path.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, Command, CommandFactory, FromArgMatches, Parser};
use octet_agent::extension_process::{ExtensionFlag, ExtensionFlagType};

use super::{
    build_config_with_global_path_and_diagnostics, global_config_path, Cli, ExtensionFlagValues,
};
use crate::config::Config;

#[derive(Clone, Debug)]
pub(super) struct RegisteredExtensionFlag {
    extension: String,
    declaration: ExtensionFlag,
    argument_id: String,
    negative_argument_id: Option<String>,
}

#[derive(Default)]
pub(super) struct ExtensionFlagBootstrap {
    pub(super) workspace: Option<PathBuf>,
    pub(super) extension_dirs: Vec<PathBuf>,
    pub(super) enable_extensions: Vec<String>,
    pub(super) trust_extensions: Vec<String>,
    pub(super) workspace_trusted: bool,
    pub(super) safe_mode: bool,
    pub(super) effect_policy: Option<String>,
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
pub(super) fn extension_flag_bootstrap(args: &[OsString]) -> Option<ExtensionFlagBootstrap> {
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

#[cfg(test)]
thread_local! {
    static EXTENSION_COMMAND_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn static_extension_command() -> Command {
    #[cfg(test)]
    EXTENSION_COMMAND_BUILDS.with(|count| count.set(count.get() + 1));
    Cli::command()
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

pub(super) fn register_extension_flags(
    declarations: Vec<(String, ExtensionFlag)>,
) -> anyhow::Result<Vec<RegisteredExtensionFlag>> {
    // clap adds these built-ins while building the command, after
    // `get_arguments()` exposes derive-declared arguments.
    let mut occupied = BTreeSet::from(["help".to_owned(), "version".to_owned()]);
    collect_static_long_options(&static_extension_command(), &mut occupied);
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

pub(super) fn extension_flag_command(registered: &[RegisteredExtensionFlag]) -> Command {
    let mut command = static_extension_command();
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

pub(super) fn resolve_extension_flag_values(
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

pub(super) fn invocation_has_top_level_subcommand(
    args: &[OsString],
    registered: &[RegisteredExtensionFlag],
) -> bool {
    let command = static_extension_command();
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
        if matches!(value, "-p" | "-c" | "-r") {
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
    let declarations = bootstrap_extension_config(&args, cwd)
        .map(|config| crate::extensions::selected_extension_flag_declarations(&config))
        .unwrap_or_default();
    parse_declared_extension_flags(args, declarations)
}

fn parse_declared_extension_flags(
    args: Vec<OsString>,
    declarations: Vec<(String, ExtensionFlag)>,
) -> anyhow::Result<(Cli, ExtensionFlagValues)> {
    // Manifest discovery and its trust checks still run before this boundary.
    // Without declarations, there is nothing to register or scan around: use
    // the existing static parser without rebuilding its command for collision
    // enumeration and subcommand detection first.
    if declarations.is_empty() {
        return Ok((parse_static_or_exit(args), BTreeMap::new()));
    }
    let static_args = args.clone();
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
    let command = static_extension_command();
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
        if matches!(value, "-p" | "-c" | "-r") {
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

#[cfg(test)]
mod fast_path_tests {
    use super::*;

    #[test]
    fn no_declarations_preserve_static_parse_without_extension_command_builds() {
        for invocation in [
            vec!["octet"],
            vec!["octet", "--offline", "--model", "custom/bench/model-00000"],
            vec!["octet", "--workspace", "/tmp", "--", "--literal-prompt"],
            vec!["octet", "-p", "hello"],
            vec!["octet", "-r", "initial prompt"],
            vec!["octet", "-c", "initial prompt"],
            vec!["octet", "--thinking", "max", "initial prompt"],
            vec!["octet", "doctor"],
        ] {
            let args: Vec<OsString> = invocation
                .iter()
                .map(|argument| OsString::from(*argument))
                .collect();
            let expected = Cli::try_parse_from(args.clone()).unwrap();
            EXTENSION_COMMAND_BUILDS.with(|count| count.set(0));
            let (actual, values) = parse_declared_extension_flags(args, Vec::new()).unwrap();
            assert!(values.is_empty());
            assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
            EXTENSION_COMMAND_BUILDS.with(|count| assert_eq!(count.get(), 0));
        }
    }

    #[test]
    fn declared_flags_preserve_session_short_forms_and_thinking() {
        for selector in ["-r", "-c"] {
            let declarations = vec![(
                "fixture".to_owned(),
                ExtensionFlag {
                    name: "fixture-label".to_owned(),
                    kind: ExtensionFlagType::String,
                    default: serde_json::json!("default"),
                    description: None,
                },
            )];
            for level in ["off", "max"] {
                let args = [
                    "octet",
                    "--fixture-label",
                    "doctor",
                    selector,
                    "--thinking",
                    level,
                    "initial prompt",
                ]
                .into_iter()
                .map(OsString::from)
                .collect();
                let (cli, values) =
                    parse_declared_extension_flags(args, declarations.clone()).unwrap();
                assert!(cli.command.is_none());
                assert_eq!(cli.resume_picker, selector == "-r");
                assert_eq!(cli.continue_, selector == "-c");
                assert!(cli.resume.is_none());
                assert_eq!(cli.reasoning.as_deref(), Some(level));
                assert_eq!(cli.message.as_deref(), Some("initial prompt"));
                assert_eq!(
                    values["fixture"]["fixture-label"],
                    serde_json::json!("doctor")
                );
            }
        }
    }

    #[test]
    fn declared_flags_still_register_and_resolve_after_discovery() {
        let declarations = vec![(
            "fixture".to_owned(),
            ExtensionFlag {
                name: "fixture-count".to_owned(),
                kind: ExtensionFlagType::Integer,
                default: serde_json::json!(2),
                description: None,
            },
        )];
        let args = ["octet", "--fixture-count", "-7", "hello"]
            .into_iter()
            .map(OsString::from)
            .collect();
        EXTENSION_COMMAND_BUILDS.with(|count| count.set(0));
        let (cli, values) = parse_declared_extension_flags(args, declarations).unwrap();
        assert_eq!(cli.message.as_deref(), Some("hello"));
        assert_eq!(values["fixture"]["fixture-count"], serde_json::json!(-7));
        EXTENSION_COMMAND_BUILDS.with(|count| assert!(count.get() > 0));
    }
}
