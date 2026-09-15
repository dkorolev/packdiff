//! The diff document: what a build extracted from git, plus the parser that
//! turns raw `git diff` text into typed data.
//!
//! [`parse_unified_diff`] is a pure function `&str -> Vec<FileDiff>` — the CLI
//! feeds it `git diff --no-color --no-ext-diff --find-renames` output. Nothing
//! here shells out.

use crate::json::{self, Fields, FromJson, Map, ToJson, Value};
use crate::{RefInfo, SCHEMA_VERSION, TOOL};

/// The immutable build artifact: one rendered diff between two pinned refs.
#[derive(Debug, Clone)]
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
  /// the diff to any contiguous commit sub-range (two or more commits) and
  /// in-page expansion of hunk context (any non-empty range). `None` (and
  /// omitted from JSON) when not collected — on older builds, or empty
  /// ranges, where there is no content to snapshot.
  pub snapshots: Option<crate::snapshot::RangeSnapshots>,
  /// The PR description lifted out of the diff (the notes-commit
  /// convention: a commit whose changes are confined to notes files such as
  /// `PR-DESCRIPTION.md` carries notes, not code under review). Rendered as
  /// its own commentable panel; its commits and file are excluded from
  /// `commits` / `files`. `None` (and omitted from JSON) when the range has
  /// no notes commits. When several notes commits each rewrote the
  /// description, this is the NEWEST version and the older ones land in
  /// [`Self::superseded_descriptions`].
  pub description: Option<NotesFile>,
  /// Earlier versions of [`Self::description`], newest first — non-empty
  /// only when the range commits `PR-DESCRIPTION.md` more than once, which
  /// is a malformed history: the description is metadata about the change,
  /// so it belongs in exactly one notes commit. Every version is kept (and
  /// rendered as its own commentable panel) rather than silently dropped,
  /// so a reviewer can comment on whichever one they meant. Empty (and
  /// omitted from JSON) in the well-formed case.
  pub superseded_descriptions: Vec<NotesFile>,
  /// Decisions journaled while the change was made, lifted out of the diff
  /// by the same notes-commit convention as [`Self::description`]: notes
  /// commits carrying `PR-DECISION-<topic>.md` record what was decided and
  /// why, not code under review. Each is rendered as its own commentable
  /// panel, ordered by path; their commits and files are excluded from
  /// `commits` / `files`. Empty (and omitted from JSON) when the range
  /// journals no decisions.
  pub decisions: Vec<NotesFile>,
}

/// A notes file lifted out of the diff and presented as a page panel.
#[derive(Debug, Clone)]
pub struct NotesFile {
  /// The path the file was committed under (e.g. `PR-DESCRIPTION.md`);
  /// comments on the rendered panel anchor to this path, `New` side,
  /// 1-based source lines. Several panels can share a path when
  /// [`Self::revision`] tells them apart.
  pub path: String,
  /// The file's full markdown text, as of the last notes commit — or, when
  /// [`Self::revision`] is set, as of that one commit.
  pub text: String,
  /// The notes commits (full SHAs, oldest first) hidden from the commit
  /// list — recorded so the provenance stays in the document.
  pub commits: Vec<String>,
  /// The single notes commit this version came from, set only when the
  /// document carries several versions of one path and the panels must name
  /// which is which. `None` (and omitted from JSON) in the unambiguous case.
  pub revision: Option<NotesRevision>,
}

/// The notes commit one version of a [`NotesFile`] came from: enough to
/// label its panel and to point the reviewer at the commit to squash away.
#[derive(Debug, Clone)]
pub struct NotesRevision {
  /// Abbreviated commit SHA, as git rendered it.
  pub short: String,
  /// The commit's subject line.
  pub subject: String,
}

impl DiffDocument {
  #[allow(clippy::too_many_arguments)]
  pub fn new(
    repo: String, base: RefInfo, head: RefInfo, merge_base: String, generated_at: String, commits: Vec<Commit>,
    files: Vec<FileDiff>, snapshots: Option<crate::snapshot::RangeSnapshots>, description: Option<NotesFile>,
    decisions: Vec<NotesFile>,
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
      description,
      // Set by the builder only in the malformed multiple-description case,
      // which is rare enough not to earn an eleventh positional argument.
      superseded_descriptions: Vec::new(),
      decisions,
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
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

// ------------------------------------------------------------ JSON codec
//
// Every struct lists its fields once for writing and once for reading, in
// the documented order; reading is strict (unknown fields rejected). Fields
// documented as "omitted from JSON" when absent or empty are skipped on
// write and defaulted on read.

impl ToJson for DiffDocument {
  fn to_json(&self) -> Value {
    let mut o = Map::new();
    o.insert("schema_version", self.schema_version);
    o.insert("tool", &self.tool);
    o.insert("repo", &self.repo);
    o.insert("base", self.base.to_json());
    o.insert("head", self.head.to_json());
    o.insert("merge_base", &self.merge_base);
    o.insert("generated_at", &self.generated_at);
    o.insert("commits", self.commits.to_json());
    o.insert("files", self.files.to_json());
    if let Some(snapshots) = &self.snapshots {
      o.insert("snapshots", snapshots.to_json());
    }
    if let Some(description) = &self.description {
      o.insert("description", description.to_json());
    }
    if !self.superseded_descriptions.is_empty() {
      o.insert("superseded_descriptions", self.superseded_descriptions.to_json());
    }
    if !self.decisions.is_empty() {
      o.insert("decisions", self.decisions.to_json());
    }
    Value::Object(o)
  }
}

impl FromJson for DiffDocument {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "DiffDocument")?;
    let doc = DiffDocument {
      schema_version: f.required("schema_version")?,
      tool: f.required("tool")?,
      repo: f.required("repo")?,
      base: f.required("base")?,
      head: f.required("head")?,
      merge_base: f.required("merge_base")?,
      generated_at: f.required("generated_at")?,
      commits: f.required("commits")?,
      files: f.required("files")?,
      snapshots: f.optional("snapshots")?,
      description: f.optional("description")?,
      superseded_descriptions: f.or_default("superseded_descriptions")?,
      decisions: f.or_default("decisions")?,
    };
    f.finish()?;
    Ok(doc)
  }
}

impl ToJson for NotesFile {
  fn to_json(&self) -> Value {
    let mut o = Map::new();
    o.insert("path", &self.path);
    o.insert("text", &self.text);
    o.insert("commits", self.commits.to_json());
    if let Some(revision) = &self.revision {
      o.insert("revision", revision.to_json());
    }
    Value::Object(o)
  }
}

impl FromJson for NotesFile {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "NotesFile")?;
    let notes = NotesFile {
      path: f.required("path")?,
      text: f.required("text")?,
      commits: f.required("commits")?,
      revision: f.optional("revision")?,
    };
    f.finish()?;
    Ok(notes)
  }
}

impl ToJson for NotesRevision {
  fn to_json(&self) -> Value {
    Value::object([("short", &self.short), ("subject", &self.subject)])
  }
}

impl FromJson for NotesRevision {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "NotesRevision")?;
    let revision = NotesRevision { short: f.required("short")?, subject: f.required("subject")? };
    f.finish()?;
    Ok(revision)
  }
}

impl ToJson for Commit {
  fn to_json(&self) -> Value {
    Value::object([
      ("sha", &self.sha),
      ("short", &self.short),
      ("author", &self.author),
      ("email", &self.email),
      ("date", &self.date),
      ("subject", &self.subject),
    ])
  }
}

impl FromJson for Commit {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "Commit")?;
    let commit = Commit {
      sha: f.required("sha")?,
      short: f.required("short")?,
      author: f.required("author")?,
      email: f.required("email")?,
      date: f.required("date")?,
      subject: f.required("subject")?,
    };
    f.finish()?;
    Ok(commit)
  }
}

impl ToJson for FileStatus {
  fn to_json(&self) -> Value {
    Value::from(match self {
      FileStatus::Added => "Added",
      FileStatus::Deleted => "Deleted",
      FileStatus::Modified => "Modified",
      FileStatus::Renamed => "Renamed",
    })
  }
}

impl FromJson for FileStatus {
  fn from_json(value: &Value) -> json::Result<Self> {
    match json::variant_name(value, "FileStatus")? {
      "Added" => Ok(FileStatus::Added),
      "Deleted" => Ok(FileStatus::Deleted),
      "Modified" => Ok(FileStatus::Modified),
      "Renamed" => Ok(FileStatus::Renamed),
      other => Err(json::Error::unknown_variant(other, "FileStatus")),
    }
  }
}

impl ToJson for FileDiff {
  fn to_json(&self) -> Value {
    Value::object([
      ("old_path", self.old_path.to_json()),
      ("new_path", self.new_path.to_json()),
      ("status", self.status.to_json()),
      ("binary", self.binary.to_json()),
      ("hunks", self.hunks.to_json()),
      ("additions", self.additions.to_json()),
      ("deletions", self.deletions.to_json()),
      ("notes", self.notes.to_json()),
    ])
  }
}

impl FromJson for FileDiff {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "FileDiff")?;
    let file = FileDiff {
      old_path: f.optional("old_path")?,
      new_path: f.optional("new_path")?,
      status: f.required("status")?,
      binary: f.required("binary")?,
      hunks: f.required("hunks")?,
      additions: f.required("additions")?,
      deletions: f.required("deletions")?,
      notes: f.required("notes")?,
    };
    f.finish()?;
    Ok(file)
  }
}

impl ToJson for Hunk {
  fn to_json(&self) -> Value {
    Value::object([("header", Value::from(&self.header)), ("lines", self.lines.to_json())])
  }
}

impl FromJson for Hunk {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "Hunk")?;
    let hunk = Hunk { header: f.required("header")?, lines: f.required("lines")? };
    f.finish()?;
    Ok(hunk)
  }
}

impl ToJson for Line {
  fn to_json(&self) -> Value {
    let (variant, payload) = match self {
      Line::Add { new, text } => ("Add", Value::object([("new", Value::from(*new)), ("text", Value::from(text))])),
      Line::Del { old, text } => ("Del", Value::object([("old", Value::from(*old)), ("text", Value::from(text))])),
      Line::Ctx { old, new, text } => {
        ("Ctx", Value::object([("old", Value::from(*old)), ("new", Value::from(*new)), ("text", Value::from(text))]))
      }
      Line::Meta { text } => ("Meta", Value::object([("text", Value::from(text))])),
    };
    Value::object([(variant, payload)])
  }
}

impl FromJson for Line {
  fn from_json(value: &Value) -> json::Result<Self> {
    let (variant, payload) = json::union(value, "Line")?;
    let line = match variant {
      "Add" => {
        let mut f = Fields::of(payload, "Line::Add")?;
        let line = Line::Add { new: f.required("new")?, text: f.required("text")? };
        f.finish()?;
        line
      }
      "Del" => {
        let mut f = Fields::of(payload, "Line::Del")?;
        let line = Line::Del { old: f.required("old")?, text: f.required("text")? };
        f.finish()?;
        line
      }
      "Ctx" => {
        let mut f = Fields::of(payload, "Line::Ctx")?;
        let line = Line::Ctx { old: f.required("old")?, new: f.required("new")?, text: f.required("text")? };
        f.finish()?;
        line
      }
      "Meta" => {
        let mut f = Fields::of(payload, "Line::Meta")?;
        let line = Line::Meta { text: f.required("text")? };
        f.finish()?;
        line
      }
      other => return Err(json::Error::unknown_variant(other, "Line")),
    };
    Ok(line)
  }
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
    assert_eq!(add.to_json(), json::parse(r#"{ "Add": { "new": 2, "text": "x" } }"#).unwrap());
    assert_eq!(FileStatus::Renamed.to_json(), Value::from("Renamed"));
  }

  #[test]
  fn unknown_fields_are_rejected() {
    let bad = r#"{ "Add": { "new": 2, "text": "x", "sneaky": true } }"#;
    assert!(json::from_str::<Line>(bad).is_err());
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
      Some(NotesFile {
        path: "PR-DESCRIPTION.md".into(),
        text: "# Title\n\nBody.".into(),
        commits: vec!["d".repeat(40)],
        revision: None,
      }),
      vec![NotesFile {
        path: "PR-DECISION-retry-safety.md".into(),
        text: "# Retry safety\n\nOnly unprocessed requests retry.".into(),
        commits: vec!["d".repeat(40)],
        revision: None,
      }],
    );
    let json = doc.to_json().to_string();
    let back: DiffDocument = json::from_str(&json).unwrap();
    assert_eq!(back.schema_version, SCHEMA_VERSION);
    assert_eq!(back.files.len(), 5);
    assert_eq!(back.additions(), doc.additions());
    assert_eq!(back.description.unwrap().path, "PR-DESCRIPTION.md");
    assert_eq!(back.decisions.len(), 1);
    assert_eq!(back.decisions[0].path, "PR-DECISION-retry-safety.md");
    // Absent notes stay absent from the JSON, not `null` / `[]` — a document
    // that journals nothing is byte-identical to one from before decisions
    // existed, which is why this addition needs no schema bump.
    let none = DiffDocument::new(
      "repo".into(),
      RefInfo { name: "main".into(), sha: "a".repeat(40) },
      RefInfo { name: "feat".into(), sha: "b".repeat(40) },
      "c".repeat(40),
      "2026-07-03T00:00:00Z".into(),
      vec![],
      vec![],
      None,
      None,
      Vec::new(),
    );
    let json = none.to_json().to_string();
    assert!(!json.contains("description"));
    assert!(!json.contains("decisions"));
    assert!(!json.contains("revision"));
    // A document written before decisions existed still reads.
    let back: DiffDocument = json::from_str(&json).unwrap();
    assert!(back.decisions.is_empty());
    assert!(back.superseded_descriptions.is_empty());
  }
}
