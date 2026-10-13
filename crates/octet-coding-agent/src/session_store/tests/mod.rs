//! The `SessionStore` test suite, grouped by the boundary under test rather
//! than by the function that happens to be called: accounting, workspace
//! mapping, metadata and deletion, discovery, the search projection, catalog
//! repair, the lightweight transcript summary, listing and inspection, and the
//! delegation confinement boundary.
//!
//! The groups are separate files because the suite is large enough that one
//! file would be its own monolith, and because each group needs a `//!` header
//! saying what property it is responsible for - which is the part a reader
//! actually wants and which a single shared file cannot give.

use super::*;

mod catalog_repair;
mod delegation;
mod discovery;
mod ephemeral_accounting;
mod lightweight_summary;
mod listing_and_inspection;
mod metadata_and_deletion;
mod search_projection;
mod workspace_and_paths;
