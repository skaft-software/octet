//! Bounded text and multimodal file reading.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use octet_ai::{AudioFormat, Media, Mime, ToolDef};
use serde::Deserialize;

use crate::effect::{ToolEffect, ToolPolicyDenialCode};
use crate::sandbox::RemoteReadRetryPolicy;
use crate::secure_fs::{read_regular_file_bounded_by, SecureFileError};
use crate::tool::{
    content_hash, CancellationToken, ReplaySafety, Tool, ToolConcurrency, ToolContext, ToolError,
    ToolOutput,
};
use crate::tools::{
    clip_line, parse_args, validate_effect_path, MAX_FILE_BYTES, MAX_TOOL_PATH_BYTES,
};
/// Display cap for a single line.
const MAX_LINE_CHARS: usize = 2000;
/// Default number of lines returned when `limit` is omitted.
const DEFAULT_LIMIT: usize = 500;
/// Conservative inline-image cap shared across supported provider paths.
const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Conservative inline-audio cap. Gemini's inline request limit is 20 MB.
const MAX_AUDIO_BYTES: usize = 20 * 1024 * 1024;
const REMOTE_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_READ_TIMEOUT: Duration = Duration::from_secs(30);
const REMOTE_DNS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq)]
enum MediaKind {
    Image(Mime),
    Audio(AudioFormat),
}

impl MediaKind {
    fn byte_limit(&self) -> usize {
        match self {
            Self::Image(_) => MAX_IMAGE_BYTES,
            Self::Audio(_) => MAX_AUDIO_BYTES,
        }
    }

    fn media_type(&self) -> &'static str {
        match self {
            Self::Image(mime) if mime.essence_str() == "image/png" => "image/png",
            Self::Image(mime) if mime.essence_str() == "image/jpeg" => "image/jpeg",
            Self::Image(mime) if mime.essence_str() == "image/gif" => "image/gif",
            Self::Image(mime) if mime.essence_str() == "image/webp" => "image/webp",
            Self::Image(_) => "image",
            Self::Audio(AudioFormat::Wav) => "audio/wav",
            Self::Audio(AudioFormat::Aac) => "audio/aac",
            Self::Audio(AudioFormat::Mp3) => "audio/mpeg",
            Self::Audio(AudioFormat::Flac) => "audio/flac",
            Self::Audio(AudioFormat::Opus) => "audio/opus",
            Self::Audio(AudioFormat::Pcm16) => "audio/pcm",
        }
    }

    fn into_media(self, bytes: Vec<u8>) -> Media {
        match self {
            Self::Image(mime) => Media::image_bytes(Bytes::from(bytes), mime),
            Self::Audio(format) => Media::audio_bytes(Bytes::from(bytes), format),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

/// The built-in `read` tool: bounded text, image, and audio reads.
pub struct ReadTool;

#[async_trait::async_trait]
impl Tool for ReadTool {
    fn composition_is_unmetered(&self) -> bool {
        true
    }

    fn definition(&self) -> ToolDef {
        ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "read".to_string(),
            description: "Read text, images, or audio. `path` may be a workspace-relative local path, \
                          an absolute/~/ path when trusted-local access is enabled, a local `file://` \
                          URL, or an HTTPS image/audio URL when remote reads are explicitly enabled. \
                          Text returns numbered lines plus a whole-file \
                          hash and continuation metadata. Image/audio returns bounded structured media \
                          for protocol-aware ingestion and a payload-free summary for the TUI; the active \
                          model may reject a recognized audio format it cannot accept. For independent \
                          file inspections, request all read/search calls together in one turn. Existing \
                          bracketed [Image #N]/[Audio #N] attachments are already included in the prompt \
                          and must not be read again."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Local file path, file:// URL, or (when explicitly enabled) an HTTPS image/audio URL."
                    },
                    "offset": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "1-indexed line to start from (default 1)."
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "Maximum lines to return (default 500)."
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        }
    }

    fn output_schema(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "content": {"type": "string", "description": "Returned lines without line-number prefixes, with the same per-line display clipping as a direct read."},
                        "path": {"type": "string"},
                        "hash": {"type": "string"},
                        "start_line": {"type": "integer", "minimum": 0},
                        "end_line": {"type": "integer", "minimum": 0},
                        "total_lines": {"type": "integer", "minimum": 0},
                        "next_offset": {"anyOf": [{"type": "integer", "minimum": 1}, {"type": "null"}]},
                        "truncated": {"type": "boolean", "description": "The output-byte budget omitted requested lines."},
                        "lines_clipped": {"type": "boolean", "description": "At least one returned line exceeded the per-line display cap."}
                    },
                    "required": ["content", "path", "hash", "start_line", "end_line", "total_lines", "next_offset", "truncated", "lines_clipped"],
                    "additionalProperties": false
                },
                {"type": "string", "description": "Payload-free image/audio summary; media remains available to the host."}
            ]
        }))
    }

    fn prompt_snippet(&self) -> Option<&str> {
        Some("Read file contents")
    }

    fn prompt_guidelines(&self) -> &[&str] {
        &["Use read to examine files instead of cat or sed."]
    }

    fn effect(
        &self,
        arguments: &serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        let arguments = arguments
            .as_object()
            .ok_or_else(|| ToolError::new("invalid arguments: expected an object"))?;
        if arguments.len() > 3
            || arguments
                .keys()
                .any(|key| !matches!(key.as_str(), "path" | "offset" | "limit"))
        {
            return Err(ToolError::new("invalid arguments: unknown property"));
        }
        let path = arguments
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ToolError::new("invalid arguments: `path` must be a string"))?;
        if path.is_empty() {
            return Err(ToolError::new(
                "invalid arguments: `path` must be non-empty",
            ));
        }
        if path.len() > MAX_TOOL_PATH_BYTES {
            return Err(ToolError::new(format!(
                "invalid arguments: `path` is {} bytes (limit {MAX_TOOL_PATH_BYTES})",
                path.len()
            )));
        }
        if path.contains('\0') {
            return Err(ToolError::new(
                "invalid arguments: `path` must not contain NUL",
            ));
        }
        for name in ["offset", "limit"] {
            if arguments.get(name).is_some_and(|value| {
                value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .is_none_or(|value| value == 0)
            }) {
                return Err(ToolError::new(format!(
                    "invalid arguments: `{name}` must be a positive integer"
                )));
            }
        }
        if let Some(url) = parse_url_source(path)? {
            let test_loopback_http = cfg!(test)
                && url.scheme() == "http"
                && url.host().is_some_and(|host| match host {
                    url::Host::Ipv4(address) => address.is_loopback(),
                    url::Host::Ipv6(address) => address.is_loopback(),
                    url::Host::Domain(_) => false,
                });
            match url.scheme() {
                "http" if test_loopback_http && !ctx.sandbox.allow_remote_read => {
                    return Err(ToolError::policy_denied(
                        ToolPolicyDenialCode::RemoteReadDisabled,
                        "remote URL reads are disabled; enable `allow_remote_read` \
                         (or pass `--allow-remote-read`) to permit public HTTPS image/audio fetches",
                    ));
                }
                "http" if test_loopback_http => return Ok(ToolEffect::Network),
                "http" => {
                    return Err(ToolError::new("remote media URLs must use HTTPS"));
                }
                "https" if !ctx.sandbox.allow_remote_read => {
                    return Err(ToolError::policy_denied(
                        ToolPolicyDenialCode::RemoteReadDisabled,
                        "remote URL reads are disabled; enable `allow_remote_read` \
                         (or pass `--allow-remote-read`) to permit public HTTPS image/audio fetches",
                    ));
                }
                "https" => return Ok(ToolEffect::Network),
                "file" => {
                    // Convert URL syntax lexically without resolving or probing
                    // the model-selected host path before policy admission.
                    let local = local_path_from_url(&url)?;
                    let local = workspace_relative_file_url_path(local, ctx);
                    validate_effect_path(&local, ctx.sandbox.allow_external_paths)?;
                    return Ok(if ctx.sandbox.allow_external_paths {
                        ToolEffect::HostRead
                    } else {
                        ToolEffect::WorkspaceRead
                    });
                }
                scheme => {
                    return Err(ToolError::new(format!(
                        "unsupported read URL scheme `{scheme}`; use file or https"
                    )));
                }
            }
        }
        validate_effect_path(path, ctx.sandbox.allow_external_paths)?;
        Ok(if ctx.sandbox.allow_external_paths {
            // Resolution follows symlinks and can disclose host-path existence.
            // Classify every local request by maximum ambient authority before
            // touching the filesystem.
            ToolEffect::HostRead
        } else {
            ToolEffect::WorkspaceRead
        })
    }

    fn replay_safety(&self) -> ReplaySafety {
        // The agent's reference monitor additionally requires the exact call to
        // classify as WorkspaceRead before honoring this static capability.
        ReplaySafety::Safe
    }

    fn concurrency(&self) -> ToolConcurrency {
        // Live read waves admit exact Pure, WorkspaceRead, or HostRead calls only
        // after effect classification and policy admission. HostRead remains
        // non-replayable; crash replay still requires exact Pure/WorkspaceRead,
        // while Network and all other effects remain sequential barriers.
        ToolConcurrency::Parallel
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.effect(&args, ctx)?;
        let args: ReadArgs = parse_args(args)?;
        if let Some(url) = parse_url_source(&args.path)? {
            return match url.scheme() {
                "http" | "https" if ctx.sandbox.allow_remote_read => {
                    let cancellation = ctx.cancellation.clone();
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => {
                            Err(ToolError::new("remote media read cancelled"))
                        }
                        result = read_remote_media(
                            url,
                            &ctx.sandbox.remote_read_retry,
                            cancellation.clone(),
                        ) => result,
                    }
                }
                "http" | "https" => Err(ToolError::policy_denied(
                    ToolPolicyDenialCode::RemoteReadDisabled,
                    "remote URL reads are disabled; enable `allow_remote_read` \
                     (or pass `--allow-remote-read`) to permit public HTTPS image/audio fetches",
                )),
                "file" => {
                    let path = local_path_from_url(&url)?;
                    let path = workspace_relative_file_url_path(path, ctx);
                    read_local(&args, &path, ctx).await
                }
                scheme => Err(ToolError::new(format!(
                    "unsupported read URL scheme `{scheme}`; use file or https"
                ))),
            };
        }
        read_local(&args, &args.path, ctx).await
    }
}

async fn read_local(
    args: &ReadArgs,
    requested_path: &str,
    ctx: &ToolContext<'_>,
) -> Result<ToolOutput, ToolError> {
    let display_path = ctx.display_path(requested_path);
    let target = ctx.resolve_existing(requested_path)?;
    let hinted_kind = media_kind_for_name(requested_path);
    let sniff_hint = hinted_kind.clone();
    let read_path = target.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        read_regular_file_bounded_by(&read_path, MAX_FILE_BYTES, |prefix| {
            let detected = sniff_media_kind(prefix, sniff_hint.as_ref());
            detected
                .iter()
                .chain(sniff_hint.iter())
                .map(MediaKind::byte_limit)
                .min()
                .unwrap_or(MAX_FILE_BYTES)
        })
    })
    .await
    .map_err(|error| ToolError::new(format!("{display_path}: read worker failed: {error}")))?
    .map_err(|error| match error {
        SecureFileError::NotRegular => ToolError::new(format!(
            "{display_path}: is not a regular file (a directory or special file is rejected)"
        )),
        other => ToolError::new(format!("{display_path}: {other}")),
    })?;
    match validated_media_kind(&bytes, hinted_kind.as_ref(), &display_path)? {
        Some(kind) => media_output(display_path, bytes, kind),
        None => text_output(args, ctx, display_path, &bytes),
    }
}

fn parse_url_source(value: &str) -> Result<Option<reqwest::Url>, ToolError> {
    let looks_like_url = value.contains("://") || value.starts_with("file:");
    if !looks_like_url {
        return Ok(None);
    }
    let url = reqwest::Url::parse(value)
        .map_err(|error| ToolError::new(format!("invalid read URL: {error}")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ToolError::new(
            "read URLs must not contain embedded credentials",
        ));
    }
    if url.fragment().is_some() {
        return Err(ToolError::new("read URLs must not contain fragments"));
    }
    Ok(Some(url))
}

fn local_path_from_url(url: &reqwest::Url) -> Result<String, ToolError> {
    let mut local = url.clone();
    match local.host_str() {
        None | Some("") => {}
        Some("localhost") => {
            local
                .set_host(None)
                .map_err(|_| ToolError::new("invalid localhost file URL"))?;
        }
        Some(host) => {
            return Err(ToolError::new(format!(
                "file URL host `{host}` is not local"
            )));
        }
    }
    local
        .to_file_path()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|_| ToolError::new("file URL does not contain a valid local path"))
}

fn workspace_relative_file_url_path(path: String, ctx: &ToolContext<'_>) -> String {
    if ctx.sandbox.allow_external_paths {
        return path;
    }
    if let Some(relative) = strip_workspace_prefix(&path, ctx.workspace) {
        return relative;
    }
    path
}

/// Lexically relativize a URL-derived absolute path against the workspace.
///
/// Plain `strip_prefix` first (covers Unix and Windows when both spellings
/// agree). On Windows the workspace is usually canonicalized, so it carries
/// a `\\?\` verbatim prefix while URL-derived paths do not, and drive-letter
/// case may differ; fall back to a normalized comparison that slices the
/// original spelling at the matched length.
fn strip_workspace_prefix(path: &str, workspace: &Path) -> Option<String> {
    if let Some(relative) = Path::new(path)
        .strip_prefix(workspace)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
    {
        return Some(relative.to_string_lossy().into_owned());
    }
    #[cfg(windows)]
    {
        let workspace_text = workspace.to_string_lossy();
        let workspace_body = workspace_text
            .strip_prefix(r"\\?\")
            .unwrap_or(&workspace_text);
        let path_body = path.strip_prefix(r"\\?\").unwrap_or(path);
        // Separator replacement preserves byte indices, so a match in the
        // normalized spelling maps back onto the original slice directly.
        let workspace_norm = workspace_body.replace('/', "\\");
        let path_norm = path_body.replace('/', "\\");
        if path_norm.len() > workspace_norm.len() + 1
            && path_norm
                .get(..workspace_norm.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(&workspace_norm))
            && path_norm.as_bytes()[workspace_norm.len()] == b'\\'
        {
            let cut = path.len() - path_body.len() + workspace_norm.len() + 1;
            let relative = &path[cut..];
            if !relative.is_empty() {
                return Some(relative.to_owned());
            }
        }
    }
    None
}

#[cfg(all(test, windows))]
#[test]
fn windows_unicode_workspace_prefix_comparison_is_boundary_safe() {
    assert_eq!(
        strip_workspace_prefix(r"C:\worké\outside.txt", Path::new(r"C:\workx")),
        None
    );
    assert_eq!(
        strip_workspace_prefix(r"C:\work😀\outside.txt", Path::new(r"\\?\C:\workxxx")),
        None
    );
    assert_eq!(
        strip_workspace_prefix(r"c:\répo\src\文件.rs", Path::new(r"\\?\C:\répo")),
        Some(r"src\文件.rs".into())
    );
    assert_eq!(
        strip_workspace_prefix(r"\\?\c:\répo\src\文件.rs", Path::new(r"C:\répo")),
        Some(r"src\文件.rs".into())
    );
}

async fn validated_remote_endpoint(
    url: &reqwest::Url,
    display: &str,
) -> Result<(String, Option<std::net::IpAddr>, Vec<std::net::SocketAddr>), ToolError> {
    let (host, literal_ip) = match url
        .host()
        .ok_or_else(|| ToolError::new(format!("{display}: URL has no host")))?
    {
        url::Host::Domain(domain) => (domain.trim_end_matches('.').to_ascii_lowercase(), None),
        url::Host::Ipv4(address) => {
            let address = std::net::IpAddr::V4(address);
            (address.to_string(), Some(address))
        }
        url::Host::Ipv6(address) => {
            let address = std::net::IpAddr::V6(address);
            (address.to_string(), Some(address))
        }
    };
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host == "metadata.google.internal"
    {
        return Err(ToolError::new(format!(
            "{display}: local or metadata hosts are not allowed"
        )));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| ToolError::new(format!("{display}: URL has no known port")))?;
    let test_loopback_http =
        cfg!(test) && url.scheme() == "http" && literal_ip.is_some_and(|ip| ip.is_loopback());
    match url.scheme() {
        "https" if port == 443 => {}
        "https" => {
            return Err(ToolError::new(format!(
                "{display}: HTTPS media URLs must use port 443"
            )));
        }
        "http" if test_loopback_http => {}
        "http" => {
            return Err(ToolError::new(format!(
                "{display}: remote media URLs must use HTTPS"
            )));
        }
        scheme => {
            return Err(ToolError::new(format!(
                "{display}: unsupported remote URL scheme `{scheme}`"
            )));
        }
    }

    let addresses = if let Some(address) = literal_ip {
        vec![std::net::SocketAddr::new(address, port)]
    } else {
        tokio::time::timeout(
            REMOTE_DNS_TIMEOUT,
            tokio::net::lookup_host((host.as_str(), port)),
        )
        .await
        .map_err(|_| ToolError::new(format!("{display}: DNS lookup timed out")))?
        .map_err(|error| ToolError::new(format!("{display}: DNS lookup failed: {error}")))?
        .collect::<Vec<_>>()
    };
    if addresses.is_empty() {
        return Err(ToolError::new(format!(
            "{display}: DNS lookup returned no addresses"
        )));
    }
    if test_loopback_http {
        if addresses.iter().any(|address| !address.ip().is_loopback()) {
            return Err(ToolError::new(format!(
                "{display}: test HTTP URLs must resolve only to loopback"
            )));
        }
    } else if addresses
        .iter()
        .any(|address| !is_public_remote_ip(address.ip()))
    {
        return Err(ToolError::new(format!(
            "{display}: private, link-local, metadata, and non-public targets are not allowed"
        )));
    }
    Ok((host, literal_ip, addresses))
}

fn is_public_remote_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, c, d] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0 && c == 0)
                || (a == 192 && b == 0 && c == 2)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113)
                || a >= 224
                || (a == 255 && b == 255 && c == 255 && d == 255))
        }
        std::net::IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4() {
                return is_public_remote_ip(std::net::IpAddr::V4(mapped));
            }
            let octets = ip.octets();
            // RFC 6052's well-known NAT64 prefix embeds an IPv4 address in the
            // final 32 bits. Apply the IPv4 denylist to the translated target,
            // while rejecting the local-use translation prefix outright.
            if octets[..12] == [0x00, 0x64, 0xff, 0x9b, 0, 0, 0, 0, 0, 0, 0, 0] {
                return is_public_remote_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                    octets[12], octets[13], octets[14], octets[15],
                )));
            }
            if octets[..6] == [0x00, 0x64, 0xff, 0x9b, 0x00, 0x01] {
                return false;
            }
            // 6to4 exposes its embedded IPv4 relay target directly. Teredo
            // carries multiple obfuscated IPv4 fields; reject it entirely
            // rather than attempt a permissive partial decode.
            if octets[..2] == [0x20, 0x02] {
                return is_public_remote_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::new(
                    octets[2], octets[3], octets[4], octets[5],
                )));
            }
            if octets[..4] == [0x20, 0x01, 0x00, 0x00] {
                return false;
            }
            let segments = ip.segments();
            // Remote reads are public-only. Start from the global-unicast
            // allocation (2000::/3), then subtract special-purpose space
            // within it; everything else is fail-closed.
            segments[0] & 0xe000 == 0x2000
                && !(ip.is_unspecified()
                    || ip.is_loopback()
                    || ip.is_multicast()
                    || segments[0] & 0xfe00 == 0xfc00
                    || segments[0] & 0xffc0 == 0xfe80
                    || segments[0] & 0xffc0 == 0xfec0
                    || (segments[0] == 0x2001 && segments[1] <= 0x01ff)
                    || (segments[0] == 0x2001 && segments[1] == 0x0db8)
                    || (segments[0] == 0x3fff && segments[1] & 0xf000 == 0))
        }
    }
}

const REMOTE_CLIENT_CACHE_CAPACITY: usize = 16;

#[derive(Clone, PartialEq, Eq)]
struct RemoteClientKey {
    host: String,
    endpoints: Vec<std::net::SocketAddr>,
    https_only: bool,
}

fn remote_client(
    host: &str,
    endpoints: &[std::net::SocketAddr],
    https_only: bool,
) -> Result<reqwest::Client, ToolError> {
    static CLIENTS: OnceLock<Mutex<VecDeque<(RemoteClientKey, reqwest::Client)>>> = OnceLock::new();
    let clients = CLIENTS.get_or_init(|| Mutex::new(VecDeque::new()));
    let key = RemoteClientKey {
        host: host.to_owned(),
        endpoints: endpoints.to_vec(),
        https_only,
    };

    {
        let mut cache = clients.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(index) = cache.iter().position(|(cached, _)| cached == &key) {
            let entry = cache.remove(index).expect("matching client cache entry");
            let client = entry.1.clone();
            cache.push_back(entry);
            return Ok(client);
        }
    }

    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(REMOTE_CONNECT_TIMEOUT)
        .timeout(REMOTE_READ_TIMEOUT)
        .user_agent(concat!("octet/", env!("CARGO_PKG_VERSION")))
        .no_proxy()
        .resolve_to_addrs(host, endpoints);
    if https_only {
        builder = builder.https_only(true);
    }
    let client = builder
        .build()
        .map_err(|error| ToolError::new(format!("remote media client failed: {error}")))?;

    let mut cache = clients.lock().unwrap_or_else(|error| error.into_inner());
    if let Some((_, cached)) = cache.iter().find(|(cached, _)| cached == &key) {
        return Ok(cached.clone());
    }
    if cache.len() == REMOTE_CLIENT_CACHE_CAPACITY {
        cache.pop_front();
    }
    cache.push_back((key, client.clone()));
    Ok(client)
}

enum RemoteAttemptError {
    Retryable(ToolError),
    Fatal(ToolError),
}

async fn read_remote_media(
    mut url: reqwest::Url,
    retry_policy: &RemoteReadRetryPolicy,
    cancellation: CancellationToken,
) -> Result<ToolOutput, ToolError> {
    let requested_display = display_remote_url(&url);
    let (host, literal_ip, endpoints) = validated_remote_endpoint(&url, &requested_display).await?;
    // `ClientBuilder::resolve` keys exact host spellings. Normalize the URL to
    // the validated spelling too, otherwise a trailing dot can miss the pinned
    // override and trigger a second, unvalidated DNS lookup at connect time.
    if let Some(address) = literal_ip {
        url.set_ip_host(address)
            .map_err(|_| ToolError::new(format!("{requested_display}: invalid normalized host")))?;
    } else {
        url.set_host(Some(&host))
            .map_err(|_| ToolError::new(format!("{requested_display}: invalid normalized host")))?;
    }
    // Cache only clients with the same validated hostname-to-address pin. A
    // DNS change creates a different key rather than reusing an unvalidated
    // connection pool.
    let client = remote_client(&host, &endpoints, url.scheme() == "https")?;
    let max_attempts = retry_policy.attempts();

    for attempt in 1..=max_attempts {
        if cancellation.is_cancelled() {
            return Err(ToolError::new("remote media read cancelled"));
        }
        match read_remote_media_attempt(&client, &url, &requested_display).await {
            Ok(output) => return Ok(output),
            Err(RemoteAttemptError::Fatal(error)) => return Err(error),
            Err(RemoteAttemptError::Retryable(error)) if attempt == max_attempts => {
                return Err(error);
            }
            Err(RemoteAttemptError::Retryable(_error)) => {
                let delay = retry_policy.backoff_for_retry(attempt);
                let completed = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => false,
                    _ = tokio::time::sleep(delay) => true,
                };
                if !completed {
                    return Err(ToolError::new("remote media read cancelled"));
                }
            }
        }
    }

    unreachable!("remote media retry loop always has at least one attempt")
}

async fn read_remote_media_attempt(
    client: &reqwest::Client,
    url: &reqwest::Url,
    requested_display: &str,
) -> Result<ToolOutput, RemoteAttemptError> {
    let mut response = client
        .get(url.clone())
        .header(reqwest::header::ACCEPT, "image/*, audio/*")
        .send()
        .await
        .map_err(|error| {
            let retryable =
                error.is_timeout() || error.is_connect() || error.is_request() || error.is_body();
            let display = ToolError::new(format!(
                "{requested_display}: request failed: {}",
                error.without_url()
            ));
            if retryable {
                RemoteAttemptError::Retryable(display)
            } else {
                RemoteAttemptError::Fatal(display)
            }
        })?;
    let response_display = display_remote_url(response.url());
    let status = response.status();
    if !status.is_success() {
        let error = ToolError::new(format!("{response_display}: HTTP {status}"));
        return Err(if retryable_remote_status(status) {
            RemoteAttemptError::Retryable(error)
        } else {
            RemoteAttemptError::Fatal(error)
        });
    }

    let extension_hint = media_kind_for_name(response.url().path());
    let content_type_hint = response_media_kind(response.headers(), &response_display)
        .map_err(RemoteAttemptError::Fatal)?;
    if let Some(extension) = &extension_hint {
        if extension != &content_type_hint {
            return Err(RemoteAttemptError::Fatal(ToolError::new(format!(
                "{response_display}: URL extension indicates {} but Content-Type is {}",
                extension.media_type(),
                content_type_hint.media_type()
            ))));
        }
    }
    let hinted_kind = content_type_hint;
    let byte_limit = hinted_kind.byte_limit();
    if response
        .content_length()
        .is_some_and(|length| length > byte_limit as u64)
    {
        return Err(RemoteAttemptError::Fatal(media_too_large_error(
            &response_display,
            byte_limit,
            response.content_length(),
        )));
    }

    let mut bytes = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or(0)
            .min(byte_limit as u64) as usize,
    );
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        RemoteAttemptError::Retryable(ToolError::new(format!(
            "{response_display}: response read failed: {}",
            error.without_url()
        )))
    })? {
        if bytes.len().saturating_add(chunk.len()) > byte_limit {
            return Err(RemoteAttemptError::Fatal(media_too_large_error(
                &response_display,
                byte_limit,
                Some(bytes.len().saturating_add(chunk.len()) as u64),
            )));
        }
        bytes.extend_from_slice(&chunk);
    }

    let kind = validated_media_kind(&bytes, Some(&hinted_kind), &response_display)
        .map_err(RemoteAttemptError::Fatal)?
        .ok_or_else(|| {
            RemoteAttemptError::Fatal(ToolError::new(format!(
                "{response_display}: remote reads accept supported image or audio content only"
            )))
        })?;
    media_output(response_display, bytes, kind).map_err(RemoteAttemptError::Fatal)
}

fn retryable_remote_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn response_media_kind(
    headers: &reqwest::header::HeaderMap,
    display: &str,
) -> Result<MediaKind, ToolError> {
    let Some(value) = headers.get(reqwest::header::CONTENT_TYPE) else {
        return Err(ToolError::new(format!(
            "{display}: remote media response is missing Content-Type"
        )));
    };
    let value = value
        .to_str()
        .map_err(|_| ToolError::new(format!("{display}: invalid Content-Type header")))?;
    let mime = value
        .parse::<Mime>()
        .map_err(|_| ToolError::new(format!("{display}: invalid Content-Type `{value}`")))?;
    media_kind_for_mime(&mime).ok_or_else(|| {
        ToolError::new(format!(
            "{display}: unsupported remote Content-Type `{}`",
            mime.essence_str()
        ))
    })
}

fn display_remote_url(url: &reqwest::Url) -> String {
    let had_query = url.query().is_some();
    let mut display = url.clone();
    display.set_query(None);
    display.set_fragment(None);
    let mut value = display.to_string();
    if had_query {
        value.push_str("?…");
    }
    value
}

fn media_too_large_error(display: &str, byte_limit: usize, actual: Option<u64>) -> ToolError {
    let actual = actual.map_or_else(String::new, |actual| format!(" ({actual} bytes)"));
    ToolError::new(format!(
        "{display}: media exceeds the {} MB limit{actual}",
        byte_limit / (1024 * 1024)
    ))
}

fn media_kind_for_name(value: &str) -> Option<MediaKind> {
    let extension = Path::new(value).extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "png" => Some(MediaKind::Image(image_mime("image/png"))),
        "jpg" | "jpeg" => Some(MediaKind::Image(image_mime("image/jpeg"))),
        "gif" => Some(MediaKind::Image(image_mime("image/gif"))),
        "webp" => Some(MediaKind::Image(image_mime("image/webp"))),
        "wav" => Some(MediaKind::Audio(AudioFormat::Wav)),
        "mp3" => Some(MediaKind::Audio(AudioFormat::Mp3)),
        "flac" => Some(MediaKind::Audio(AudioFormat::Flac)),
        "opus" | "ogg" => Some(MediaKind::Audio(AudioFormat::Opus)),
        "aac" | "m4a" => Some(MediaKind::Audio(AudioFormat::Aac)),
        _ => None,
    }
}

fn media_kind_for_mime(mime: &Mime) -> Option<MediaKind> {
    match mime.essence_str() {
        "image/png" => Some(MediaKind::Image(image_mime("image/png"))),
        "image/jpeg" => Some(MediaKind::Image(image_mime("image/jpeg"))),
        "image/gif" => Some(MediaKind::Image(image_mime("image/gif"))),
        "image/webp" => Some(MediaKind::Image(image_mime("image/webp"))),
        "audio/wav" | "audio/wave" | "audio/x-wav" => Some(MediaKind::Audio(AudioFormat::Wav)),
        "audio/mpeg" | "audio/mp3" => Some(MediaKind::Audio(AudioFormat::Mp3)),
        "audio/flac" | "audio/x-flac" => Some(MediaKind::Audio(AudioFormat::Flac)),
        "audio/opus" | "audio/ogg" => Some(MediaKind::Audio(AudioFormat::Opus)),
        "audio/aac" | "audio/mp4" | "audio/x-m4a" => Some(MediaKind::Audio(AudioFormat::Aac)),
        _ => None,
    }
}

fn image_mime(value: &'static str) -> Mime {
    value.parse().expect("static image MIME is valid")
}

fn validated_media_kind(
    bytes: &[u8],
    hint: Option<&MediaKind>,
    display: &str,
) -> Result<Option<MediaKind>, ToolError> {
    let detected = sniff_media_kind(bytes, hint);
    match (hint, detected) {
        (Some(expected), Some(actual)) if expected != &actual => Err(ToolError::new(format!(
            "{display}: declared {} does not match detected {} content",
            expected.media_type(),
            actual.media_type()
        ))),
        (Some(expected), None) => Err(ToolError::new(format!(
            "{display}: content does not match declared {} media",
            expected.media_type()
        ))),
        (_, detected) => Ok(detected),
    }
}

fn sniff_media_kind(bytes: &[u8], hint: Option<&MediaKind>) -> Option<MediaKind> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(MediaKind::Image(image_mime("image/png")));
    }
    if bytes.starts_with(b"\xff\xd8\xff") {
        return Some(MediaKind::Image(image_mime("image/jpeg")));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(MediaKind::Image(image_mime("image/gif")));
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(MediaKind::Image(image_mime("image/webp")));
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE" {
        return Some(MediaKind::Audio(AudioFormat::Wav));
    }
    if bytes.starts_with(b"fLaC") {
        return Some(MediaKind::Audio(AudioFormat::Flac));
    }
    if bytes.starts_with(b"OggS")
        && bytes
            .windows(b"OpusHead".len())
            .take(128)
            .any(|window| window == b"OpusHead")
    {
        return Some(MediaKind::Audio(AudioFormat::Opus));
    }
    if bytes.starts_with(b"ID3")
        || bytes.get(..2).is_some_and(|prefix| {
            prefix[0] == 0xff && prefix[1] & 0xe0 == 0xe0 && prefix[1] & 0x06 != 0
        })
    {
        return Some(MediaKind::Audio(AudioFormat::Mp3));
    }
    if bytes
        .get(..2)
        .is_some_and(|prefix| prefix[0] == 0xff && prefix[1] & 0xf6 == 0xf0)
    {
        return Some(MediaKind::Audio(AudioFormat::Aac));
    }
    if matches!(hint, Some(MediaKind::Audio(AudioFormat::Aac)))
        && bytes.len() >= 12
        && &bytes[4..8] == b"ftyp"
    {
        return Some(MediaKind::Audio(AudioFormat::Aac));
    }
    None
}

fn media_output(
    display_path: String,
    bytes: Vec<u8>,
    kind: MediaKind,
) -> Result<ToolOutput, ToolError> {
    if bytes.len() > kind.byte_limit() {
        return Err(media_too_large_error(
            &display_path,
            kind.byte_limit(),
            Some(bytes.len() as u64),
        ));
    }
    let hash = content_hash(&bytes);
    let byte_len = bytes.len();
    let media_type = kind.media_type();
    let read_kind = match &kind {
        MediaKind::Image(_) => "vision",
        MediaKind::Audio(_) => "audio",
    };
    let media = kind.into_media(bytes);
    Ok(ToolOutput::new(format!(
        "{display_path}: media={media_type} bytes={byte_len} hash={hash}\nread={read_kind}"
    ))
    .with_media(media))
}

fn text_output(
    args: &ReadArgs,
    ctx: &ToolContext<'_>,
    display_path: String,
    bytes: &[u8],
) -> Result<ToolOutput, ToolError> {
    let hash = content_hash(bytes);
    let text = String::from_utf8_lossy(bytes);
    let offset = args.offset.unwrap_or(1).max(1);
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT).max(1);

    // Reserve some budget for the header/footer lines. Count and render in one
    // pass so newline-dense files do not allocate one fat reference per line.
    let byte_budget = ctx.sandbox.max_output_bytes.saturating_sub(256).max(1024);
    let requested_end = offset.saturating_add(limit.saturating_sub(1));
    let mut body = String::new();
    let mut content = String::new();
    let mut lines_clipped = false;
    let mut total = 0usize;
    let mut end = offset - 1; // last included line
    let mut truncated = false;
    for (index, (line, source)) in text.lines().zip(text.split_inclusive('\n')).enumerate() {
        let line_number = index + 1;
        total = line_number;
        if line_number < offset || line_number > requested_end || truncated {
            continue;
        }
        let clipped = clip_line(line, MAX_LINE_CHARS);
        let rendered = format!("{line_number}: {clipped}\n");
        if !body.is_empty() && body.len() + rendered.len() > byte_budget {
            truncated = true;
            continue;
        }
        body.push_str(&rendered);
        if ctx.progress.is_programmatic() {
            lines_clipped |= clipped != line;
            content.push_str(&clipped);
            // Preserve source line endings, including CRLF and an unterminated
            // final line, rather than copying the model's numbered rendering.
            content.push_str(&source[line.len()..]);
        }
        end = line_number;
    }

    if total == 0 {
        let output = ToolOutput::new(format!(
            "{display_path}:0-0/0 hash={hash}\n(empty file)\ntruncated=false"
        ));
        return if ctx.progress.is_programmatic() {
            output
                .try_with_programmatic_content(serde_json::json!({
                    "content": "", "path": display_path, "hash": hash,
                    "start_line": 0, "end_line": 0, "total_lines": 0,
                    "next_offset": null, "truncated": false, "lines_clipped": false
                }))
                .map_err(|error| ToolError::new(error.to_string()))
        } else {
            Ok(output)
        };
    }
    if offset > total {
        return Err(ToolError::new(format!(
            "{display_path}: offset {offset} is beyond the end of the file ({total} lines)"
        )));
    }

    let header = format!("{display_path}:{offset}-{end}/{total} hash={hash}");
    let footer = if end < total {
        format!("next_offset={} truncated={truncated}", end + 1)
    } else {
        format!("truncated={truncated}")
    };
    let output = ToolOutput::new(format!("{header}\n{body}{footer}"));
    if ctx.progress.is_programmatic() {
        output
            .try_with_programmatic_content(serde_json::json!({
                "content": content, "path": display_path, "hash": hash,
                "start_line": offset, "end_line": end, "total_lines": total,
                "next_offset": (end < total).then_some(end + 1),
                "truncated": truncated, "lines_clipped": lines_clipped
            }))
            .map_err(|error| ToolError::new(error.to_string()))
    } else {
        Ok(output)
    }
}

#[cfg(test)]
mod tests;
