//! The raw wire structs, and the conversions that turn them into checked types.
//!
//! This is the only module that knows a wire field name. A wire struct is
//! `deny_unknown_fields` and uses `BoundedString`/`deserialize_bounded_list`
//! from [`crate::json_bounds`] rather than plain `String`/`Vec`, so the
//! per-field bounds are enforced during deserialization rather than re-checked
//! after it. Each `Raw*` type then has exactly one `into_public`, which is
//! where the aggregate budget is charged; the checked types in
//! [`crate::migrated`] and [`crate::compare`] cannot be built any other way.
//!
//! Splitting this from the checked types is what lets a wire field be renamed
//! or added without touching the API a caller holds, and lets the deny-unknown
//! policy stay uniform across all of them instead of drifting per struct.

use serde::Deserialize;

use crate::compare::{CompareReport, CompareReportHeader, CompareTaskRow};
use crate::diagnostic::{Diagnostic, DiagnosticSeverity};
use crate::error::{Result, SchemaVersionMismatch, ValidationError};
use crate::json_bounds::{
    deserialize_bounded_list, deserialize_peak_rss_bytes, deserialize_tokens_in,
    deserialize_tokens_out, deserialize_wall_clock, BoundedString, RawVersion, UniqueStringMap,
};
use crate::limits::{COMPARE_REPORT_SCHEMA_VERSION, MIGRATED_SETUP_SCHEMA_VERSION};
use crate::migrated::{
    McpServer, McpTransport, McpTransportInner, MigratedSetup, MigrationOutcome, Model, Permission,
    PermissionDecision, Skill,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawMigratedSetup {
    schema_version: RawVersion,
    source_agent: BoundedString,
    #[serde(deserialize_with = "deserialize_bounded_list")]
    models: Vec<RawMigrationOutcome<RawModel>>,
    #[serde(deserialize_with = "deserialize_bounded_list")]
    skills: Vec<RawMigrationOutcome<RawSkill>>,
    #[serde(deserialize_with = "deserialize_bounded_list")]
    mcp_servers: Vec<RawMigrationOutcome<RawMcpServer>>,
    #[serde(deserialize_with = "deserialize_bounded_list")]
    permissions: Vec<RawMigrationOutcome<RawPermission>>,
    #[serde(deserialize_with = "deserialize_bounded_list")]
    diagnostics: Vec<RawDiagnostic>,
}

impl RawMigratedSetup {
    pub(super) fn into_validated(self) -> Result<MigratedSetup> {
        if self.schema_version.0 != u64::from(MIGRATED_SETUP_SCHEMA_VERSION) {
            return Err(ValidationError::new(
                SchemaVersionMismatch {
                    document: "MigratedSetup",
                    found: self.schema_version.0,
                    expected: MIGRATED_SETUP_SCHEMA_VERSION,
                }
                .to_string(),
            ));
        }
        MigratedSetup::with_parts(
            self.source_agent.0,
            self.models
                .into_iter()
                .map(|outcome| outcome.into_public(RawModel::into_public))
                .collect(),
            self.skills
                .into_iter()
                .map(|outcome| outcome.into_public(RawSkill::into_public))
                .collect(),
            self.mcp_servers
                .into_iter()
                .map(|outcome| outcome.into_public(RawMcpServer::into_public))
                .collect(),
            self.permissions
                .into_iter()
                .map(|outcome| outcome.into_public(RawPermission::into_public))
                .collect(),
            self.diagnostics
                .into_iter()
                .map(RawDiagnostic::into_public)
                .collect(),
        )
    }
}

#[derive(Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum RawMigrationOutcome<T> {
    Mapped { path: BoundedString, value: T },
    Unmapped { diagnostic: RawDiagnostic },
}

impl<T> RawMigrationOutcome<T> {
    pub(super) fn into_public<U>(self, map_value: impl FnOnce(T) -> U) -> MigrationOutcome<U> {
        match self {
            Self::Mapped { path, value } => {
                MigrationOutcome::mapped_unchecked(path.0, map_value(value))
            }
            Self::Unmapped { diagnostic } => {
                MigrationOutcome::unmapped_unchecked(diagnostic.into_public())
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawModel {
    provider: BoundedString,
    model: BoundedString,
}

impl RawModel {
    pub(super) fn into_public(self) -> Model {
        Model {
            provider: self.provider.0,
            model: self.model.0,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawSkill {
    name: BoundedString,
    content: BoundedString,
}

impl RawSkill {
    pub(super) fn into_public(self) -> Skill {
        Skill {
            name: self.name.0,
            content: self.content.0,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawMcpServer {
    name: BoundedString,
    transport: RawMcpTransport,
}

impl RawMcpServer {
    pub(super) fn into_public(self) -> McpServer {
        McpServer {
            name: self.name.0,
            transport: self.transport.into_public(),
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum RawMcpTransport {
    Stdio {
        command: BoundedString,
        #[serde(deserialize_with = "deserialize_bounded_list")]
        args: Vec<BoundedString>,
    },
    Http {
        url: BoundedString,
    },
}

impl RawMcpTransport {
    pub(super) fn into_public(self) -> McpTransport {
        match self {
            Self::Stdio { command, args } => McpTransport {
                inner: McpTransportInner::Stdio {
                    command: command.0,
                    args: args.into_iter().map(|argument| argument.0).collect(),
                },
            },
            Self::Http { url } => McpTransport {
                inner: McpTransportInner::Http { url: url.0 },
            },
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawPermission {
    capability: BoundedString,
    decision: RawPermissionDecision,
}

impl RawPermission {
    pub(super) fn into_public(self) -> Permission {
        Permission {
            capability: self.capability.0,
            decision: self.decision.into_public(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RawPermissionDecision {
    Allow,
    Ask,
    Deny,
}

impl RawPermissionDecision {
    pub(super) fn into_public(self) -> PermissionDecision {
        match self {
            Self::Allow => PermissionDecision::Allow,
            Self::Ask => PermissionDecision::Ask,
            Self::Deny => PermissionDecision::Deny,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawDiagnostic {
    path: BoundedString,
    severity: RawDiagnosticSeverity,
    reason: BoundedString,
}

impl RawDiagnostic {
    pub(super) fn into_public(self) -> Diagnostic {
        Diagnostic {
            path: self.path.0,
            severity: self.severity.into_public(),
            reason: self.reason.0,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RawDiagnosticSeverity {
    Warning,
    Error,
}

impl RawDiagnosticSeverity {
    pub(super) fn into_public(self) -> DiagnosticSeverity {
        match self {
            Self::Warning => DiagnosticSeverity::Warning,
            Self::Error => DiagnosticSeverity::Error,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawCompareReportHeader {
    schema_version: RawVersion,
    versions: UniqueStringMap,
    hardware: UniqueStringMap,
}

impl RawCompareReportHeader {
    pub(super) fn into_validated(self) -> Result<CompareReportHeader> {
        if self.schema_version.0 != u64::from(COMPARE_REPORT_SCHEMA_VERSION) {
            return Err(ValidationError::new(
                SchemaVersionMismatch {
                    document: "CompareReportHeader",
                    found: self.schema_version.0,
                    expected: COMPARE_REPORT_SCHEMA_VERSION,
                }
                .to_string(),
            ));
        }
        CompareReportHeader::new(self.versions.0, self.hardware.0)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawCompareTaskRow {
    task_id: BoundedString,
    agent: BoundedString,
    #[serde(deserialize_with = "deserialize_wall_clock")]
    wall_clock: u64,
    #[serde(deserialize_with = "deserialize_peak_rss_bytes")]
    peak_rss_bytes: u64,
    #[serde(deserialize_with = "deserialize_tokens_in")]
    tokens_in: u64,
    #[serde(deserialize_with = "deserialize_tokens_out")]
    tokens_out: u64,
    success: bool,
}

impl RawCompareTaskRow {
    pub(super) fn into_public(self) -> CompareTaskRow {
        CompareTaskRow {
            task_id: self.task_id.0,
            agent: self.agent.0,
            wall_clock: self.wall_clock,
            peak_rss_bytes: self.peak_rss_bytes,
            tokens_in: self.tokens_in,
            tokens_out: self.tokens_out,
            success: self.success,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RawCompareReport {
    header: RawCompareReportHeader,
    #[serde(deserialize_with = "deserialize_bounded_list")]
    tasks: Vec<RawCompareTaskRow>,
}

impl RawCompareReport {
    pub(super) fn into_validated(self) -> Result<CompareReport> {
        CompareReport::new(
            self.header.into_validated()?,
            self.tasks
                .into_iter()
                .map(RawCompareTaskRow::into_public)
                .collect(),
        )
    }
}
