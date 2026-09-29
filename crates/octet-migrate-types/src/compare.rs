//! The comparison report: header metadata, one row per task, and the JSON-to-
//! Markdown boundary.
//!
//! Markdown is *derived* here and nowhere else. There is no separately
//! deserialized Markdown report: a report is decoded once, through the same
//! validated path as a migrated setup, and its Markdown view is rendered from
//! the checked value. That is why the renderer and the report types share a
//! module — the guarantee that a rendered cell can only contain characters the
//! checked value already admitted is a property of the pair, and it belongs with
//! the escaping rules in [`crate::markdown`].

use std::collections::BTreeMap;

use serde::Serialize;

use crate::error::{Result, ValidationError};
use crate::json_bounds::{decode_raw, ensure_supported_version, preflight_json};
use crate::limits::{
    COMPARE_REPORT_SCHEMA_VERSION, MAX_JSON_INPUT_BYTES, MAX_RENDERED_MARKDOWN_BYTES, MAX_TASKS,
};
use crate::markdown::markdown_cell;
use crate::validate::{
    ensure_can_append, validate_collection_count, validate_metadata_map, validate_portable_metric,
    ResourceUsage,
};
use crate::version::{probe_header_version, probe_report_version};
use crate::wire::{RawCompareReport, RawCompareReportHeader};

/// Header metadata for a [`CompareReport`].
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompareReportHeader {
    pub(super) schema_version: u32,
    pub(super) versions: BTreeMap<String, String>,
    pub(super) hardware: BTreeMap<String, String>,
}

impl CompareReportHeader {
    /// Creates a checked v1 comparison header from deterministic key-value maps.
    pub fn new(
        versions: BTreeMap<String, String>,
        hardware: BTreeMap<String, String>,
    ) -> Result<Self> {
        let header = Self {
            schema_version: COMPARE_REPORT_SCHEMA_VERSION,
            versions,
            hardware,
        };
        header.validate()?;
        Ok(header)
    }

    /// Decodes raw v1 header JSON without passing through `serde_json::Value`.
    ///
    /// This is the only wire-decoding route for a validated
    /// `CompareReportHeader`.
    pub fn from_json(json: &str) -> Result<Self> {
        preflight_json(json)?;
        ensure_supported_version(
            probe_header_version(json)?,
            "CompareReportHeader",
            COMPARE_REPORT_SCHEMA_VERSION,
        )?;
        decode_raw::<RawCompareReportHeader>(json)?.into_validated()
    }

    /// Returns this header's schema version.
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns exact component versions in deterministic key order.
    pub fn versions(&self) -> &BTreeMap<String, String> {
        &self.versions
    }

    /// Returns normalized hardware facts in deterministic key order.
    pub fn hardware(&self) -> &BTreeMap<String, String> {
        &self.hardware
    }

    /// Serializes this header with fixed field and map order and a trailing newline.
    pub fn to_canonical_json(&self) -> Result<String> {
        self.validate()?;
        canonical_json(self)
    }

    pub(super) fn validate(&self) -> Result<()> {
        self.validate_with_usage(&mut ResourceUsage::default())
    }

    pub(super) fn validate_with_usage(&self, usage: &mut ResourceUsage) -> Result<()> {
        if self.schema_version != COMPARE_REPORT_SCHEMA_VERSION {
            return Err(ValidationError::new(
                "invalid CompareReportHeader schema version",
            ));
        }
        validate_metadata_map(&self.versions, "versions", usage)?;
        validate_metadata_map(&self.hardware, "hardware", usage)
    }
}

/// One task result in a paired comparison report.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct CompareTaskRow {
    pub(super) task_id: String,
    pub(super) agent: String,
    pub(super) wall_clock: u64,
    pub(super) peak_rss_bytes: u64,
    pub(super) tokens_in: u64,
    pub(super) tokens_out: u64,
    pub(super) success: bool,
}

impl CompareTaskRow {
    /// Creates a checked task row with exact portable JSON integer metrics.
    pub fn new(
        task_id: impl Into<String>,
        agent: impl Into<String>,
        wall_clock: u64,
        peak_rss_bytes: u64,
        tokens_in: u64,
        tokens_out: u64,
        success: bool,
    ) -> Result<Self> {
        let row = Self {
            task_id: task_id.into(),
            agent: agent.into(),
            wall_clock,
            peak_rss_bytes,
            tokens_in,
            tokens_out,
            success,
        };
        row.validate_with_usage(&mut ResourceUsage::default())?;
        Ok(row)
    }

    /// Returns the stable task identifier shared across compared agents.
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// Returns the agent that produced this result.
    pub fn agent(&self) -> &str {
        &self.agent
    }

    /// Returns elapsed wall-clock time in milliseconds.
    pub const fn wall_clock(&self) -> u64 {
        self.wall_clock
    }

    /// Returns peak resident-set size in bytes.
    pub const fn peak_rss_bytes(&self) -> u64 {
        self.peak_rss_bytes
    }

    /// Returns input tokens reported for the task.
    pub const fn tokens_in(&self) -> u64 {
        self.tokens_in
    }

    /// Returns output tokens reported for the task.
    pub const fn tokens_out(&self) -> u64 {
        self.tokens_out
    }

    /// Returns whether the task's authoritative success check passed.
    pub const fn success(&self) -> bool {
        self.success
    }

    pub(super) fn validate_with_usage(&self, usage: &mut ResourceUsage) -> Result<()> {
        usage.take_string("task identifier", &self.task_id)?;
        usage.take_string("task agent", &self.agent)?;
        validate_portable_metric(self.wall_clock, "wall_clock")?;
        validate_portable_metric(self.peak_rss_bytes, "peak_rss_bytes")?;
        validate_portable_metric(self.tokens_in, "tokens_in")?;
        validate_portable_metric(self.tokens_out, "tokens_out")
    }
}

/// A JSON-first paired comparison report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CompareReport {
    pub(super) header: CompareReportHeader,
    pub(super) tasks: Vec<CompareTaskRow>,
}

impl CompareReport {
    /// Creates a checked comparison report from a v1 header and task rows.
    pub fn new(header: CompareReportHeader, tasks: Vec<CompareTaskRow>) -> Result<Self> {
        let report = Self { header, tasks };
        report.validate()?;
        Ok(report)
    }

    /// Decodes raw v1 report JSON without passing through `serde_json::Value`.
    ///
    /// This is the only wire-decoding route for a validated `CompareReport`.
    pub fn from_json(json: &str) -> Result<Self> {
        preflight_json(json)?;
        ensure_supported_version(
            probe_report_version(json)?,
            "CompareReport",
            COMPARE_REPORT_SCHEMA_VERSION,
        )?;
        decode_raw::<RawCompareReport>(json)?.into_validated()
    }

    /// Returns the version and metadata header.
    pub fn header(&self) -> &CompareReportHeader {
        &self.header
    }

    /// Returns task rows in their supplied source order.
    pub fn tasks(&self) -> &[CompareTaskRow] {
        &self.tasks
    }

    /// Appends a checked task row after enforcing all aggregate limits.
    pub fn push_task(&mut self, task: CompareTaskRow) -> Result<()> {
        ensure_can_append(self.tasks.len(), MAX_TASKS, "tasks")?;
        let mut usage = self.resource_usage()?;
        usage.take_collection_entry("task")?;
        usage.take_record("task")?;
        task.validate_with_usage(&mut usage)?;
        self.tasks.push(task);
        Ok(())
    }

    /// Serializes a deterministic JSON report with a trailing newline.
    ///
    /// Version and hardware maps are [`BTreeMap`]s. Task ordering is represented
    /// by a sorted vector of references, so this does not clone the report or
    /// task rows merely to render canonical JSON.
    pub fn to_canonical_json(&self) -> Result<String> {
        self.validate()?;
        let canonical = CanonicalCompareReport {
            header: &self.header,
            tasks: self.sorted_task_refs(),
        };
        canonical_json(&canonical)
    }

    /// Renders a human-readable Markdown view of this validated JSON schema.
    ///
    /// Rows use the same ordering as [`Self::to_canonical_json`]. Every
    /// report-provided string is rendered as literal text: HTML and Markdown
    /// syntax are escaped, and control or bidirectional formatting characters
    /// are shown as visible Unicode escapes. Newlines use trusted `<br>` markup.
    /// Sorting uses references and never clones report task rows.
    pub fn to_markdown(&self) -> Result<String> {
        self.validate()?;
        let tasks = self.sorted_task_refs();
        let mut markdown = String::from("# Compare report\n\n");
        markdown.push_str(&format!(
            "Schema version: {}\n\n",
            self.header.schema_version()
        ));

        markdown.push_str("## Versions\n\n| Component | Version |\n| --- | --- |\n");
        for (component, version) in self.header.versions() {
            markdown.push_str(&format!(
                "| {} | {} |\n",
                markdown_cell(component),
                markdown_cell(version)
            ));
        }

        markdown.push_str("\n## Hardware\n\n| Property | Value |\n| --- | --- |\n");
        for (property, value) in self.header.hardware() {
            markdown.push_str(&format!(
                "| {} | {} |\n",
                markdown_cell(property),
                markdown_cell(value)
            ));
        }

        markdown.push_str(
            "\n## Tasks\n\n| Task ID | Agent | Wall clock (ms) | Peak RSS (bytes) | Tokens in | Tokens out | Success |\n| --- | --- | ---: | ---: | ---: | ---: | --- |\n",
        );
        for task in tasks {
            markdown.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} |\n",
                markdown_cell(task.task_id()),
                markdown_cell(task.agent()),
                task.wall_clock(),
                task.peak_rss_bytes(),
                task.tokens_in(),
                task.tokens_out(),
                task.success(),
            ));
        }

        if markdown.len() > MAX_RENDERED_MARKDOWN_BYTES {
            return Err(ValidationError::new(format!(
                "rendered Markdown exceeds MAX_RENDERED_MARKDOWN_BYTES ({MAX_RENDERED_MARKDOWN_BYTES} bytes)"
            )));
        }
        Ok(markdown)
    }

    pub(super) fn sorted_task_refs(&self) -> Vec<&CompareTaskRow> {
        let mut tasks = self.tasks.iter().collect::<Vec<_>>();
        tasks.sort_unstable();
        tasks
    }

    pub(super) fn validate(&self) -> Result<()> {
        self.resource_usage().map(|_| ())
    }

    pub(super) fn resource_usage(&self) -> Result<ResourceUsage> {
        validate_collection_count(self.tasks.len(), MAX_TASKS, "tasks")?;
        let mut usage = ResourceUsage::default();
        self.header.validate_with_usage(&mut usage)?;
        for task in &self.tasks {
            usage.take_collection_entry("task")?;
            usage.take_record("task")?;
            task.validate_with_usage(&mut usage)?;
        }
        Ok(usage)
    }
}

#[derive(Serialize)]
pub(super) struct CanonicalCompareReport<'a> {
    pub(super) header: &'a CompareReportHeader,
    pub(super) tasks: Vec<&'a CompareTaskRow>,
}

/// Decodes a canonical comparison JSON report and renders its Markdown view.
///
/// This is the JSON-to-Markdown boundary: there is no separately deserialized
/// Markdown report schema.
pub fn render_compare_report_markdown(json: &str) -> Result<String> {
    CompareReport::from_json(json)?.to_markdown()
}

pub(super) fn canonical_json<T>(value: &T) -> Result<String>
where
    T: Serialize,
{
    let mut json = serde_json::to_string_pretty(value).map_err(ValidationError::from_serde)?;
    let output_bytes = json
        .len()
        .checked_add(1)
        .ok_or_else(|| ValidationError::new("canonical JSON byte counter overflow"))?;
    if output_bytes > MAX_JSON_INPUT_BYTES {
        return Err(ValidationError::new(format!(
            "canonical JSON exceeds MAX_JSON_INPUT_BYTES ({MAX_JSON_INPUT_BYTES} bytes)"
        )));
    }
    json.push('\n');
    Ok(json)
}
