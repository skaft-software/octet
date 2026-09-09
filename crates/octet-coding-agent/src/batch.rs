#![allow(missing_docs)]

//! CLI support for OpenRouter's asynchronous Batch API.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use clap::Subcommand;
use octet_ai::{AiClient, Model, ModelCatalog, ModelId, OpenRouterBatchListOptions, Protocol};
use serde::{Deserialize, Serialize};

use crate::app::bootstrap;
use crate::config::Config;

const MAX_BATCH_INPUT_BYTES: usize = 256 * 1024 * 1024;
const MAX_POLL_SECONDS: u64 = 3_600;

/// OpenRouter Batch API commands.
#[derive(Clone, Debug, Subcommand)]
pub enum BatchCommand {
    /// Submit a JSON batch and print the provider's batch id.
    Submit {
        /// octet model id or an OpenRouter API model slug.
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// JSON file containing a `requests` array, or `-` for stdin.
        #[arg(long, value_name = "PATH")]
        input: PathBuf,
        /// OpenRouter endpoint path. Defaults from the selected model protocol.
        #[arg(long, value_name = "PATH")]
        endpoint: Option<String>,
    },
    /// Retrieve a batch; use `--wait` to poll until a terminal status.
    #[command(alias = "status", alias = "retrieve")]
    Get {
        /// OpenRouter batch id, such as `batch_...`.
        id: String,
        /// octet model id or an OpenRouter API model slug used for credentials.
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// Poll until the batch is completed, failed, expired, or cancelled.
        #[arg(long)]
        wait: bool,
        /// Seconds between polls when `--wait` is used.
        #[arg(long, default_value_t = 30, value_name = "SECONDS")]
        poll_seconds: u64,
    },
    /// List OpenRouter batches.
    List {
        /// octet model id or an OpenRouter API model slug used for credentials.
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,
        /// Maximum number of batches to return (1 through 100).
        #[arg(long)]
        limit: Option<u32>,
        /// Cursor from a previous page's `last_id`.
        #[arg(long)]
        after: Option<String>,
        /// Status filter; repeat for multiple statuses.
        #[arg(long = "status", value_name = "STATUS")]
        statuses: Vec<String>,
        /// Return batches created after this Unix timestamp or ISO-8601 value.
        #[arg(long)]
        created_after: Option<String>,
        /// Return batches created before this Unix timestamp or ISO-8601 value.
        #[arg(long)]
        created_before: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct BatchInputEnvelope {
    #[serde(default)]
    endpoint: Option<String>,
    #[serde(default)]
    model: Option<String>,
    requests: Vec<octet_ai::OpenRouterBatchRequestItem>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BatchInput {
    Requests(Vec<octet_ai::OpenRouterBatchRequestItem>),
    Envelope(BatchInputEnvelope),
}

#[derive(Debug)]
struct ParsedBatchInput {
    endpoint: Option<String>,
    model: Option<String>,
    requests: Vec<octet_ai::OpenRouterBatchRequestItem>,
}

impl From<BatchInput> for ParsedBatchInput {
    fn from(input: BatchInput) -> Self {
        match input {
            BatchInput::Requests(requests) => Self {
                endpoint: None,
                model: None,
                requests,
            },
            BatchInput::Envelope(envelope) => Self {
                endpoint: envelope.endpoint,
                model: envelope.model,
                requests: envelope.requests,
            },
        }
    }
}

fn read_batch_input(path: &Path, config: &Config) -> anyhow::Result<ParsedBatchInput> {
    let bytes = if path == Path::new("-") {
        let stdin = std::io::stdin();
        let stdin = stdin.lock();
        let mut bytes = Vec::new();
        stdin
            .take((MAX_BATCH_INPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .context("read OpenRouter batch JSON from stdin")?;
        bytes
    } else {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            config.invocation_cwd.join(path)
        };
        octet_agent::secure_fs::read_regular_file_bounded(&path, MAX_BATCH_INPUT_BYTES).map_err(
            |error| anyhow::anyhow!("read OpenRouter batch input {}: {error}", path.display()),
        )?
    };

    if bytes.len() > MAX_BATCH_INPUT_BYTES {
        anyhow::bail!(
            "OpenRouter batch input exceeds the {}-byte limit",
            MAX_BATCH_INPUT_BYTES
        );
    }
    let input = serde_json::from_slice::<BatchInput>(&bytes)
        .context("parse OpenRouter batch input; expected an object with a requests array or a requests array")?;
    Ok(input.into())
}

fn model_id_candidates(raw: &str) -> Vec<ModelId> {
    let raw = raw.trim();
    let mut candidates = vec![ModelId(raw.to_owned())];
    if !raw.starts_with("openrouter/") {
        candidates.push(ModelId(format!("openrouter/{raw}")));
    }
    candidates
}

fn resolve_batch_model(
    catalog: &mut ModelCatalog,
    config: &Config,
    model_arg: Option<&str>,
    input_model: Option<&str>,
) -> anyhow::Result<Model> {
    let requested = model_arg
        .or_else(|| config.model.as_ref().map(|model| model.0.as_str()))
        .or(input_model)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "an OpenRouter model is required; pass --model openrouter/<provider>/<model>"
            )
        })?;
    if requested.trim().is_empty() {
        anyhow::bail!("OpenRouter model must not be empty");
    }

    let candidates = model_id_candidates(requested);
    let mut model = candidates.iter().find_map(|id| catalog.resolve(id).ok());
    if model.is_none() && config.offline {
        bootstrap::register_offline_openrouter_model(catalog, requested)?;
        model = candidates.iter().find_map(|id| catalog.resolve(id).ok());
    }
    let model = model.ok_or_else(|| {
        anyhow::anyhow!("unknown model {requested:?}; run `octet doctor` to inspect the catalog")
    })?;
    if model.endpoint.id.0 != "openrouter" {
        anyhow::bail!(
            "OpenRouter Batch API requires an openrouter model, but {} uses {}",
            model.spec.id.0,
            model.endpoint.id.0
        );
    }
    Ok(model)
}

fn input_model_matches(model: &Model, input_model: &str) -> bool {
    input_model == model.spec.api_name || input_model == model.spec.id.0
}

fn validate_input_model_match(model: &Model, input_model: &str) -> anyhow::Result<()> {
    if !input_model_matches(model, input_model) {
        anyhow::bail!(
            "batch input model {input_model:?} does not match selected model {}",
            model.spec.api_name
        );
    }
    Ok(())
}

fn validate_input_endpoint_match(
    input_endpoint: Option<&str>,
    endpoint_arg: Option<&str>,
) -> anyhow::Result<()> {
    if let (Some(input_endpoint), Some(endpoint_arg)) = (input_endpoint, endpoint_arg) {
        if input_endpoint != endpoint_arg {
            anyhow::bail!(
                "batch input endpoint {input_endpoint:?} does not match --endpoint {endpoint_arg:?}"
            );
        }
    }
    Ok(())
}

fn default_batch_endpoint(model: &Model) -> anyhow::Result<&'static str> {
    match model.spec.protocol {
        Protocol::OpenAiChat => Ok("/v1/chat/completions"),
        Protocol::OpenAiResponses => Ok("/v1/responses"),
        Protocol::AnthropicMessages => Ok("/v1/messages"),
        Protocol::BedrockConverse | Protocol::GoogleGenerativeAi => {
            anyhow::bail!("OpenRouter Batch API does not support the selected model protocol")
        }
    }
}

fn print_json<T: Serialize>(value: &T) -> anyhow::Result<()> {
    crate::output::stdout_multiline(serde_json::to_string_pretty(value)?);
    Ok(())
}

async fn submit(
    config: &Config,
    model_arg: Option<&str>,
    input_path: &Path,
    endpoint_arg: Option<&str>,
) -> anyhow::Result<()> {
    let input = read_batch_input(input_path, config)?;
    let mut catalog = bootstrap::model_catalog_with_offline(config.offline)?;
    let model = resolve_batch_model(&mut catalog, config, model_arg, input.model.as_deref())?;

    if let Some(input_model) = input.model.as_deref() {
        validate_input_model_match(&model, input_model)?;
    }

    validate_input_endpoint_match(input.endpoint.as_deref(), endpoint_arg)?;
    let endpoint = if let Some(endpoint) = endpoint_arg.or(input.endpoint.as_deref()) {
        endpoint
    } else {
        default_batch_endpoint(&model)?
    };

    let request = octet_ai::OpenRouterBatchRequest::new(
        endpoint,
        model.spec.api_name.clone(),
        input.requests,
    );
    let client = AiClient::try_new()?;
    let batch = client
        .submit_openrouter_batch(&model.endpoint, request)
        .await?;
    print_json(&batch)
}

async fn get(
    config: &Config,
    model_arg: Option<&str>,
    id: &str,
    wait: bool,
    poll_seconds: u64,
) -> anyhow::Result<()> {
    if wait && !(1..=MAX_POLL_SECONDS).contains(&poll_seconds) {
        anyhow::bail!("--poll-seconds must be between 1 and {MAX_POLL_SECONDS}");
    }
    let mut catalog = bootstrap::model_catalog_with_offline(config.offline)?;
    let model = resolve_batch_model(&mut catalog, config, model_arg, None)?;
    let client = AiClient::try_new()?;

    loop {
        let batch = client.get_openrouter_batch(&model.endpoint, id).await?;
        if !wait || batch.is_terminal() {
            return print_json(&batch);
        }
        crate::output::stderr_line(format!(
            "OpenRouter batch {} is {}; waiting {} seconds",
            batch.id, batch.status, poll_seconds
        ));
        tokio::time::sleep(Duration::from_secs(poll_seconds)).await;
    }
}

async fn list(
    config: &Config,
    model_arg: Option<&str>,
    options: OpenRouterBatchListOptions,
) -> anyhow::Result<()> {
    let mut catalog = bootstrap::model_catalog_with_offline(config.offline)?;
    let model = resolve_batch_model(&mut catalog, config, model_arg, None)?;
    let client = AiClient::try_new()?;
    let batches = client
        .list_openrouter_batches(&model.endpoint, &options)
        .await?;
    print_json(&batches)
}

/// Run one Batch API command without entering the interactive agent loop.
pub async fn run(command: BatchCommand, config: &Config) -> anyhow::Result<()> {
    match command {
        BatchCommand::Submit {
            model,
            input,
            endpoint,
        } => submit(config, model.as_deref(), &input, endpoint.as_deref()).await,
        BatchCommand::Get {
            id,
            model,
            wait,
            poll_seconds,
        } => get(config, model.as_deref(), &id, wait, poll_seconds).await,
        BatchCommand::List {
            model,
            limit,
            after,
            statuses,
            created_after,
            created_before,
        } => {
            list(
                config,
                model.as_deref(),
                OpenRouterBatchListOptions {
                    limit,
                    after,
                    statuses,
                    created_after,
                    created_before,
                },
            )
            .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    use crate::cli::Cli;

    #[test]
    fn parses_batch_commands_without_entering_agent_mode() {
        let cli = Cli::try_parse_from([
            "octet",
            "batch",
            "submit",
            "--model",
            "openrouter/openai/gpt-4o",
            "--input",
            "requests.json",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Some(crate::cli::TopLevelCommand::Batch { .. })
        ));
    }

    #[test]
    fn validates_input_model_and_endpoint_mismatches_before_submission() {
        let catalog = ModelCatalog::builtin().unwrap();
        let model = catalog.resolve(&ModelId("gpt-4o-mini".into())).unwrap();
        let api_name = model.spec.api_name.clone();
        let model_id = model.spec.id.0.clone();

        assert!(validate_input_model_match(&model, &api_name).is_ok());
        assert!(validate_input_model_match(&model, &model_id).is_ok());
        let model_error = validate_input_model_match(&model, "different/model").unwrap_err();
        assert!(model_error
            .to_string()
            .contains("does not match selected model"));

        assert!(validate_input_endpoint_match(None, Some("/v1/responses")).is_ok());
        assert!(validate_input_endpoint_match(
            Some("/v1/chat/completions"),
            Some("/v1/chat/completions"),
        )
        .is_ok());
        let endpoint_error =
            validate_input_endpoint_match(Some("/v1/responses"), Some("/v1/chat/completions"))
                .unwrap_err();
        assert!(endpoint_error
            .to_string()
            .contains("does not match --endpoint"));
    }

    #[test]
    fn maps_a_bare_requests_array_without_losing_items() {
        let input: BatchInput = serde_json::from_value(serde_json::json!([
            {"custom_id": "one", "body": {"messages": []}}
        ]))
        .unwrap();
        let input: ParsedBatchInput = input.into();
        assert_eq!(input.requests.len(), 1);
        assert!(input.endpoint.is_none());
        assert!(input.model.is_none());
    }
}
