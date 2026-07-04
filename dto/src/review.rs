//! The review document: the comments a reviewer leaves on a diff, and every
//! operation that may change them.
//!
//! This is the state that lives in the browser's localStorage. The page's
//! JavaScript never edits it directly — it calls into the WASM build of this
//! crate for every mutation, so upsert/delete/merge/ordering semantics are
//! defined exactly once, here, and are identical in tests, in the CLI, and in
//! the browser.
//!
//! Purity contract: the model has no clock and no entropy. Comment `id`s and
//! RFC 3339 timestamps are supplied by the caller; the model validates and
//! orders them.

use serde::{Deserialize, Serialize};

use crate::{ModelError, RefInfo, SCHEMA_VERSION, TOOL};

/// Which side of the diff a comment anchors to. Serialized as the bare
/// `CamelCase` variant name (`"Old"` / `"New"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Side {
  /// Pre-image (a deleted line): `line` is an old-file line number.
  Old,
  /// Post-image (an added or context line): `line` is a new-file line number.
  New,
}

/// One review comment, anchored to a diff line.
///
/// Anchor identity is `(file, side, line)` where `file` is the post-image
/// path ([`crate::diff::FileDiff::anchor_path`]). Comment identity is `id` —
/// anchors may collide (several comments on one line), ids may not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comment {
  /// Caller-supplied unique id — the comment's identity for upsert, delete,
  /// and import merging.
  pub id: String,
  /// Post-image path of the file the comment anchors to.
  pub file: String,
  /// Which side of the diff `line` counts in.
  pub side: Side,
  /// 1-based line number on `side`.
  pub line: u32,
  /// The comment body, verbatim; may span multiple lines.
  pub text: String,
  /// Creation time, RFC 3339 UTC, caller-supplied; part of the stable sort
  /// order so threads on one line read chronologically.
  pub created_at: String,
  /// Last-edit time, RFC 3339 UTC, caller-supplied; drives merge conflict
  /// resolution (newer wins).
  pub updated_at: String,
}

impl Comment {
  /// Validation applied on every entry into the document.
  pub fn validate(&self) -> Result<(), ModelError> {
    fn need(cond: bool, msg: &str) -> Result<(), ModelError> {
      if cond {
        Ok(())
      } else {
        Err(ModelError::InvalidComment(msg.to_string()))
      }
    }
    need(!self.id.trim().is_empty(), "id must be non-empty")?;
    need(!self.file.trim().is_empty(), "file must be non-empty")?;
    need(self.line >= 1, "line must be >= 1")?;
    need(!self.text.trim().is_empty(), "text must be non-empty")?;
    need(!self.created_at.trim().is_empty(), "created_at must be non-empty")?;
    need(!self.updated_at.trim().is_empty(), "updated_at must be non-empty")?;
    Ok(())
  }

  /// Deterministic document order: by file, then line, then side (old
  /// before new), then creation time, then id as the final tiebreaker.
  fn sort_key(&self) -> (&str, u32, Side, &str, &str) {
    (&self.file, self.line, self.side, &self.created_at, &self.id)
  }
}

/// The mutable review state for one `(repo, base_sha, head_sha)` diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewDocument {
  /// Schema generation this document was written with; readers reject
  /// documents newer than they understand ([`SCHEMA_VERSION`]).
  pub schema_version: u32,
  /// Producing tool identifier (`"packdiff"`); distinguishes exported files
  /// from look-alike JSON of other origins.
  pub tool: String,
  /// Repository directory name the review belongs to.
  pub repo: String,
  /// The reviewed diff's base ref, pinned to its SHA.
  pub base: RefInfo,
  /// The reviewed diff's head ref, pinned to its SHA.
  pub head: RefInfo,
  /// The comments, always in the canonical order ([`ReviewDocument::sort`]).
  pub comments: Vec<Comment>,
}

impl ReviewDocument {
  pub fn new(repo: String, base: RefInfo, head: RefInfo) -> Self {
    Self { schema_version: SCHEMA_VERSION, tool: TOOL.to_string(), repo, base, head, comments: Vec::new() }
  }

  /// Parse and validate a document. Rejects documents from a newer schema
  /// and any unknown field (strict deserialization); re-validates and
  /// re-sorts every comment so untrusted input (an imported file, a
  /// hand-edited store) is normalized on the way in.
  pub fn parse(json: &str) -> Result<Self, ModelError> {
    let mut doc: ReviewDocument = serde_json::from_str(json).map_err(|e| ModelError::Json(e.to_string()))?;
    if doc.schema_version > SCHEMA_VERSION {
      return Err(ModelError::UnsupportedSchema { found: doc.schema_version, supported: SCHEMA_VERSION });
    }
    for c in &doc.comments {
      c.validate()?;
    }
    doc.sort();
    Ok(doc)
  }

  /// Canonical serialized form (pretty, trailing newline) — what exports
  /// and the localStorage round-trip use.
  pub fn to_json_pretty(&self) -> String {
    let mut s = serde_json::to_string_pretty(self).expect("document serializes: no non-string keys, no fallible types");
    s.push('\n');
    s
  }

  /// Insert a new comment or replace the one with the same `id`.
  pub fn upsert(&mut self, comment: Comment) -> Result<(), ModelError> {
    comment.validate()?;
    match self.comments.iter_mut().find(|c| c.id == comment.id) {
      Some(existing) => *existing = comment,
      None => self.comments.push(comment),
    }
    self.sort();
    Ok(())
  }

  /// Remove a comment by id. Returns whether anything was removed.
  pub fn delete(&mut self, id: &str) -> bool {
    let before = self.comments.len();
    self.comments.retain(|c| c.id != id);
    let removed = self.comments.len() != before;
    if removed {
      self.sort();
    }
    removed
  }

  /// Merge another document's comments into this one (the import path).
  ///
  /// Semantics: union by `id`; on an id collision the comment with the
  /// **later `updated_at`** wins (RFC 3339 UTC strings compare correctly
  /// lexicographically); an exact tie keeps the existing comment. Metadata
  /// (repo/refs) of `self` is kept — importing does not re-target a store.
  pub fn merge(&mut self, incoming: &ReviewDocument) {
    for inc in &incoming.comments {
      match self.comments.iter_mut().find(|c| c.id == inc.id) {
        Some(existing) => {
          if inc.updated_at > existing.updated_at {
            *existing = inc.clone();
          }
        }
        None => self.comments.push(inc.clone()),
      }
    }
    self.sort();
  }

  /// Restore the canonical deterministic order.
  pub fn sort(&mut self) {
    self.comments.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn refinfo(name: &str, sha_char: char) -> RefInfo {
    RefInfo { name: name.into(), sha: std::iter::repeat(sha_char).take(40).collect() }
  }

  fn doc() -> ReviewDocument {
    ReviewDocument::new("repo".into(), refinfo("main", 'a'), refinfo("feat", 'b'))
  }

  fn comment(id: &str, file: &str, line: u32, updated: &str) -> Comment {
    Comment {
      id: id.into(),
      file: file.into(),
      side: Side::New,
      line,
      text: format!("note {id}"),
      created_at: "2026-07-03T10:00:00Z".into(),
      updated_at: updated.into(),
    }
  }

  #[test]
  fn side_serializes_as_camelcase_variant_name() {
    assert_eq!(serde_json::to_value(Side::Old).unwrap(), serde_json::json!("Old"));
    assert_eq!(serde_json::to_value(Side::New).unwrap(), serde_json::json!("New"));
  }

  #[test]
  fn unknown_fields_are_rejected() {
    let mut v = serde_json::to_value(comment("c1", "a.rs", 5, "t")).unwrap();
    v.as_object_mut().unwrap().insert("sneaky".into(), serde_json::json!(true));
    assert!(serde_json::from_value::<Comment>(v).is_err());
  }

  #[test]
  fn upsert_inserts_then_replaces() {
    let mut d = doc();
    d.upsert(comment("c1", "a.rs", 5, "2026-07-03T10:00:00Z")).unwrap();
    let mut edited = comment("c1", "a.rs", 5, "2026-07-03T11:00:00Z");
    edited.text = "edited".into();
    d.upsert(edited).unwrap();
    assert_eq!(d.comments.len(), 1);
    assert_eq!(d.comments[0].text, "edited");
  }

  #[test]
  fn upsert_rejects_invalid() {
    let mut d = doc();
    let mut bad = comment("c1", "a.rs", 5, "2026-07-03T10:00:00Z");
    bad.text = "   ".into();
    assert!(matches!(d.upsert(bad), Err(ModelError::InvalidComment(_))));
    let mut bad = comment("c2", "a.rs", 5, "2026-07-03T10:00:00Z");
    bad.line = 0;
    assert!(d.upsert(bad).is_err());
    assert!(d.comments.is_empty());
  }

  #[test]
  fn delete_by_id() {
    let mut d = doc();
    d.upsert(comment("c1", "a.rs", 5, "2026-07-03T10:00:00Z")).unwrap();
    assert!(d.delete("c1"));
    assert!(!d.delete("c1"));
    assert!(d.comments.is_empty());
  }

  #[test]
  fn ordering_is_file_line_side_created_id() {
    let mut d = doc();
    d.upsert(comment("z", "b.rs", 1, "2026-07-03T10:00:00Z")).unwrap();
    d.upsert(comment("y", "a.rs", 9, "2026-07-03T10:00:00Z")).unwrap();
    d.upsert(comment("x", "a.rs", 2, "2026-07-03T10:00:00Z")).unwrap();
    let mut old_side = comment("w", "a.rs", 2, "2026-07-03T10:00:00Z");
    old_side.side = Side::Old;
    d.upsert(old_side).unwrap();
    let order: Vec<&str> = d.comments.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(order, vec!["w", "x", "y", "z"]); // old before new on the same line
  }

  #[test]
  fn merge_unions_and_newer_updated_at_wins() {
    let mut ours = doc();
    ours.upsert(comment("shared", "a.rs", 1, "2026-07-03T10:00:00Z")).unwrap();
    ours.upsert(comment("only-ours", "a.rs", 2, "2026-07-03T10:00:00Z")).unwrap();

    let mut theirs = doc();
    let mut newer = comment("shared", "a.rs", 1, "2026-07-03T12:00:00Z");
    newer.text = "their newer edit".into();
    theirs.upsert(newer).unwrap();
    theirs.upsert(comment("only-theirs", "a.rs", 3, "2026-07-03T10:00:00Z")).unwrap();

    ours.merge(&theirs);
    assert_eq!(ours.comments.len(), 3);
    let shared = ours.comments.iter().find(|c| c.id == "shared").unwrap();
    assert_eq!(shared.text, "their newer edit");
  }

  #[test]
  fn merge_older_incoming_loses() {
    let mut ours = doc();
    let mut mine = comment("shared", "a.rs", 1, "2026-07-03T12:00:00Z");
    mine.text = "my newer edit".into();
    ours.upsert(mine).unwrap();

    let mut theirs = doc();
    theirs.upsert(comment("shared", "a.rs", 1, "2026-07-03T10:00:00Z")).unwrap();

    ours.merge(&theirs);
    assert_eq!(ours.comments[0].text, "my newer edit");
  }

  #[test]
  fn parse_rejects_newer_schema() {
    let mut d = doc();
    d.schema_version = SCHEMA_VERSION + 1;
    let json = serde_json::to_string(&d).unwrap();
    assert_eq!(
      ReviewDocument::parse(&json),
      Err(ModelError::UnsupportedSchema { found: SCHEMA_VERSION + 1, supported: SCHEMA_VERSION })
    );
  }

  #[test]
  fn parse_rejects_garbage_and_invalid_comments() {
    assert!(matches!(ReviewDocument::parse("not json"), Err(ModelError::Json(_))));
    let mut d = doc();
    d.comments.push(Comment {
      id: "".into(),
      file: "a.rs".into(),
      side: Side::New,
      line: 1,
      text: "x".into(),
      created_at: "t".into(),
      updated_at: "t".into(),
    });
    let json = serde_json::to_string(&d).unwrap();
    assert!(matches!(ReviewDocument::parse(&json), Err(ModelError::InvalidComment(_))));
  }

  #[test]
  fn parse_normalizes_order() {
    let mut d = doc();
    d.comments.push(comment("b", "z.rs", 9, "2026-07-03T10:00:00Z"));
    d.comments.push(comment("a", "a.rs", 1, "2026-07-03T10:00:00Z"));
    let parsed = ReviewDocument::parse(&serde_json::to_string(&d).unwrap()).unwrap();
    assert_eq!(parsed.comments[0].id, "a");
  }

  #[test]
  fn canonical_json_roundtrips() {
    let mut d = doc();
    d.upsert(comment("c1", "a.rs", 5, "2026-07-03T10:00:00Z")).unwrap();
    let json = d.to_json_pretty();
    assert!(json.ends_with('\n'));
    let back = ReviewDocument::parse(&json).unwrap();
    assert_eq!(back, d);
  }
}
