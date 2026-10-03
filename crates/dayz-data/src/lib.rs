//! Reader and resolver for the `dayz-data` database.
//!
//! The database answers one question: where, in this particular build of the game, is the
//! thing called `render.frame`? It has two halves.
//!
//! - **Patterns** ([`PatternFile`]) are the source of truth: a byte signature per symbol,
//!   independent of any build. An unknown build can be resolved by scanning them.
//! - **Build files** ([`BuildFile`]) are a cache: addresses already resolved for one
//!   executable, keyed by its SHA-256, each with a byte check that invalidates a stale entry
//!   instead of letting it point somewhere wrong.
//!
//! Resolution ([`SymbolTable::resolve`]) takes the mapped image, prefers the cache, falls
//! back to scanning, verifies everything it produces, and reports per symbol where the
//! answer came from. Nothing here is Windows specific: the image is a byte slice, so the
//! whole thing is testable on any host.

mod db;
mod hex;
mod pattern;
mod resolve;
mod schema;
mod table;

pub use db::{sha256_file, Database, DatabaseError, Identity, MatchedBy};
pub use pattern::{Pattern, PatternError};
pub use resolve::{Resolve, ResolveError};
pub use schema::{
    BuildFile, BuildInfo, OffsetEntry, PatternEntry, PatternFile, Provenance, SymbolEntry,
    SymbolKind,
};
pub use table::{Issue, Origin, Resolved, SymbolTable};

/// Schema version this crate reads and writes.
pub const SCHEMA_VERSION: u32 = 1;

/// Directory name the loader looks for inside the game folder.
pub const DIRECTORY_NAME: &str = "data";

/// File name of the pattern file within a database directory.
pub const PATTERNS_FILE: &str = "patterns.json";

/// Subdirectory holding one file per known build.
pub const BUILDS_DIR: &str = "builds";
