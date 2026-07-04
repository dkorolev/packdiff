//! # packdiff-dto — the data model
//!
//! This crate is the single source of truth for every piece of data packdiff
//! handles. It is **pure logic**: no filesystem, no subprocess, no clock, no
//! randomness — those all live in the callers (`packdiff-cli` natively,
//! `packdiff-wasm` in the browser). That is what lets the *same* compiled
//! semantics run on both sides of the tool.
//!
//! Two document families:
//!
//! - [`diff::DiffDocument`] — the **immutable build artifact**: everything the
//!   CLI extracted from git (refs, commits, per-file hunks and lines). It is
//!   produced once per render and embedded/consumed read-only.
//! - [`review::ReviewDocument`] — the **mutable review state**: the comments a
//!   human leaves on the page. It lives in the browser's localStorage, and
//!   every mutation (upsert, delete, merge/import) goes through this crate via
//!   WASM — the page's JavaScript never edits the document itself.
//!
//! Schema rules:
//!
//! - JSON field names are `snake_case`; union variants are `CamelCase` and
//!   encode as single-key objects — `{ "VariantName": { ...payload } }` — with
//!   no discriminator field.
//! - Every struct strict-rejects unknown fields (`deny_unknown_fields`): a
//!   typo or a drifted producer is a hard, loud error, never a silently
//!   ignored key.
//! - Both documents carry `schema_version` ([`SCHEMA_VERSION`]). This is the
//!   one deliberate opt-in to long-term compatibility (review documents live
//!   in users' localStorage): parsers reject documents from a *newer* schema
//!   than they understand and accept older ones (there is only v1 today, so
//!   "accept" means "equal").
//! - Determinism: same inputs → byte-identical outputs. Ordering is always
//!   explicit ([`review::ReviewDocument::sort`]), timestamps and ids are
//!   caller-supplied, and exports are stable.

pub mod diff;
pub mod export;
pub mod review;

/// Version stamped into (and required of) every document this crate touches.
pub const SCHEMA_VERSION: u32 = 1;

/// Tool identifier stamped into documents.
pub const TOOL: &str = "packdiff";

/// A named ref pinned to the commit it resolved to at build time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefInfo {
  /// What the user asked for: branch, tag, or SHA — kept verbatim so pages
  /// and exports show the name the reviewer thinks in.
  pub name: String,
  /// The full commit SHA the name resolved to; pins the document to an exact
  /// state even if the ref later moves.
  pub sha: String,
}

/// Errors surfaced by any model operation. Typed so callers branch on
/// variants; the WASM ABI ships them as `{ "Error": { "message": ... } }`.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
  /// Input was not valid JSON for the expected shape.
  #[error("invalid JSON: {0}")]
  Json(String),
  /// Document is from a newer schema than this build understands.
  #[error("unsupported schema_version {found} (this build supports up to {supported})")]
  UnsupportedSchema {
    /// The `schema_version` the document claims.
    found: u32,
    /// The newest `schema_version` this build can read.
    supported: u32,
  },
  /// A comment failed validation.
  #[error("invalid comment: {0}")]
  InvalidComment(String),
}

/// The localStorage key a review document is filed under.
///
/// The key pins the exact endpoint SHAs on purpose: line numbers are only
/// meaningful against the diff they were written on. Regenerating the same
/// diff finds the same comments; new commits get a fresh store (carry
/// comments over with export → import, where [`review::ReviewDocument::merge`]
/// applies).
pub fn storage_key(repo: &str, base_sha: &str, head_sha: &str) -> String {
  fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
  }
  format!("packdiff:v{}:{}:{}..{}", SCHEMA_VERSION, repo, short(base_sha), short(head_sha))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn storage_key_pins_repo_and_short_shas() {
    let key =
      storage_key("myrepo", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    assert_eq!(key, "packdiff:v1:myrepo:aaaaaaaaaaaa..bbbbbbbbbbbb");
  }

  #[test]
  fn storage_key_tolerates_short_shas() {
    assert_eq!(storage_key("r", "abc", "def"), "packdiff:v1:r:abc..def");
  }
}
