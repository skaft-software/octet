//! The checked public types of a migrated setup: models, skills, MCP servers,
//! permissions, and the setup that holds one outcome per source item.
//!
//! Every field here is private and every value arrives through a constructor
//! that re-applies the crate's bounds, so a `MigratedSetup` in memory is
//! indistinguishable from one that just passed the wire preflight. This module
//! owns that checked shape only; the wire structs it is decoded from live in
//! [`crate::wire`], the bounds it enforces are named in [`crate::limits`], and
//! the aggregate accounting that stops a document from exceeding its total
//! budget lives in [`crate::validate`]. Keeping those three apart is what lets
//! a reader be added or a bound be tightened without touching the checked types
//! their callers already hold.

use serde::ser::SerializeMap;
use serde::Serialize;

use crate::compare::canonical_json;
use crate::diagnostic::Diagnostic;
use crate::error::{Result, ValidationError};
use crate::json_bounds::{decode_raw, ensure_supported_version, preflight_json};
use crate::limits::{
    MAX_DIAGNOSTICS, MAX_MCP_ARGUMENTS, MAX_MCP_SERVERS, MAX_MODELS, MAX_PERMISSIONS, MAX_SKILLS,
    MIGRATED_SETUP_SCHEMA_VERSION,
};
use crate::validate::{
    ensure_can_append, validate_collection_count, validate_mcp_server_outcome,
    validate_model_outcome, validate_permission_outcome, validate_skill_outcome,
    validate_source_path, ResourceUsage,
};
use crate::version::probe_setup_version;
use crate::wire::RawMigratedSetup;

/// The explicit outcome for one source item in a migrated setup.
///
/// Each source item belongs in one category list as either a successful mapping
/// or an unmapped diagnostic. Consumers therefore cannot mistake an omitted item
/// for a mapped one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationOutcome<T> {
    pub(super) inner: MigrationOutcomeInner<T>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum MigrationOutcomeInner<T> {
    Mapped { path: String, value: T },
    Unmapped { diagnostic: Diagnostic },
}

impl<T> MigrationOutcome<T> {
    /// Returns the mapped source path and value, if this outcome was mapped.
    pub fn as_mapped(&self) -> Option<(&str, &T)> {
        match &self.inner {
            MigrationOutcomeInner::Mapped { path, value } => Some((path, value)),
            MigrationOutcomeInner::Unmapped { .. } => None,
        }
    }

    /// Returns the unmapped diagnostic, if this outcome was not mapped.
    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        match &self.inner {
            MigrationOutcomeInner::Mapped { .. } => None,
            MigrationOutcomeInner::Unmapped { diagnostic } => Some(diagnostic),
        }
    }

    /// Returns whether this is a successful mapping.
    pub const fn is_mapped(&self) -> bool {
        matches!(self.inner, MigrationOutcomeInner::Mapped { .. })
    }

    /// Returns whether this is an explicit unmapped diagnostic.
    pub const fn is_unmapped(&self) -> bool {
        matches!(self.inner, MigrationOutcomeInner::Unmapped { .. })
    }

    /// Creates a bounded mapped outcome.
    ///
    /// The mapped source path is bounded here. A [`MigratedSetup`] performs the
    /// concrete target and aggregate validation when it accepts the outcome.
    pub fn mapped(path: impl Into<String>, value: T) -> Result<Self> {
        let path = path.into();
        let mut usage = ResourceUsage::default();
        validate_source_path(&path, &mut usage)?;
        Ok(Self::mapped_unchecked(path, value))
    }

    /// Creates a checked unmapped outcome.
    pub fn unmapped(diagnostic: Diagnostic) -> Result<Self> {
        let mut usage = ResourceUsage::default();
        diagnostic.validate_with_usage(&mut usage)?;
        Ok(Self::unmapped_unchecked(diagnostic))
    }

    pub(super) fn mapped_unchecked(path: String, value: T) -> Self {
        Self {
            inner: MigrationOutcomeInner::Mapped { path, value },
        }
    }

    pub(super) fn unmapped_unchecked(diagnostic: Diagnostic) -> Self {
        Self {
            inner: MigrationOutcomeInner::Unmapped { diagnostic },
        }
    }
}

impl<T> Serialize for MigrationOutcome<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.inner {
            MigrationOutcomeInner::Mapped { path, value } => {
                let mut map = serializer.serialize_map(Some(3))?;
                map.serialize_entry("outcome", "mapped")?;
                map.serialize_entry("path", path)?;
                map.serialize_entry("value", value)?;
                map.end()
            }
            MigrationOutcomeInner::Unmapped { diagnostic } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("outcome", "unmapped")?;
                map.serialize_entry("diagnostic", diagnostic)?;
                map.end()
            }
        }
    }
}

/// A model selection that can be imported without provider credentials.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Model {
    pub(super) provider: String,
    pub(super) model: String,
}

impl Model {
    /// Creates a bounded source-neutral provider and model selection.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        let model = Self {
            provider: provider.into(),
            model: model.into(),
        };
        let mut usage = ResourceUsage::default();
        model.validate_with_usage(&mut usage)?;
        Ok(model)
    }

    /// Returns the source-neutral provider identifier.
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Returns the provider's model identifier.
    pub fn model(&self) -> &str {
        &self.model
    }

    pub(super) fn validate_with_usage(&self, usage: &mut ResourceUsage) -> Result<()> {
        usage.take_string("model provider", &self.provider)?;
        usage.take_string("model identifier", &self.model)
    }
}

/// A portable skill's target name and Markdown instructions.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Skill {
    pub(super) name: String,
    pub(super) content: String,
}

impl Skill {
    /// Creates a bounded skill name and Markdown content payload.
    pub fn new(name: impl Into<String>, content: impl Into<String>) -> Result<Self> {
        let skill = Self {
            name: name.into(),
            content: content.into(),
        };
        let mut usage = ResourceUsage::default();
        skill.validate_with_usage(&mut usage)?;
        Ok(skill)
    }

    /// Returns the target skill name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the portable Markdown instruction content.
    pub fn content(&self) -> &str {
        &self.content
    }

    pub(super) fn validate_with_usage(&self, usage: &mut ResourceUsage) -> Result<()> {
        usage.take_string("skill name", &self.name)?;
        usage.take_string("skill content", &self.content)
    }
}

/// The non-secret transport kind used by an MCP server declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum McpTransportKind {
    /// A local stdio command.
    Stdio,
    /// A remote HTTP endpoint.
    Http,
}

/// A non-secret MCP connection transport.
///
/// Stdio declarations are data only and must not be started merely by handling
/// them. HTTP declarations are data only and must not be contacted merely by
/// handling them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpTransport {
    pub(super) inner: McpTransportInner,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum McpTransportInner {
    Stdio { command: String, args: Vec<String> },
    Http { url: String },
}

impl McpTransport {
    /// Creates a bounded local stdio command declaration.
    pub fn stdio(command: impl Into<String>, args: Vec<String>) -> Result<Self> {
        let transport = Self {
            inner: McpTransportInner::Stdio {
                command: command.into(),
                args,
            },
        };
        let mut usage = ResourceUsage::default();
        transport.validate_with_usage(&mut usage)?;
        Ok(transport)
    }

    /// Creates a bounded remote HTTP endpoint declaration.
    pub fn http(url: impl Into<String>) -> Result<Self> {
        let transport = Self {
            inner: McpTransportInner::Http { url: url.into() },
        };
        let mut usage = ResourceUsage::default();
        transport.validate_with_usage(&mut usage)?;
        Ok(transport)
    }

    /// Returns this transport's kind.
    pub const fn kind(&self) -> McpTransportKind {
        match self.inner {
            McpTransportInner::Stdio { .. } => McpTransportKind::Stdio,
            McpTransportInner::Http { .. } => McpTransportKind::Http,
        }
    }

    /// Returns the stdio command when this is a stdio transport.
    pub fn command(&self) -> Option<&str> {
        match &self.inner {
            McpTransportInner::Stdio { command, .. } => Some(command),
            McpTransportInner::Http { .. } => None,
        }
    }

    /// Returns the stdio command arguments when this is a stdio transport.
    pub fn args(&self) -> Option<&[String]> {
        match &self.inner {
            McpTransportInner::Stdio { args, .. } => Some(args),
            McpTransportInner::Http { .. } => None,
        }
    }

    /// Returns the HTTP URL when this is an HTTP transport.
    pub fn url(&self) -> Option<&str> {
        match &self.inner {
            McpTransportInner::Stdio { .. } => None,
            McpTransportInner::Http { url } => Some(url),
        }
    }

    pub(super) fn validate_with_usage(&self, usage: &mut ResourceUsage) -> Result<()> {
        match &self.inner {
            McpTransportInner::Stdio { command, args } => {
                validate_collection_count(args.len(), MAX_MCP_ARGUMENTS, "MCP arguments")?;
                usage.take_string("MCP command", command)?;
                for argument in args {
                    usage.take_collection_entry("MCP argument")?;
                    usage.take_string("MCP argument", argument)?;
                }
                Ok(())
            }
            McpTransportInner::Http { url } => usage.take_string("MCP HTTP URL", url),
        }
    }
}

impl Serialize for McpTransport {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.inner.serialize(serializer)
    }
}

/// An MCP server declaration retained as non-secret data only.
///
/// This schema does not carry environment variables, headers, or credentials.
/// Adapters must report those source fields as unmapped diagnostics instead of
/// serializing secrets into a setup artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct McpServer {
    pub(super) name: String,
    pub(super) transport: McpTransport,
}

impl McpServer {
    /// Creates a bounded named non-secret MCP server declaration.
    pub fn new(name: impl Into<String>, transport: McpTransport) -> Result<Self> {
        let server = Self {
            name: name.into(),
            transport,
        };
        let mut usage = ResourceUsage::default();
        server.validate_with_usage(&mut usage)?;
        Ok(server)
    }

    /// Returns the user-visible server name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the non-secret transport declaration.
    pub fn transport(&self) -> &McpTransport {
        &self.transport
    }

    pub(super) fn validate_with_usage(&self, usage: &mut ResourceUsage) -> Result<()> {
        usage.take_string("MCP server name", &self.name)?;
        self.transport.validate_with_usage(usage)
    }
}

/// A migrated capability decision.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Permission {
    pub(super) capability: String,
    pub(super) decision: PermissionDecision,
}

impl Permission {
    /// Creates a bounded source-neutral capability decision.
    pub fn new(capability: impl Into<String>, decision: PermissionDecision) -> Result<Self> {
        let permission = Self {
            capability: capability.into(),
            decision,
        };
        let mut usage = ResourceUsage::default();
        permission.validate_with_usage(&mut usage)?;
        Ok(permission)
    }

    /// Returns the source-neutral capability name.
    pub fn capability(&self) -> &str {
        &self.capability
    }

    /// Returns the decision that applies to the capability.
    pub const fn decision(&self) -> PermissionDecision {
        self.decision
    }

    pub(super) fn validate_with_usage(&self, usage: &mut ResourceUsage) -> Result<()> {
        usage.take_string("permission capability", &self.capability)
    }
}

/// A source-neutral permission decision.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    /// Allow the capability without another prompt.
    Allow,
    /// Require a user decision when the capability is requested.
    Ask,
    /// Deny the capability.
    Deny,
}

/// A v1 migrated setup envelope.
///
/// Category lists hold [`MigrationOutcome`] values rather than bare target
/// values. This makes an unmapped source item part of the schema instead of an
/// implicit, silently dropped conversion result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MigratedSetup {
    pub(super) schema_version: u32,
    pub(super) source_agent: String,
    pub(super) models: Vec<MigrationOutcome<Model>>,
    pub(super) skills: Vec<MigrationOutcome<Skill>>,
    pub(super) mcp_servers: Vec<MigrationOutcome<McpServer>>,
    pub(super) permissions: Vec<MigrationOutcome<Permission>>,
    pub(super) diagnostics: Vec<Diagnostic>,
}

impl MigratedSetup {
    /// Creates an empty checked v1 envelope for `source_agent`.
    pub fn new(source_agent: impl Into<String>) -> Result<Self> {
        Self::with_parts(
            source_agent,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    /// Creates a checked v1 envelope from its typed category lists.
    pub fn with_parts(
        source_agent: impl Into<String>,
        models: Vec<MigrationOutcome<Model>>,
        skills: Vec<MigrationOutcome<Skill>>,
        mcp_servers: Vec<MigrationOutcome<McpServer>>,
        permissions: Vec<MigrationOutcome<Permission>>,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<Self> {
        let setup = Self {
            schema_version: MIGRATED_SETUP_SCHEMA_VERSION,
            source_agent: source_agent.into(),
            models,
            skills,
            mcp_servers,
            permissions,
            diagnostics,
        };
        setup.validate()?;
        Ok(setup)
    }

    /// Decodes raw v1 JSON without passing through `serde_json::Value`.
    ///
    /// This is the only wire-decoding route for a validated `MigratedSetup`.
    /// It applies raw input, nesting, string, map/list, and aggregate preflight
    /// limits before strict duplicate and unknown-field validation.
    pub fn from_json(json: &str) -> Result<Self> {
        preflight_json(json)?;
        ensure_supported_version(
            probe_setup_version(json)?,
            "MigratedSetup",
            MIGRATED_SETUP_SCHEMA_VERSION,
        )?;
        decode_raw::<RawMigratedSetup>(json)?.into_validated()
    }

    /// Returns this envelope's schema version.
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the source agent or setup format that produced this envelope.
    pub fn source_agent(&self) -> &str {
        &self.source_agent
    }

    /// Returns model-selection outcomes in source precedence order.
    pub fn models(&self) -> &[MigrationOutcome<Model>] {
        &self.models
    }

    /// Returns skill-conversion outcomes in source precedence order.
    pub fn skills(&self) -> &[MigrationOutcome<Skill>] {
        &self.skills
    }

    /// Returns MCP-server outcomes in source precedence order.
    pub fn mcp_servers(&self) -> &[MigrationOutcome<McpServer>] {
        &self.mcp_servers
    }

    /// Returns permission-conversion outcomes in source precedence order.
    pub fn permissions(&self) -> &[MigrationOutcome<Permission>] {
        &self.permissions
    }

    /// Returns setup-level diagnostics in source precedence order.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Appends a checked model outcome after enforcing all aggregate limits.
    pub fn push_model(&mut self, outcome: MigrationOutcome<Model>) -> Result<()> {
        ensure_can_append(self.models.len(), MAX_MODELS, "models")?;
        let mut usage = self.resource_usage()?;
        usage.take_collection_entry("model")?;
        usage.take_record("model")?;
        validate_model_outcome(&outcome, &mut usage)?;
        self.models.push(outcome);
        Ok(())
    }

    /// Appends a checked skill outcome after enforcing all aggregate limits.
    pub fn push_skill(&mut self, outcome: MigrationOutcome<Skill>) -> Result<()> {
        ensure_can_append(self.skills.len(), MAX_SKILLS, "skills")?;
        let mut usage = self.resource_usage()?;
        usage.take_collection_entry("skill")?;
        usage.take_record("skill")?;
        validate_skill_outcome(&outcome, &mut usage)?;
        self.skills.push(outcome);
        Ok(())
    }

    /// Appends a checked MCP-server outcome after enforcing all aggregate limits.
    pub fn push_mcp_server(&mut self, outcome: MigrationOutcome<McpServer>) -> Result<()> {
        ensure_can_append(self.mcp_servers.len(), MAX_MCP_SERVERS, "MCP servers")?;
        let mut usage = self.resource_usage()?;
        usage.take_collection_entry("MCP server")?;
        usage.take_record("MCP server")?;
        validate_mcp_server_outcome(&outcome, &mut usage)?;
        self.mcp_servers.push(outcome);
        Ok(())
    }

    /// Appends a checked permission outcome after enforcing all aggregate limits.
    pub fn push_permission(&mut self, outcome: MigrationOutcome<Permission>) -> Result<()> {
        ensure_can_append(self.permissions.len(), MAX_PERMISSIONS, "permissions")?;
        let mut usage = self.resource_usage()?;
        usage.take_collection_entry("permission")?;
        usage.take_record("permission")?;
        validate_permission_outcome(&outcome, &mut usage)?;
        self.permissions.push(outcome);
        Ok(())
    }

    /// Appends a checked setup-level diagnostic after enforcing all limits.
    pub fn push_diagnostic(&mut self, diagnostic: Diagnostic) -> Result<()> {
        ensure_can_append(self.diagnostics.len(), MAX_DIAGNOSTICS, "diagnostics")?;
        let mut usage = self.resource_usage()?;
        usage.take_collection_entry("diagnostic")?;
        usage.take_record("diagnostic")?;
        diagnostic.validate_with_usage(&mut usage)?;
        self.diagnostics.push(diagnostic);
        Ok(())
    }

    /// Serializes this envelope with fixed field order and a trailing newline.
    ///
    /// Category-list order is preserved because it records source precedence.
    pub fn to_canonical_json(&self) -> Result<String> {
        self.validate()?;
        canonical_json(self)
    }

    pub(super) fn validate(&self) -> Result<()> {
        self.resource_usage().map(|_| ())
    }

    pub(super) fn resource_usage(&self) -> Result<ResourceUsage> {
        if self.schema_version != MIGRATED_SETUP_SCHEMA_VERSION {
            return Err(ValidationError::new("invalid MigratedSetup schema version"));
        }
        validate_collection_count(self.models.len(), MAX_MODELS, "models")?;
        validate_collection_count(self.skills.len(), MAX_SKILLS, "skills")?;
        validate_collection_count(self.mcp_servers.len(), MAX_MCP_SERVERS, "MCP servers")?;
        validate_collection_count(self.permissions.len(), MAX_PERMISSIONS, "permissions")?;
        validate_collection_count(self.diagnostics.len(), MAX_DIAGNOSTICS, "diagnostics")?;

        let mut usage = ResourceUsage::default();
        usage.take_string("source agent", &self.source_agent)?;
        for outcome in &self.models {
            usage.take_collection_entry("model")?;
            usage.take_record("model")?;
            validate_model_outcome(outcome, &mut usage)?;
        }
        for outcome in &self.skills {
            usage.take_collection_entry("skill")?;
            usage.take_record("skill")?;
            validate_skill_outcome(outcome, &mut usage)?;
        }
        for outcome in &self.mcp_servers {
            usage.take_collection_entry("MCP server")?;
            usage.take_record("MCP server")?;
            validate_mcp_server_outcome(outcome, &mut usage)?;
        }
        for outcome in &self.permissions {
            usage.take_collection_entry("permission")?;
            usage.take_record("permission")?;
            validate_permission_outcome(outcome, &mut usage)?;
        }
        for diagnostic in &self.diagnostics {
            usage.take_collection_entry("diagnostic")?;
            usage.take_record("diagnostic")?;
            diagnostic.validate_with_usage(&mut usage)?;
        }
        Ok(usage)
    }
}
