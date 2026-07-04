//! Export renderers for a [`ReviewDocument`]: Markdown, CSV, and the
//! canonical JSON. Pure string builders — deterministic given the document
//! (which [`ReviewDocument::sort`] keeps in canonical order).

use crate::review::{ReviewDocument, Side};

fn side_label(side: Side) -> &'static str {
  match side {
    Side::Old => "old",
    Side::New => "new",
  }
}

/// Lossless export: the canonical pretty JSON of the whole document. This is
/// the only format `merge`/import accepts back.
pub fn to_json(doc: &ReviewDocument) -> String {
  doc.to_json_pretty()
}

/// Human/agent-friendly export: comments grouped by file.
///
/// ```markdown
/// # Review comments — repo main..feature
///
/// 2 comment(s) · base <sha12> · head <sha12>
///
/// ## src/app.rs
///
/// - **L42 (new)** — first line
///   continuation lines indented
/// ```
pub fn to_markdown(doc: &ReviewDocument) -> String {
  let mut out = String::new();
  out.push_str(&format!("# Review comments — {} {}..{}\n\n", doc.repo, doc.base.name, doc.head.name));
  out.push_str(&format!(
    "{} comment(s) · base {} · head {}\n",
    doc.comments.len(),
    &doc.base.sha[..doc.base.sha.len().min(12)],
    &doc.head.sha[..doc.head.sha.len().min(12)],
  ));
  let mut current_file: Option<&str> = None;
  for c in &doc.comments {
    if current_file != Some(c.file.as_str()) {
      current_file = Some(c.file.as_str());
      out.push_str(&format!("\n## {}\n\n", c.file));
    }
    let body = c.text.replace('\n', "\n  ");
    out.push_str(&format!("- **L{} ({})** — {}\n", c.line, side_label(c.side), body));
  }
  out
}

/// Spreadsheet export: RFC 4180 (quoted fields, CRLF line endings).
pub fn to_csv(doc: &ReviewDocument) -> String {
  fn field(v: &str) -> String {
    format!("\"{}\"", v.replace('"', "\"\""))
  }
  let mut rows = vec![["file", "side", "line", "created_at", "updated_at", "text"].map(field).join(",")];
  for c in &doc.comments {
    rows.push(
      [
        c.file.as_str(),
        side_label(c.side),
        &c.line.to_string(),
        c.created_at.as_str(),
        c.updated_at.as_str(),
        c.text.as_str(),
      ]
      .map(field)
      .join(","),
    );
  }
  rows.push(String::new()); // trailing CRLF
  rows.join("\r\n")
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::review::Comment;
  use crate::RefInfo;

  fn doc_with_comments() -> ReviewDocument {
    let mut d = ReviewDocument::new(
      "myrepo".into(),
      RefInfo { name: "main".into(), sha: "a".repeat(40) },
      RefInfo { name: "feat".into(), sha: "b".repeat(40) },
    );
    d.upsert(Comment {
      id: "c2".into(),
      file: "src/lib.rs".into(),
      side: Side::Old,
      line: 7,
      text: "why removed?".into(),
      created_at: "2026-07-03T10:05:00Z".into(),
      updated_at: "2026-07-03T10:05:00Z".into(),
    })
    .unwrap();
    d.upsert(Comment {
      id: "c1".into(),
      file: "src/app.rs".into(),
      side: Side::New,
      line: 42,
      text: "multi\nline \"note\"".into(),
      created_at: "2026-07-03T10:00:00Z".into(),
      updated_at: "2026-07-03T10:00:00Z".into(),
    })
    .unwrap();
    d
  }

  #[test]
  fn markdown_groups_by_file_and_indents_continuations() {
    let md = to_markdown(&doc_with_comments());
    assert!(md.starts_with("# Review comments — myrepo main..feat\n"));
    assert!(md.contains("2 comment(s) · base aaaaaaaaaaaa · head bbbbbbbbbbbb"));
    let app = md.find("## src/app.rs").unwrap();
    let lib = md.find("## src/lib.rs").unwrap();
    assert!(app < lib, "files sorted");
    assert!(md.contains("- **L42 (new)** — multi\n  line \"note\"\n"));
    assert!(md.contains("- **L7 (old)** — why removed?\n"));
  }

  #[test]
  fn csv_quotes_and_escapes() {
    let csv = to_csv(&doc_with_comments());
    let lines: Vec<&str> = csv.split("\r\n").collect();
    assert_eq!(lines[0], "\"file\",\"side\",\"line\",\"created_at\",\"updated_at\",\"text\"");
    assert!(lines[1].starts_with("\"src/app.rs\",\"new\",\"42\""));
    assert!(lines[1].contains("\"multi\nline \"\"note\"\"\""));
    assert_eq!(lines.last(), Some(&""), "ends with CRLF");
  }

  #[test]
  fn json_export_is_canonical_and_reimportable() {
    let d = doc_with_comments();
    let json = to_json(&d);
    let back = ReviewDocument::parse(&json).unwrap();
    assert_eq!(back, d);
  }

  #[test]
  fn empty_document_exports() {
    let d = ReviewDocument::new(
      "r".into(),
      RefInfo { name: "a".into(), sha: "a".repeat(40) },
      RefInfo { name: "b".into(), sha: "b".repeat(40) },
    );
    assert!(to_markdown(&d).contains("0 comment(s)"));
    let csv = to_csv(&d);
    assert!(csv.ends_with("\"text\"\r\n"), "header row only, CRLF-terminated: {csv:?}");
  }
}
