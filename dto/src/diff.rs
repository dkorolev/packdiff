//! The diff document: what a build extracted from git, plus the parser that
//! turns raw `git diff` text into typed data.
//!
//! [`parse_unified_diff`] is a pure function `&str -> Vec<FileDiff>` — the CLI
//! feeds it `git diff --no-color --no-ext-diff --find-renames` output. Nothing
//! here shells out.

use serde::{Deserialize, Serialize};

use crate::{RefInfo, SCHEMA_VERSION, TOOL};

/// The immutable build artifact: one rendered diff between two pinned refs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffDocument {
  /// Schema generation this document was written with; readers reject
  /// documents newer than they understand ([`SCHEMA_VERSION`]).
  pub schema_version: u32,
  /// Producing tool identifier (`"packdiff"`); lets a reader distinguish this
  /// document from look-alike JSON of other origins.
  pub tool: String,
  /// Repository directory name — not a path, so documents stay shareable
  /// across machines.
  pub repo: String,
  /// The base ref, pinned to the SHA it resolved to at build time.
  pub base: RefInfo,
  /// The head ref, pinned to the SHA it resolved to at build time.
  pub head: RefInfo,
  /// The commit the diff actually starts from: `merge-base(base, head)` in
  /// the default three-dot mode, or `base` itself in two-dot mode.
  pub merge_base: String,
  /// Build timestamp, RFC 3339 UTC — supplied by the caller (the model has
  /// no clock); the only non-deterministic field in a build.
  pub generated_at: String,
  /// Commits in the diffed range, oldest first.
  pub commits: Vec<Commit>,
  /// Per-file changes, in the order git emitted them.
  pub files: Vec<FileDiff>,
  /// File snapshots at every commit boundary, enabling in-page filtering of
  /// the diff to any contiguous commit sub-range. `None` (and omitted from
  /// JSON) when not collected — on older builds, or ranges of fewer than
  /// two commits, where there is nothing to filter.
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub snapshots: Option<crate::snapshot::RangeSnapshots>,
}

impl DiffDocument {
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    repo: String, base: RefInfo, head: RefInfo, merge_base: String, generated_at: String, commits: Vec<Commit>,
    files: Vec<FileDiff>, snapshots: Option<crate::snapshot::RangeSnapshots>,
  ) -> Self {
    Self {
      schema_version: SCHEMA_VERSION,
      tool: TOOL.to_string(),
      repo,
      base,
      head,
      merge_base,
      generated_at,
      commits,
      files,
      snapshots,
    }
  }

  pub fn additions(&self) -> u32 {
    self.files.iter().map(|f| f.additions).sum()
  }

  pub fn deletions(&self) -> u32 {
    self.files.iter().map(|f| f.deletions).sum()
  }
}

/// One commit in the diffed range.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commit {
  /// Full 40-hex commit id — the stable identity of the commit.
  pub sha: String,
  /// Abbreviated id as git printed it, for human-facing display.
  pub short: String,
  /// Author name, verbatim from git (`%an`).
  pub author: String,
  /// Author email, verbatim from git (`%ae`); kept so exports can attribute
  /// precisely even when display names collide.
  pub email: String,
  /// Author date, RFC 3339 (git `%aI`).
  pub date: String,
  /// First line of the commit message.
  pub subject: String,
}

/// How a file changed. Serialized as the bare `CamelCase` variant name
/// (`"Added"` / `"Deleted"` / `"Modified"` / `"Renamed"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileStatus {
  /// The file exists only in the post-image.
  Added,
  /// The file exists only in the pre-image.
  Deleted,
  /// Same path on both sides, contents differ.
  Modified,
  /// Detected rename (`--find-renames`); paths differ, content may too.
  Renamed,
}

/// One file's change. `old_path`/`new_path` are `None` where the file does not
/// exist on that side (added / deleted).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileDiff {
  /// Pre-image path; `None` for added files.
  pub old_path: Option<String>,
  /// Post-image path; `None` for deleted files.
  pub new_path: Option<String>,
  /// The kind of change; determines which of the paths are present.
  pub status: FileStatus,
  /// True when git reported binary content — such files carry no hunks.
  pub binary: bool,
  /// Textual change hunks, in file order; empty for binary or metadata-only
  /// changes.
  pub hunks: Vec<Hunk>,
  /// Count of `+` lines across all hunks; denormalized so consumers need not
  /// re-walk the hunks for totals.
  pub additions: u32,
  /// Count of `-` lines across all hunks; denormalized like `additions`.
  pub deletions: u32,
  /// Extended-header notes worth surfacing to a reviewer (mode changes).
  pub notes: Vec<String>,
}

impl FileDiff {
  /// Human-facing label: `old → new` for renames, otherwise the path.
  pub fn display_path(&self) -> String {
    match (self.status, &self.old_path, &self.new_path) {
      (FileStatus::Renamed, Some(old), Some(new)) if old != new => format!("{old} → {new}"),
      _ => self.anchor_path().to_string(),
    }
  }

  /// The path comments anchor to: the post-image path when it exists.
  pub fn anchor_path(&self) -> &str {
    self.new_path.as_deref().or(self.old_path.as_deref()).unwrap_or("")
  }
}

/// One contiguous run of diff lines.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hunk {
  /// The raw `@@ -a,b +c,d @@ context` header line, kept verbatim for display.
  pub header: String,
  /// The hunk's lines, in order.
  pub lines: Vec<Line>,
}

/// One diff line. Encoded as a single-key union —
/// `{ "Add": { "new": 2, "text": "…" } }` — with no discriminator field.
/// Line numbers are 1-based and refer to the side named by the field
/// (`old` = pre-image, `new` = post-image) — the same coordinates comments
/// anchor to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Line {
  /// A line present only in the post-image (`+` in unified diff).
  Add {
    /// 1-based post-image line number.
    new: u32,
    /// Line content without the leading `+`.
    text: String,
  },
  /// A line present only in the pre-image (`-` in unified diff).
  Del {
    /// 1-based pre-image line number.
    old: u32,
    /// Line content without the leading `-`.
    text: String,
  },
  /// An unchanged context line, present on both sides.
  Ctx {
    /// 1-based pre-image line number.
    old: u32,
    /// 1-based post-image line number.
    new: u32,
    /// Line content without the leading space.
    text: String,
  },
  /// A diff-level annotation such as `\ No newline at end of file`; carries
  /// no line numbers.
  Meta {
    /// The annotation verbatim, including its leading backslash.
    text: String,
  },
}

/// Parse `git diff` unified output (with `--find-renames`) into typed files.
pub fn parse_unified_diff(text: &str) -> Vec<FileDiff> {
  let mut files: Vec<FileDiff> = Vec::new();
  let mut cur: Option<FileDiff> = None;
  let mut in_hunk = false;
  let mut old_no: u32 = 0;
  let mut new_no: u32 = 0;

  for line in text.lines() {
    if let Some(rest) = line.strip_prefix("diff --git ") {
      if let Some(done) = cur.take() {
        files.push(done);
      }
      let (old, new) = split_ab(rest);
      cur = Some(FileDiff {
        old_path: Some(old),
        new_path: Some(new),
        status: FileStatus::Modified,
        binary: false,
        hunks: Vec::new(),
        additions: 0,
        deletions: 0,
        notes: Vec::new(),
      });
      in_hunk = false;
      continue;
    }
    let Some(f) = cur.as_mut() else { continue };

    if in_hunk {
      // `in_hunk` is only set right after a hunk is pushed, so the expects
      // below are provable invariants, not runtime fallibility.
      if let Some(body) = line.strip_prefix('+') {
        f.hunks
          .last_mut()
          .expect("in_hunk implies a current hunk")
          .lines
          .push(Line::Add { new: new_no, text: body.to_string() });
        new_no += 1;
        f.additions += 1;
        continue;
      }
      if let Some(body) = line.strip_prefix('-') {
        f.hunks
          .last_mut()
          .expect("in_hunk implies a current hunk")
          .lines
          .push(Line::Del { old: old_no, text: body.to_string() });
        old_no += 1;
        f.deletions += 1;
        continue;
      }
      if let Some(body) = line.strip_prefix(' ') {
        f.hunks.last_mut().expect("in_hunk implies a current hunk").lines.push(Line::Ctx {
          old: old_no,
          new: new_no,
          text: body.to_string(),
        });
        old_no += 1;
        new_no += 1;
        continue;
      }
      if line.starts_with('\\') {
        f.hunks.last_mut().expect("in_hunk implies a current hunk").lines.push(Line::Meta { text: line.to_string() });
        continue;
      }
      in_hunk = false; // fall through to header handling
    }

    if let Some((o, n)) = parse_hunk_header(line) {
      old_no = o;
      new_no = n;
      f.hunks.push(Hunk { header: line.to_string(), lines: Vec::new() });
      in_hunk = true;
    } else if line.starts_with("new file mode") {
      f.status = FileStatus::Added;
      f.old_path = None;
    } else if line.starts_with("deleted file mode") {
      f.status = FileStatus::Deleted;
      f.new_path = None;
    } else if let Some(p) = line.strip_prefix("rename from ") {
      f.status = FileStatus::Renamed;
      f.old_path = Some(p.to_string());
    } else if let Some(p) = line.strip_prefix("rename to ") {
      f.status = FileStatus::Renamed;
      f.new_path = Some(p.to_string());
    } else if line.starts_with("old mode ") || line.starts_with("new mode ") {
      f.notes.push(line.to_string());
    } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
      f.binary = true;
    } else if let Some(p) = line.strip_prefix("--- ") {
      if let Some(stripped) = strip_prefix_path(p) {
        f.old_path = Some(stripped);
      } else if f.status != FileStatus::Added {
        // `--- /dev/null` on a file we haven't classified yet.
        f.status = FileStatus::Added;
        f.old_path = None;
      }
    } else if let Some(p) = line.strip_prefix("+++ ") {
      if let Some(stripped) = strip_prefix_path(p) {
        f.new_path = Some(stripped);
      } else if f.status != FileStatus::Deleted {
        f.status = FileStatus::Deleted;
        f.new_path = None;
      }
    }
    // `index`, `similarity index`, `copy from/to` headers are ignored.
  }
  if let Some(done) = cur.take() {
    files.push(done);
  }
  files
}

/// `@@ -a[,b] +c[,d] @@ …` → `(a, c)`, or None if the line is not a hunk header.
fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
  let rest = line.strip_prefix("@@ -")?;
  let (old_part, rest) = rest.split_once(" +")?;
  let (new_part, _) = rest.split_once(" @@")?;
  let old = old_part.split(',').next()?.parse().ok()?;
  let new = new_part.split(',').next()?.parse().ok()?;
  Some((old, new))
}

/// Drop the `a/` / `b/` prefix; `None` for `/dev/null`.
fn strip_prefix_path(path: &str) -> Option<String> {
  if path == "/dev/null" {
    return None;
  }
  let stripped = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")).unwrap_or(path);
  Some(stripped.to_string())
}

/// Best-effort split of `a/old b/new` from a `diff --git` header. Exact for
/// paths without spaces; paths WITH spaces are re-derived from the
/// `---`/`+++`/`rename` headers that follow, so this only needs to not crash.
fn split_ab(rest: &str) -> (String, String) {
  if let Some(stripped) = rest.strip_prefix('"') {
    // Quoted (unusual) paths: `"a/x" "b/y"`.
    if let Some((old, new)) = stripped.split_once("\" \"") {
      let old = old.strip_prefix("a/").unwrap_or(old);
      let new = new.strip_prefix("b/").unwrap_or(new).trim_end_matches('"');
      return (old.to_string(), new.to_string());
    }
  }
  if let Some(idx) = rest.rfind(" b/") {
    let old = strip_prefix_path(&rest[..idx]).unwrap_or_default();
    let new = rest[idx + 3..].to_string();
    return (old, new);
  }
  (rest.to_string(), rest.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  const SAMPLE: &str = "\
diff --git a/hello.py b/hello.py
index 1111111..2222222 100644
--- a/hello.py
+++ b/hello.py
@@ -1,2 +1,4 @@
 def hello():
-    return 'hi'
+    return 'hello'
+
+# trailing
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 3333333..0000000
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-obsolete
\\ No newline at end of file
diff --git a/old name.txt b/new name.txt
similarity index 90%
rename from old name.txt
rename to new name.txt
diff --git a/fresh.md b/fresh.md
new file mode 100644
index 0000000..4444444
--- /dev/null
+++ b/fresh.md
@@ -0,0 +1,2 @@
+# Fresh
+body
diff --git a/blob.bin b/blob.bin
index 5555555..6666666 100644
Binary files a/blob.bin and b/blob.bin differ
";

  fn parsed() -> Vec<FileDiff> {
    parse_unified_diff(SAMPLE)
  }

  #[test]
  fn parses_all_files() {
    assert_eq!(parsed().len(), 5);
  }

  #[test]
  fn modified_file_lines_and_numbers() {
    let files = parsed();
    let f = &files[0];
    assert_eq!(f.status, FileStatus::Modified);
    assert_eq!((f.additions, f.deletions), (3, 1));
    let lines = &f.hunks[0].lines;
    assert_eq!(lines[0], Line::Ctx { old: 1, new: 1, text: "def hello():".into() });
    assert_eq!(lines[1], Line::Del { old: 2, text: "    return 'hi'".into() });
    assert_eq!(lines[2], Line::Add { new: 2, text: "    return 'hello'".into() });
    assert_eq!(lines[4], Line::Add { new: 4, text: "# trailing".into() });
  }

  #[test]
  fn deleted_file() {
    let files = parsed();
    let f = &files[1];
    assert_eq!(f.status, FileStatus::Deleted);
    assert_eq!(f.new_path, None);
    assert_eq!(f.anchor_path(), "gone.txt");
    assert!(matches!(f.hunks[0].lines.last(), Some(Line::Meta { .. })));
  }

  #[test]
  fn rename_with_spaces_in_paths() {
    let files = parsed();
    let f = &files[2];
    assert_eq!(f.status, FileStatus::Renamed);
    assert_eq!(f.old_path.as_deref(), Some("old name.txt"));
    assert_eq!(f.new_path.as_deref(), Some("new name.txt"));
    assert_eq!(f.display_path(), "old name.txt → new name.txt");
    assert!(f.hunks.is_empty());
  }

  #[test]
  fn added_file() {
    let files = parsed();
    let f = &files[3];
    assert_eq!(f.status, FileStatus::Added);
    assert_eq!(f.old_path, None);
    assert_eq!(f.additions, 2);
  }

  #[test]
  fn binary_file() {
    let files = parsed();
    assert!(files[4].binary);
    assert!(files[4].hunks.is_empty());
  }

  #[test]
  fn hunk_header_forms() {
    assert_eq!(parse_hunk_header("@@ -1,2 +3,4 @@"), Some((1, 3)));
    assert_eq!(parse_hunk_header("@@ -1 +0,0 @@"), Some((1, 0)));
    assert_eq!(parse_hunk_header("@@ -10,5 +12,7 @@ fn ctx()"), Some((10, 12)));
    assert_eq!(parse_hunk_header("not a hunk"), None);
  }

  #[test]
  fn lines_encode_as_single_key_unions() {
    let add = Line::Add { new: 2, text: "x".into() };
    assert_eq!(serde_json::to_value(&add).unwrap(), serde_json::json!({ "Add": { "new": 2, "text": "x" } }));
    assert_eq!(serde_json::to_value(FileStatus::Renamed).unwrap(), serde_json::json!("Renamed"));
  }

  #[test]
  fn unknown_fields_are_rejected() {
    let bad = r#"{ "Add": { "new": 2, "text": "x", "sneaky": true } }"#;
    assert!(serde_json::from_str::<Line>(bad).is_err());
  }

  #[test]
  fn document_roundtrips_through_json() {
    let doc = DiffDocument::new(
      "repo".into(),
      RefInfo { name: "main".into(), sha: "a".repeat(40) },
      RefInfo { name: "feat".into(), sha: "b".repeat(40) },
      "c".repeat(40),
      "2026-07-03T00:00:00Z".into(),
      vec![],
      parsed(),
      None,
    );
    let json = serde_json::to_string(&doc).unwrap();
    let back: DiffDocument = serde_json::from_str(&json).unwrap();
    assert_eq!(back.schema_version, SCHEMA_VERSION);
    assert_eq!(back.files.len(), 5);
    assert_eq!(back.additions(), doc.additions());
  }
}
