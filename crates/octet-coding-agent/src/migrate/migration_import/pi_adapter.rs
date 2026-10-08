//! The read-only Pi adapter: its API 0.3 stdio wire plus the Pi source
//! reader that backs it.
//!
//! Why this is a separate module: the adapter is the one part of a migration
//! that runs as a child process, speaks newline-framed JSON-RPC, and reads
//! untrusted source data. Isolating it makes that trust boundary auditable in
//! one place - every byte the adapter can observe passes through this file,
//! and the host-owned ingestion side in the parent never has to reason about
//! framing, canonicality, spawn arguments, or the source read limits.
//!
//! Two halves live here because they are two ends of one contract and are only
//! meaningful together:
//!
//! - the **host** end, [`AdapterClient`], which re-execs the current binary as
//!   `octet migrate adapter pi` and exchanges bounded, canonical frames with
//!   it. It deliberately accepts no user-selected command or adapter path, so
//!   source data can never turn a migration into arbitrary process execution.
//! - the **adapter** end, [`run_pi_adapter_stdio`], which serves
//!   `migration/detect` and `migration/import` from a bounded read-only view of
//!   the source root, plus the readers ([`pi_detect`], [`pi_import`]) that turn
//!   that root into non-secret [`api`] values.
//!
//! Nothing here writes. Every destination read, conflict decision, backup, and
//! write is host-owned and stays in the parent module, so a compromised or
//! buggy adapter can at worst return wrong data, never corrupt octet's own
//! state. The frame limit, canonical-JSON check, and per-file byte caps below
//! are the whole of the adapter's authority.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use octet_agent::extension_api_v03 as api;
use serde_json::{json, Map, Value};

use super::{read_optional_regular, MAX_CONFIG_BYTES, MAX_SKILL_BYTES, MAX_SOURCE_ITEMS};

const ADAPTER_TIMEOUT: Duration = Duration::from_secs(15);
const CONFIG_CANDIDATES: &[&str] = &[
    "settings.json",
    "config.json",
    ".pi/settings.json",
    ".pi/agent/settings.json",
];
const SKILL_ROOT_CANDIDATES: &[&str] = &["skills", ".pi/skills", ".pi/agent/skills"];

/// A synchronous, bounded API 0.3 client for the current binary's read-only
/// adapter mode. It intentionally accepts no user-selected command or adapter
/// path, so source data cannot turn migration into arbitrary process execution.
pub(super) struct AdapterClient {
    child: Child,
    stdin: ChildStdin,
    responses: mpsc::Receiver<anyhow::Result<String>>,
    next_id: u64,
    contract: api::NegotiatedContract,
}

impl AdapterClient {
    pub(super) fn start() -> anyhow::Result<Self> {
        let executable = std::env::current_exe()
            .map_err(|error| anyhow::anyhow!("cannot locate the current octet binary: {error}"))?;
        let mut command = Command::new(executable);
        command
            .args(["migrate", "adapter", "pi"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env_clear();
        command.envs(octet_agent::extension_process::sanitized_subprocess_environment());
        let mut child = command.spawn().map_err(|error| {
            anyhow::anyhow!("could not start the built-in Pi migration adapter: {error}")
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("adapter stdin was not available"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("adapter stdout was not available"))?;
        let (sender, responses) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let result = read_bounded_adapter_line(&mut reader)
                    .map_err(|error| anyhow::anyhow!("cannot read adapter response: {error}"));
                match result {
                    Ok(Some(line)) => {
                        if sender.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });

        let offer = api::host_offer(api::MAX_FRAME_BYTES, 1)
            .map_err(|error| anyhow::anyhow!("cannot create migration adapter offer: {error}"))?;
        let mut client = Self {
            child,
            stdin,
            responses,
            next_id: 1,
            contract: api::NegotiatedContract {
                capabilities: BTreeSet::new(),
                methods: BTreeSet::new(),
                limits: api::ProtocolLimits {
                    max_frame_bytes: api::MAX_FRAME_BYTES,
                    max_concurrent_requests: 1,
                    max_tools: 0,
                },
            },
        };
        let request = api::InitializeRequest {
            api_version: api::API_VERSION.to_owned(),
            octet_version: env!("CARGO_PKG_VERSION").to_owned(),
            extension: json!({"name":"octet-import-pi","version":"1"}),
            workspace: "/migration-source".to_owned(),
            capabilities: json!({"filesystem":"source_read_only","network":false,"process":false}),
            contributes: json!({}),
            flag_values: Vec::new(),
            host: json!({"migration_ingestion":"host_owned"}),
            contract: offer.clone(),
        };
        api::validate_initialize_request(&request)
            .map_err(|error| anyhow::anyhow!("invalid adapter initialization request: {error}"))?;
        let response = client.call("initialize", serde_json::to_value(request)?)?;
        let response = api::parse_initialize_response(response).map_err(|error| {
            anyhow::anyhow!("adapter returned invalid initialization response: {error}")
        })?;
        let contract = api::negotiate(&offer, &response.contract)
            .map_err(|error| anyhow::anyhow!("adapter contract negotiation failed: {error}"))?;
        for method in ["migration/detect", "migration/import"] {
            api::require_method(&contract, method, api::MethodDirection::HostToExtension)
                .map_err(|error| anyhow::anyhow!("adapter does not provide {method}: {error}"))?;
        }
        client.contract = contract;
        Ok(client)
    }

    pub(super) fn detect(&mut self, source: &Path) -> anyhow::Result<api::MigrationDetectResult> {
        let params = api::MigrationDetectParams {
            source_root: path_to_utf8(source, "migration source")?,
        };
        let value = self.call("migration/detect", serde_json::to_value(params)?)?;
        api::parse_migration_detect_result(value)
            .map_err(|error| anyhow::anyhow!("adapter returned invalid detect result: {error}"))
    }

    pub(super) fn import(
        &mut self,
        source: &Path,
        config_paths: &[String],
    ) -> anyhow::Result<api::MigrationImportResult> {
        let params = api::MigrationImportParams {
            source_root: path_to_utf8(source, "migration source")?,
            config_paths: config_paths.to_vec(),
        };
        let value = self.call("migration/import", serde_json::to_value(params)?)?;
        api::parse_migration_import_result(value)
            .map_err(|error| anyhow::anyhow!("adapter returned invalid import result: {error}"))
    }

    pub(super) fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        // Initialization is the one request sent before a negotiated contract
        // exists. Every later request must be explicitly selected.
        if method != "initialize" {
            api::require_method(
                &self.contract,
                method,
                api::MethodDirection::HostToExtension,
            )
            .map_err(|error| anyhow::anyhow!("adapter method {method} is unavailable: {error}"))?;
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let request = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params});
        let frame = api::canonical_frame(&request, api::MAX_FRAME_BYTES)
            .map_err(|error| anyhow::anyhow!("cannot encode adapter request: {error}"))?;
        self.stdin.write_all(frame.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;

        let raw = match self.responses.recv_timeout(ADAPTER_TIMEOUT) {
            Ok(result) => result?,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                anyhow::bail!("Pi migration adapter timed out")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                anyhow::bail!("Pi migration adapter exited before responding")
            }
        };
        let envelope = parse_canonical_adapter_frame(&raw)?;
        match envelope {
            api::JsonRpcEnvelope::SuccessResponse(response) => {
                if response.id != api::JsonRpcId::Number(id) {
                    anyhow::bail!("Pi migration adapter returned a response with an unexpected id")
                }
                Ok(response.result)
            }
            api::JsonRpcEnvelope::ErrorResponse(response) => {
                if response.id != api::JsonRpcId::Number(id) {
                    anyhow::bail!("Pi migration adapter returned an error with an unexpected id")
                }
                anyhow::bail!(
                    "Pi migration adapter rejected {method} with protocol code {}",
                    response.error.code
                )
            }
            _ => anyhow::bail!("Pi migration adapter returned a non-response frame"),
        }
    }

    pub(super) fn shutdown(&mut self) {
        let _ = self.call("shutdown", json!({}));
        let _ = self.child.wait();
    }
}

impl Drop for AdapterClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn parse_canonical_adapter_frame(raw: &str) -> anyhow::Result<api::JsonRpcEnvelope> {
    if raw.is_empty() || raw.len() > api::MAX_FRAME_BYTES {
        anyhow::bail!("adapter response is empty or exceeds the API 0.3 frame limit")
    }
    let value: Value = serde_json::from_str(raw)
        .map_err(|_| anyhow::anyhow!("adapter response is not valid JSON"))?;
    let canonical = api::canonical_json(&value)
        .map_err(|error| anyhow::anyhow!("adapter response is not canonical: {error}"))?;
    if canonical != raw {
        anyhow::bail!("adapter response is not canonical API 0.3 JSON")
    }
    api::parse_json_rpc_envelope(value).map_err(|error| {
        anyhow::anyhow!("adapter response has an invalid JSON-RPC envelope: {error}")
    })
}

pub(super) fn read_bounded_adapter_line(
    reader: &mut impl BufRead,
) -> std::io::Result<Option<String>> {
    let mut bytes = Vec::with_capacity(api::MAX_FRAME_BYTES.min(4096));
    loop {
        let (consumed, newline) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if bytes.is_empty() {
                    return Ok(None);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "adapter frame is not newline terminated",
                ));
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |index| index.saturating_add(1));
            let payload = consumed.saturating_sub(usize::from(newline.is_some()));
            if bytes.len().saturating_add(payload) > api::MAX_FRAME_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "adapter frame exceeds the API 0.3 limit",
                ));
            }
            bytes.extend_from_slice(&available[..payload]);
            (consumed, newline.is_some())
        };
        reader.consume(consumed);
        if newline {
            return String::from_utf8(bytes).map(Some).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "adapter frame is not UTF-8",
                )
            });
        }
    }
}

pub(super) fn run_pi_adapter_stdio() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    let mut initialized: Option<api::NegotiatedContract> = None;

    let mut reader = stdin.lock();
    while let Some(line) = read_bounded_adapter_line(&mut reader)? {
        let request = match parse_canonical_adapter_frame(&line) {
            Ok(api::JsonRpcEnvelope::Request(request)) => request,
            Ok(_) | Err(_) => {
                write_adapter_error(&mut writer, api::JsonRpcId::Number(0), "invalid_request")?;
                continue;
            }
        };
        let id = request.id.clone();
        let mut should_shutdown = false;
        let response = match request.method.as_str() {
            "initialize" => (|| -> anyhow::Result<Value> {
                let request = api::parse_initialize_request(request.params)
                    .map_err(|error| anyhow::anyhow!("invalid initialize request: {error}"))?;
                api::validate_initialize_request(&request)
                    .map_err(|error| anyhow::anyhow!("invalid initialize request: {error}"))?;
                let mut selection = api::select_required(&request.contract).map_err(|error| {
                    anyhow::anyhow!("could not select adapter contract: {error}")
                })?;
                selection
                    .capabilities
                    .push("migration.adapter.v1".to_owned());
                selection
                    .methods
                    .extend(["migration/detect".to_owned(), "migration/import".to_owned()]);
                let contract = api::negotiate(&request.contract, &selection)
                    .map_err(|error| anyhow::anyhow!("invalid adapter contract: {error}"))?;
                initialized = Some(contract);
                serde_json::to_value(api::InitializeResponse {
                    api_version: api::API_VERSION.to_owned(),
                    tools: Vec::new(),
                    contract: selection,
                })
                .map_err(Into::into)
            })(),
            "migration/detect" => (|| -> anyhow::Result<Value> {
                let contract = initialized
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("adapter is not initialized"))?;
                api::require_method(
                    contract,
                    "migration/detect",
                    api::MethodDirection::HostToExtension,
                )
                .map_err(|error| anyhow::anyhow!("unnegotiated migration/detect: {error}"))?;
                let params = api::parse_migration_detect_params(request.params)
                    .map_err(|error| anyhow::anyhow!("invalid detect parameters: {error}"))?;
                serde_json::to_value(pi_detect(Path::new(&params.source_root))?).map_err(Into::into)
            })(),
            "migration/import" => (|| -> anyhow::Result<Value> {
                let contract = initialized
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("adapter is not initialized"))?;
                api::require_method(
                    contract,
                    "migration/import",
                    api::MethodDirection::HostToExtension,
                )
                .map_err(|error| anyhow::anyhow!("unnegotiated migration/import: {error}"))?;
                let params = api::parse_migration_import_params(request.params)
                    .map_err(|error| anyhow::anyhow!("invalid import parameters: {error}"))?;
                serde_json::to_value(pi_import(
                    Path::new(&params.source_root),
                    &params.config_paths,
                )?)
                .map_err(Into::into)
            })(),
            "shutdown" => (|| -> anyhow::Result<Value> {
                let params = api::parse_shutdown_params(request.params)
                    .map_err(|error| anyhow::anyhow!("invalid shutdown parameters: {error}"))?;
                api::validate_shutdown_params(&params)
                    .map_err(|error| anyhow::anyhow!("invalid shutdown parameters: {error}"))?;
                should_shutdown = true;
                serde_json::to_value(api::ShutdownResult {
                    terminal: "shutdown".to_owned(),
                })
                .map_err(Into::into)
            })(),
            _ => {
                write_adapter_error(&mut writer, id, "unknown_method")?;
                continue;
            }
        };
        match response {
            Ok(result) => write_adapter_success(&mut writer, id, result)?,
            Err(_) => write_adapter_error(&mut writer, id, "invalid_params")?,
        }
        if should_shutdown {
            break;
        }
    }
    Ok(())
}

fn write_adapter_success(
    writer: &mut impl Write,
    id: api::JsonRpcId,
    result: Value,
) -> anyhow::Result<()> {
    let value = serde_json::to_value(api::JsonRpcSuccessResponse {
        jsonrpc: "2.0".to_owned(),
        id,
        result,
    })?;
    let frame = api::canonical_frame(&value, api::MAX_FRAME_BYTES)
        .map_err(|error| anyhow::anyhow!("cannot write adapter response: {error}"))?;
    writer.write_all(frame.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn write_adapter_error(
    writer: &mut impl Write,
    id: api::JsonRpcId,
    name: &str,
) -> anyhow::Result<()> {
    let error = api::error_object(name, None)
        .map_err(|error| anyhow::anyhow!("cannot encode adapter error: {error}"))?;
    let value = serde_json::to_value(api::JsonRpcErrorResponse {
        jsonrpc: "2.0".to_owned(),
        id,
        error,
    })?;
    let frame = api::canonical_frame(&value, api::MAX_FRAME_BYTES)
        .map_err(|error| anyhow::anyhow!("cannot write adapter error: {error}"))?;
    writer.write_all(frame.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn adapter_source_root(source: &Path) -> anyhow::Result<PathBuf> {
    if !source.is_absolute() {
        anyhow::bail!("migration source root must be absolute")
    }
    let metadata = fs::symlink_metadata(source)
        .map_err(|_| anyhow::anyhow!("migration source root does not exist"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!("migration source root must be a regular directory")
    }
    let canonical = source
        .canonicalize()
        .map_err(|_| anyhow::anyhow!("migration source root does not exist"))?;
    let metadata = fs::symlink_metadata(&canonical)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        anyhow::bail!("migration source root must be a regular directory")
    }
    Ok(canonical)
}

pub(super) fn pi_detect(source: &Path) -> anyhow::Result<api::MigrationDetectResult> {
    let source = adapter_source_root(source)?;
    let mut config_paths = Vec::new();
    let mut diagnostics = Vec::new();
    for candidate in CONFIG_CANDIDATES {
        let path = source.join(candidate);
        match read_optional_regular(&path, MAX_CONFIG_BYTES) {
            Ok(Some(_)) => config_paths.push((*candidate).to_owned()),
            Ok(None) => {}
            Err(_) => diagnostics.push(adapter_diagnostic(
                candidate,
                "warning",
                "The Pi configuration file could not be read and will be skipped.",
            )),
        }
    }
    let has_skills = discover_skill_files(&source, &mut diagnostics)?
        .into_iter()
        .next()
        .is_some();
    Ok(api::MigrationDetectResult {
        detected: !config_paths.is_empty() || has_skills,
        config_paths,
        diagnostics,
    })
}

pub(super) fn pi_import(
    source: &Path,
    config_paths: &[String],
) -> anyhow::Result<api::MigrationImportResult> {
    let source = adapter_source_root(source)?;
    if config_paths.len() > MAX_SOURCE_ITEMS {
        anyhow::bail!("too many detected Pi configuration paths")
    }
    let mut seen = BTreeSet::new();
    let mut models = Vec::new();
    let mut mcp_servers = Vec::new();
    let mut diagnostics = Vec::new();

    for relative in config_paths {
        if !CONFIG_CANDIDATES.contains(&relative.as_str()) || !seen.insert(relative.clone()) {
            anyhow::bail!("adapter import received an unauthorized configuration path")
        }
        let path = source.join(relative);
        let Some(bytes) = read_optional_regular(&path, MAX_CONFIG_BYTES)? else {
            diagnostics.push(adapter_diagnostic(
                relative,
                "warning",
                "A configuration file reported during detection is no longer present.",
            ));
            continue;
        };
        let value: Value = match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                diagnostics.push(adapter_diagnostic(
                    relative,
                    "error",
                    "The Pi configuration is not valid JSON and was not imported.",
                ));
                continue;
            }
        };
        let Some(object) = value.as_object() else {
            diagnostics.push(adapter_diagnostic(
                relative,
                "error",
                "The Pi configuration root must be an object and was not imported.",
            ));
            continue;
        };
        extract_pi_models(object, relative, &mut models, &mut diagnostics);
        extract_pi_mcp_servers(object, relative, &mut mcp_servers, &mut diagnostics);
        if object.contains_key("permissions") || object.contains_key("permission") {
            diagnostics.push(adapter_diagnostic(
                relative,
                "warning",
                "Pi permission decisions were not imported; review octet policy settings separately.",
            ));
        }
    }

    let skill_files = discover_skill_files(&source, &mut diagnostics)?;
    let mut skills = Vec::new();
    for (name, path) in skill_files {
        let relative = source_relative(&source, &path)?;
        match octet_agent::secure_fs::read_regular_file_bounded(&path, MAX_SKILL_BYTES) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(content) => skills.push(api::MigrationSkill {
                    path: relative,
                    name,
                    content,
                }),
                Err(_) => diagnostics.push(adapter_diagnostic(
                    &source_relative(&source, &path)?,
                    "warning",
                    "A Pi skill is not UTF-8 and was not imported.",
                )),
            },
            Err(_) => diagnostics.push(adapter_diagnostic(
                &source_relative(&source, &path)?,
                "warning",
                "A Pi skill could not be read and was not imported.",
            )),
        }
    }

    if models.len() > MAX_SOURCE_ITEMS
        || skills.len() > MAX_SOURCE_ITEMS
        || mcp_servers.len() > MAX_SOURCE_ITEMS
        || diagnostics.len() > MAX_SOURCE_ITEMS
    {
        anyhow::bail!("Pi setup exceeds migration adapter item limits")
    }
    Ok(api::MigrationImportResult {
        models,
        skills,
        mcp_servers,
        diagnostics,
    })
}

fn extract_pi_models(
    object: &Map<String, Value>,
    path: &str,
    models: &mut Vec<api::MigrationModel>,
    diagnostics: &mut Vec<api::MigrationDiagnostic>,
) {
    let configured_provider = object.get("provider").and_then(Value::as_str);
    for key in ["model", "defaultModel", "selectedModel"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        match pi_model_parts(value, configured_provider) {
            Some((provider, model)) => models.push(api::MigrationModel {
                path: path.to_owned(),
                provider,
                model,
            }),
            None => diagnostics.push(adapter_diagnostic(
                path,
                "warning",
                "A Pi model selection could not be mapped without guessing a provider.",
            )),
        }
    }
    if let Some(values) = object.get("models").and_then(Value::as_array) {
        for value in values {
            match pi_model_parts(value, configured_provider) {
                Some((provider, model)) => models.push(api::MigrationModel {
                    path: path.to_owned(),
                    provider,
                    model,
                }),
                None => diagnostics.push(adapter_diagnostic(
                    path,
                    "warning",
                    "A Pi model selection could not be mapped without guessing a provider.",
                )),
            }
        }
    }
}

fn pi_model_parts(value: &Value, configured_provider: Option<&str>) -> Option<(String, String)> {
    match value {
        Value::String(value) => {
            let (provider, model) = value.split_once('/')?;
            valid_adapter_text(provider, api::MAX_MIGRATION_NAME_BYTES)
                .then(|| (provider.to_owned(), model.to_owned()))
        }
        Value::Object(object) => {
            let provider = object
                .get("provider")
                .and_then(Value::as_str)
                .or(configured_provider)?;
            let model = object
                .get("model")
                .or_else(|| object.get("id"))
                .and_then(Value::as_str)?;
            (valid_adapter_text(provider, api::MAX_MIGRATION_NAME_BYTES)
                && valid_adapter_text(model, api::MAX_MIGRATION_NAME_BYTES))
            .then(|| (provider.to_owned(), model.to_owned()))
        }
        _ => None,
    }
}

fn extract_pi_mcp_servers(
    object: &Map<String, Value>,
    path: &str,
    servers: &mut Vec<api::MigrationMcpServer>,
    diagnostics: &mut Vec<api::MigrationDiagnostic>,
) {
    let values = object
        .get("mcpServers")
        .or_else(|| object.get("mcp_servers"));
    let Some(Value::Object(values)) = values else {
        return;
    };
    for (name, value) in values {
        let Some(server) = value.as_object() else {
            diagnostics.push(adapter_diagnostic(
                path,
                "warning",
                "A Pi MCP entry is not an object and was not imported.",
            ));
            continue;
        };
        if server.contains_key("env") || server.contains_key("headers") {
            diagnostics.push(adapter_diagnostic(
                path,
                "warning",
                "MCP environment variables and headers were not imported.",
            ));
        }
        if server.contains_key("cwd") {
            diagnostics.push(adapter_diagnostic(
                path,
                "warning",
                "MCP working directories were not imported; configure them after review.",
            ));
        }
        if server
            .get("transport")
            .or_else(|| server.get("type"))
            .and_then(Value::as_str)
            .is_some_and(|transport| transport != "stdio")
            || server.contains_key("url")
        {
            diagnostics.push(adapter_diagnostic(
                path,
                "warning",
                "Only local stdio MCP servers can be imported.",
            ));
            continue;
        }
        let Some(command) = server.get("command").and_then(Value::as_str) else {
            diagnostics.push(adapter_diagnostic(
                path,
                "warning",
                "An MCP server without a direct command was not imported.",
            ));
            continue;
        };
        let args = match server.get("args") {
            None => Vec::new(),
            Some(Value::Array(args)) => {
                let Some(args) = args.iter().map(Value::as_str).collect::<Option<Vec<_>>>() else {
                    diagnostics.push(adapter_diagnostic(
                        path,
                        "warning",
                        "An MCP server with non-string arguments was not imported.",
                    ));
                    continue;
                };
                args.into_iter().map(str::to_owned).collect()
            }
            Some(_) => {
                diagnostics.push(adapter_diagnostic(
                    path,
                    "warning",
                    "An MCP server with invalid arguments was not imported.",
                ));
                continue;
            }
        };
        if !valid_adapter_text(name, api::MAX_MIGRATION_NAME_BYTES)
            || !valid_adapter_text(command, api::MAX_MIGRATION_COMMAND_BYTES)
            || args.len() > 64
            || args
                .iter()
                .any(|arg| !valid_adapter_text(arg, api::MAX_MIGRATION_ARGUMENT_BYTES))
        {
            diagnostics.push(adapter_diagnostic(
                path,
                "warning",
                "An MCP server exceeded migration safety bounds and was not imported.",
            ));
            continue;
        }
        servers.push(api::MigrationMcpServer {
            path: path.to_owned(),
            name: name.to_owned(),
            command: command.to_owned(),
            args,
        });
    }
}

fn discover_skill_files(
    source: &Path,
    diagnostics: &mut Vec<api::MigrationDiagnostic>,
) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let mut found = Vec::new();
    for root in SKILL_ROOT_CANDIDATES {
        let root = source.join(root);
        let metadata = match fs::symlink_metadata(&root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                diagnostics.push(adapter_diagnostic(
                    "$",
                    "warning",
                    "A Pi skill directory could not be inspected and was skipped.",
                ));
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            diagnostics.push(adapter_diagnostic(
                "$",
                "warning",
                "A Pi skill directory is not a regular directory and was skipped.",
            ));
            continue;
        }
        let mut entries = fs::read_dir(&root)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries.into_iter().take(MAX_SOURCE_ITEMS) {
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                diagnostics.push(adapter_diagnostic(
                    "$",
                    "warning",
                    "A Pi skill with a non-UTF-8 name was skipped.",
                ));
                continue;
            };
            let skill = path.join("SKILL.md");
            if octet_agent::secure_fs::read_regular_file_bounded(&skill, MAX_SKILL_BYTES).is_ok() {
                found.push((name, skill));
            }
        }
    }
    found.sort_by(|left, right| left.1.cmp(&right.1));
    found.dedup_by(|left, right| left.1 == right.1);
    Ok(found)
}

fn adapter_diagnostic(path: &str, severity: &str, reason: &str) -> api::MigrationDiagnostic {
    api::MigrationDiagnostic {
        path: path.to_owned(),
        severity: severity.to_owned(),
        reason: reason.to_owned(),
    }
}

fn valid_adapter_text(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && !value.chars().any(|character| character.is_control())
}

fn source_relative(source: &Path, path: &Path) -> anyhow::Result<String> {
    let relative = path
        .strip_prefix(source)
        .map_err(|_| anyhow::anyhow!("source path escaped the authorized source root"))?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            anyhow::bail!("source path is not normalized")
        };
        parts.push(
            part.to_str()
                .ok_or_else(|| anyhow::anyhow!("source path is not valid UTF-8"))?,
        );
    }
    if parts.is_empty() {
        anyhow::bail!("source path must not be the source root")
    }
    Ok(parts.join("/"))
}

fn path_to_utf8(path: &Path, label: &str) -> anyhow::Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("{label} must be valid UTF-8"))
}
