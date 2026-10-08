//! Schema bounds: every number a v1 reader enforces, named once.
//!
//! These constants are the crate's fail-closed surface. A value that is not
//! bounded by one of them is a value this crate will accept without a
//! documented limit, so they are kept apart from the types that *apply* them:
//! a reader cites a bound, it never repeats the number. Grouping them here also
//! makes the total budget of one document reviewable in a single screen — the
//! per-collection caps and the aggregate caps are one policy, and splitting them
//! across the readers that enforce them would let the two drift.
//!
//! Everything in this module is re-exported from the crate root, so the public
//! path of every constant is unchanged.

/// The only `MigratedSetup` schema version supported by this crate release.
pub const MIGRATED_SETUP_SCHEMA_VERSION: u32 = 1;

/// The only comparison-report schema version supported by this crate release.
pub const COMPARE_REPORT_SCHEMA_VERSION: u32 = 1;

/// Maximum raw UTF-8 JSON input size accepted by any `from_json` entry point.
pub const MAX_JSON_INPUT_BYTES: usize = 1_048_576;

/// Maximum object/array nesting depth accepted by raw JSON entry points.
///
/// A root object or array has depth one. Scalars do not increase depth.
pub const MAX_JSON_NESTING: usize = 32;

/// Maximum UTF-8 byte length of one decoded JSON string or typed string input.
pub const MAX_STRING_BYTES: usize = 16_384;

/// Maximum members in any JSON object, including objects not recognized by v1.
pub const MAX_MAP_ENTRIES: usize = 128;

/// Maximum elements in any JSON array, including arrays not recognized by v1.
pub const MAX_LIST_ENTRIES: usize = 128;

/// Maximum total decoded UTF-8 string bytes inspected during raw JSON preflight.
///
/// This includes JSON object names, including fixed wire field names, so it
/// bounds hostile unknown material before strict schema validation.
pub const MAX_TOTAL_JSON_STRING_BYTES: usize = 131_072;

/// Maximum total object members and array elements inspected during raw JSON
/// preflight.
pub const MAX_TOTAL_JSON_ENTRIES: usize = 16_384;

/// Maximum decoded payload-string bytes in one validated schema value.
///
/// This counts dynamic payload strings (for example, metadata names and values,
/// source paths, task names, and skill content), but not fixed wire field names.
pub const MAX_TOTAL_DECODED_STRING_BYTES: usize = 65_536;

/// Maximum aggregate source-item records in one validated schema value.
///
/// For a migrated setup, this is the sum of category outcomes and setup-level
/// diagnostics. An unmapped outcome already counts as its source-item record;
/// its nested diagnostic does not count a second time. For a comparison report,
/// this is the number of task rows.
pub const MAX_TOTAL_DECODED_RECORDS: usize = 256;

/// Maximum aggregate dynamic collection entries in one validated schema value.
///
/// This counts category outcomes, setup diagnostics, task rows, metadata-map
/// members, and stdio arguments, but excludes fixed schema-field members.
pub const MAX_TOTAL_DECODED_COLLECTION_ENTRIES: usize = MAX_TOTAL_JSON_ENTRIES;

/// Maximum model outcomes in a migrated setup.
pub const MAX_MODELS: usize = MAX_LIST_ENTRIES;

/// Maximum skill outcomes in a migrated setup.
pub const MAX_SKILLS: usize = MAX_LIST_ENTRIES;

/// Maximum MCP-server outcomes in a migrated setup.
pub const MAX_MCP_SERVERS: usize = MAX_LIST_ENTRIES;

/// Maximum permission outcomes in a migrated setup.
pub const MAX_PERMISSIONS: usize = MAX_LIST_ENTRIES;

/// Maximum diagnostics in a migrated setup, including nested unmapped ones.
pub const MAX_DIAGNOSTICS: usize = MAX_LIST_ENTRIES;

/// Maximum command arguments in a stdio MCP transport.
pub const MAX_MCP_ARGUMENTS: usize = MAX_LIST_ENTRIES;

/// Maximum task rows in a comparison report.
pub const MAX_TASKS: usize = MAX_LIST_ENTRIES;

/// Largest exactly portable JSON integer: `(1 << 53) - 1`.
///
/// JavaScript and many JSON consumers store numbers in IEEE-754 binary64. Every
/// integer through this bound is represented exactly in those consumers, while
/// larger `u64` values can silently round. All four comparison task metrics use
/// this domain instead of accepting arbitrary Rust `u64` values.
pub const MAX_PORTABLE_JSON_INTEGER: u64 = (1_u64 << 53) - 1;

/// Maximum bytes emitted by [`crate::CompareReport::to_markdown`].
///
/// The decoded-string limit keeps valid input well below this output limit even
/// when every rendered character needs escaping.
pub const MAX_RENDERED_MARKDOWN_BYTES: usize = MAX_JSON_INPUT_BYTES;

/// Explicit normalized path denoting a setup-level diagnostic.
///
/// Any other diagnostic path is a trimmed source-relative path with nonempty
/// slash-separated segments, no `.` or `..` segment, no backslash, no C0/C1,
/// line/paragraph separator, or bidirectional control, and no Unicode
/// `Default_Ignorable_Code_Point` scalar. The v1 check is scalar-based and does
/// not normalize or rewrite the path.
pub const ROOT_DIAGNOSTIC_PATH: &str = "$";
