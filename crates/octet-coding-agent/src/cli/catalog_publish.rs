//! Fail-closed publication of a checked catalog document to an immutable path.
//!
//! A catalog documents the providers/models a build can serve. Publishing one is
//! additive and local: the publisher declares the expected metadata (minimum
//! client version, required provider set, entry count and checksum) and the
//! exact bytes are installed only when every gate agrees. The destination is
//! treated as immutable — an existing path is never replaced, so a rejected
//! publication always leaves the previous catalog untouched.
//!
//! Every gate fails closed. A mismatch in the checksum, entry count, minimum
//! client version, required provider set or destination path refuses the whole
//! publication before any file is created.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;

/// The only catalog document schema this binary understands. A newer or
/// unknown schema is refused rather than published.
pub const CATALOG_SCHEMA: &str = "octet-catalog-1";
/// Hard upper bound for one catalog document read from disk.
pub const MAX_CATALOG_BYTES: u64 = 16 * 1024 * 1024;

static TEMP_SUFFIX: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Subcommand)]
pub enum CatalogCommand {
    /// Validate every publish gate and atomically install a catalog document.
    Publish {
        /// Catalog document to publish (regular file, bounded size).
        source: PathBuf,
        /// Immutable destination path. An existing path is never replaced.
        #[arg(long, value_name = "PATH")]
        destination: PathBuf,
        /// Minimum octet version the document requires. Must match the document.
        #[arg(long, value_name = "SEMVER")]
        min_client_version: String,
        /// Provider id the document requires (repeatable or comma-separated).
        #[arg(long = "require-provider", value_name = "ID", value_delimiter = ',')]
        required_providers: Vec<String>,
        /// Exact number of catalog entries the document must contain.
        #[arg(long, value_name = "N")]
        expected_count: u64,
        /// Expected lowercase hex sha256 of the exact source bytes.
        #[arg(long, value_name = "HEX")]
        expected_checksum: String,
    },
}

/// How a publication was requested, independent of how the envelope was read.
#[derive(Clone, Copy, Debug)]
pub struct PublishRequest<'a> {
    pub source: &'a Path,
    pub destination: &'a Path,
    pub min_client_version: &'a str,
    pub required_providers: &'a [String],
    pub expected_count: u64,
    pub expected_checksum: &'a str,
}

/// What a successful publication installed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PublishReceipt {
    pub checksum: String,
    pub entries: u64,
    pub min_client_version: String,
    pub required_providers: Vec<String>,
}

/// A strictly validated catalog document. Unknown fields are refused so a
/// future schema cannot be silently truncated into today's shape.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogDocument {
    schema: String,
    min_client_version: String,
    required_providers: Vec<String>,
    entries: Vec<serde_json::Value>,
}

/// Which publish gate refused a document. Each variant is a distinct, testable
/// gate; no gate can be satisfied by inference from another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishGate {
    Checksum,
    Schema,
    MinClientVersion,
    RequiredProvider,
    EntryCount,
    ImmutablePath,
}

impl std::fmt::Display for PublishGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Checksum => "checksum",
            Self::Schema => "schema",
            Self::MinClientVersion => "minimum-client-version",
            Self::RequiredProvider => "required-provider",
            Self::EntryCount => "entry-count",
            Self::ImmutablePath => "immutable-path",
        })
    }
}

/// Validate every gate and, only on success, install the document atomically.
///
/// `available_providers` is the set of provider ids this build can serve and
/// `client_version` is the running binary version.
pub fn publish(
    request: &PublishRequest<'_>,
    available_providers: &BTreeSet<String>,
    client_version: &str,
) -> anyhow::Result<PublishReceipt> {
    let bytes = octet_agent::secure_fs::read_regular_file_bounded(
        request.source,
        MAX_CATALOG_BYTES as usize,
    )
    .map_err(|error| anyhow::anyhow!("catalog publish refused (source): {error}"))?;

    // Gate: checksum. The exact bytes are validated before parsing, so a
    // corrupted or substituted document never reaches the JSON decoder.
    let checksum = format!("{:x}", Sha256::digest(&bytes));
    if !checksum.eq_ignore_ascii_case(request.expected_checksum.trim()) {
        return refuse(
            PublishGate::Checksum,
            format!(
                "expected {}, computed {checksum}",
                request.expected_checksum
            ),
        );
    }

    // Gate: schema. A malformed body is a schema failure, not a checksum one.
    let document: CatalogDocument = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!("catalog publish refused (schema): invalid document: {error}")
    })?;
    if document.schema != CATALOG_SCHEMA {
        return refuse(
            PublishGate::Schema,
            format!(
                "document schema {:?}, expected {CATALOG_SCHEMA:?}",
                document.schema
            ),
        );
    }

    // Gate: minimum client version. The declared value must match the request,
    // and this client must satisfy it.
    if document.min_client_version != request.min_client_version {
        return refuse(
            PublishGate::MinClientVersion,
            format!(
                "document declares {:?}, request expects {:?}",
                document.min_client_version, request.min_client_version
            ),
        );
    }
    let required = semver::Version::parse(&document.min_client_version).map_err(|error| {
        anyhow::anyhow!("catalog publish refused (minimum-client-version): {error}")
    })?;
    let current = semver::Version::parse(client_version).map_err(|error| {
        anyhow::anyhow!(
            "catalog publish refused (minimum-client-version): invalid client version: {error}"
        )
    })?;
    if required > current {
        return refuse(
            PublishGate::MinClientVersion,
            format!("document requires {required}, this client is {current}"),
        );
    }

    // Gate: required providers. The declared set must match the request exactly
    // and every entry must be one this build can serve.
    let declared = document
        .required_providers
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let expected = request
        .required_providers
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    if declared != expected {
        return refuse(
            PublishGate::RequiredProvider,
            format!("document declares {declared:?}, request requires {expected:?}"),
        );
    }
    for provider in &declared {
        if !available_providers.contains(provider) {
            return refuse(
                PublishGate::RequiredProvider,
                format!("provider {provider:?} is not available in this build"),
            );
        }
    }

    // Gate: entry count.
    let entries = u64::try_from(document.entries.len()).unwrap_or(u64::MAX);
    if entries != request.expected_count {
        return refuse(
            PublishGate::EntryCount,
            format!(
                "document has {entries} entries, expected {}",
                request.expected_count
            ),
        );
    }

    // Gate: immutable path. A published catalog is never replaced, so an
    // existing destination refuses the publication and stays untouched.
    install_immutable(request.destination, &bytes)?;

    Ok(PublishReceipt {
        checksum,
        entries,
        min_client_version: document.min_client_version,
        required_providers: declared.into_iter().collect(),
    })
}

fn refuse<T>(gate: PublishGate, detail: impl std::fmt::Display) -> anyhow::Result<T> {
    anyhow::bail!("catalog publish refused ({gate}): {detail}")
}

/// Install bytes at an immutable destination using an atomic create-if-absent
/// link. The destination is never opened for truncation, so a refusal cannot
/// damage an existing catalog.
fn install_immutable(destination: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    match destination.symlink_metadata() {
        Ok(_) => {
            return refuse(
                PublishGate::ImmutablePath,
                format!("destination already exists: {}", destination.display()),
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return refuse(
                PublishGate::ImmutablePath,
                format!("cannot inspect {}: {error}", destination.display()),
            )
        }
    }
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let parent = parent.unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return refuse(
            PublishGate::ImmutablePath,
            format!("destination directory does not exist: {}", parent.display()),
        );
    }
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("catalog");
    let suffix = TEMP_SUFFIX.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{file_name}.publish-{}-{suffix}",
        std::process::id()
    ));

    let write_result = (|| -> std::io::Result<()> {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temporary);
        return Err(anyhow::anyhow!("catalog publish refused (write): {error}"));
    }
    // `hard_link` fails with `AlreadyExists` if a concurrent publisher created
    // the destination between the gate and this call, so immutability holds
    // across processes without a separate lock.
    let link_result = std::fs::hard_link(&temporary, destination);
    let _ = std::fs::remove_file(&temporary);
    match link_result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => refuse(
            PublishGate::ImmutablePath,
            format!("destination already exists: {}", destination.display()),
        ),
        Err(error) => Err(anyhow::anyhow!("catalog publish refused (write): {error}")),
    }
}

/// Run the `octet catalog publish` subcommand.
pub(crate) fn run(command: CatalogCommand, _config: &Config) -> anyhow::Result<()> {
    match command {
        CatalogCommand::Publish {
            source,
            destination,
            min_client_version,
            required_providers,
            expected_count,
            expected_checksum,
        } => {
            let available = available_provider_ids();
            let request = PublishRequest {
                source: &source,
                destination: &destination,
                min_client_version: &min_client_version,
                required_providers: &required_providers,
                expected_count,
                expected_checksum: &expected_checksum,
            };
            let receipt = publish(&request, &available, env!("CARGO_PKG_VERSION"))?;
            crate::output::stdout_line(format!(
                "Published catalog to {} ({} entries, sha256 {}).",
                destination.display(),
                receipt.entries,
                receipt.checksum
            ));
            Ok(())
        }
    }
}

/// Provider ids this build can serve, independent of configured credentials.
fn available_provider_ids() -> BTreeSet<String> {
    crate::providers::builtin_provider_definitions()
        .into_iter()
        .map(|definition| definition.id().to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(checksum_source: &str) -> (String, String) {
        let bytes = checksum_source.to_owned();
        let checksum = format!("{:x}", Sha256::digest(bytes.as_bytes()));
        (bytes, checksum)
    }

    fn available() -> BTreeSet<String> {
        ["openai".to_owned(), "anthropic".to_owned()]
            .into_iter()
            .collect()
    }

    fn body(min_client_version: &str, providers: &[&str], entries: usize) -> String {
        let entries = (0..entries)
            .map(|index| format!("{{\"provider\":\"openai\",\"model\":\"m{index}\"}}"))
            .collect::<Vec<_>>()
            .join(",");
        let providers = providers
            .iter()
            .map(|provider| format!("\"{provider}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "{{\"schema\":\"{CATALOG_SCHEMA}\",\"min_client_version\":\"{min_client_version}\",\"required_providers\":[{providers}],\"entries\":[{entries}]}}"
        )
    }

    fn request<'a>(
        source: &'a Path,
        destination: &'a Path,
        min_client_version: &'a str,
        providers: &'a [String],
        count: u64,
        checksum: &'a str,
    ) -> PublishRequest<'a> {
        PublishRequest {
            source,
            destination,
            min_client_version,
            required_providers: providers,
            expected_count: count,
            expected_checksum: checksum,
        }
    }

    #[test]
    fn publishes_when_every_gate_agrees() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("catalog.json");
        let destination = temp.path().join("published.json");
        let (body, checksum) = document(&body("0.1.0", &["openai"], 2));
        std::fs::write(&source, &body).unwrap();
        let providers = vec!["openai".to_owned()];
        let request = request(&source, &destination, "0.1.0", &providers, 2, &checksum);
        let receipt = publish(&request, &available(), "9.9.9").unwrap();
        assert_eq!(receipt.entries, 2);
        assert_eq!(receipt.required_providers, vec!["openai".to_owned()]);
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), body);
    }

    #[test]
    fn checksum_mismatch_refuses_and_leaves_no_destination() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("catalog.json");
        let destination = temp.path().join("published.json");
        std::fs::write(&source, body("0.1.0", &["openai"], 1)).unwrap();
        let providers = vec!["openai".to_owned()];
        let checksum = "00".repeat(32);
        let request = request(&source, &destination, "0.1.0", &providers, 1, &checksum);
        let error = publish(&request, &available(), "9.9.9").unwrap_err();
        assert!(error.to_string().contains("checksum"), "{error}");
        assert!(!destination.exists());
    }

    #[test]
    fn entry_count_mismatch_refuses() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("catalog.json");
        let destination = temp.path().join("published.json");
        let (body, checksum) = document(&body("0.1.0", &["openai"], 2));
        std::fs::write(&source, &body).unwrap();
        let providers = vec!["openai".to_owned()];
        let request = request(&source, &destination, "0.1.0", &providers, 3, &checksum);
        let error = publish(&request, &available(), "9.9.9").unwrap_err();
        assert!(error.to_string().contains("entry-count"), "{error}");
        assert!(!destination.exists());
    }

    #[test]
    fn minimum_client_version_gate_refuses_an_older_client() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("catalog.json");
        let destination = temp.path().join("published.json");
        let (body, checksum) = document(&body("99.0.0", &["openai"], 1));
        std::fs::write(&source, &body).unwrap();
        let providers = vec!["openai".to_owned()];
        let request = request(&source, &destination, "99.0.0", &providers, 1, &checksum);
        let error = publish(&request, &available(), "0.0.1").unwrap_err();
        assert!(
            error.to_string().contains("minimum-client-version"),
            "{error}"
        );
        assert!(!destination.exists());
    }

    #[test]
    fn required_provider_gate_refuses_an_unavailable_provider() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("catalog.json");
        let destination = temp.path().join("published.json");
        let (body, checksum) = document(&body("0.1.0", &["missing-provider"], 1));
        std::fs::write(&source, &body).unwrap();
        let providers = vec!["missing-provider".to_owned()];
        let request = request(&source, &destination, "0.1.0", &providers, 1, &checksum);
        let error = publish(&request, &available(), "9.9.9").unwrap_err();
        assert!(error.to_string().contains("required-provider"), "{error}");
        assert!(!destination.exists());
    }

    #[test]
    fn immutable_path_gate_keeps_the_previous_catalog_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("catalog.json");
        let destination = temp.path().join("published.json");
        std::fs::write(&destination, "previous catalog").unwrap();
        let (body, checksum) = document(&body("0.1.0", &["openai"], 1));
        std::fs::write(&source, &body).unwrap();
        let providers = vec!["openai".to_owned()];
        let request = request(&source, &destination, "0.1.0", &providers, 1, &checksum);
        let error = publish(&request, &available(), "9.9.9").unwrap_err();
        assert!(error.to_string().contains("immutable-path"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&destination).unwrap(),
            "previous catalog"
        );
    }

    #[test]
    fn schema_gate_refuses_an_unknown_document_schema() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("catalog.json");
        let destination = temp.path().join("published.json");
        let body = body("0.1.0", &["openai"], 1).replace(CATALOG_SCHEMA, "octet-catalog-999");
        let checksum = format!("{:x}", Sha256::digest(body.as_bytes()));
        std::fs::write(&source, &body).unwrap();
        let providers = vec!["openai".to_owned()];
        let request = request(&source, &destination, "0.1.0", &providers, 1, &checksum);
        let error = publish(&request, &available(), "9.9.9").unwrap_err();
        assert!(error.to_string().contains("schema"), "{error}");
        assert!(!destination.exists());
    }
}
