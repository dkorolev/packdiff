//! HTML page assembly. Presentation only: consumes `packdiff_dto` types,
//! escapes everything, inlines CSS + JS + the base64 wasm module. The page it
//! emits makes zero network requests and works from `file://`.
//!
//! Authored frontend assets live in `cli/assets/` and are embedded at compile
//! time via `include_str!` — the generated artifact remains one self-contained
//! HTML file.
//!
//! The web-layer split is doctrine: strict Rust for the engine (all review
//! semantics, in `packdiff-dto`), vanilla JS for the player (`assets/page.js`,
//! presentation and browser state only) — see docs/ARCHITECTURE.md.

use packdiff_dto::diff::{DiffDocument, FileDiff, FileStatus, Line};
use packdiff_dto::markdown;

const CSS: &str = include_str!("../assets/page.css");
const JS: &str = include_str!("../assets/page.js");

pub fn esc(s: &str) -> String {
  let mut out = String::with_capacity(s.len());
  for ch in s.chars() {
    match ch {
      '&' => out.push_str("&amp;"),
      '<' => out.push_str("&lt;"),
      '>' => out.push_str("&gt;"),
      '"' => out.push_str("&quot;"),
      '\'' => out.push_str("&#x27;"),
      _ => out.push(ch),
    }
  }
  out
}

pub fn base64(data: &[u8]) -> String {
  const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
  for chunk in data.chunks(3) {
    let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
    let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
    out.push(TABLE[(n >> 18) as usize & 63] as char);
    out.push(TABLE[(n >> 12) as usize & 63] as char);
    out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
    out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
  }
  out
}

fn render_commits(doc: &DiffDocument) -> String {
  if doc.commits.is_empty() {
    return r#"<p class="muted">No commits in range.</p>"#.to_string();
  }
  // Rows are selectable (range filtering) only when snapshots were collected.
  let selectable = if doc.snapshots.is_some() { " selectable" } else { "" };
  let mut out = String::new();
  if doc.snapshots.is_some() {
    out.push_str(
      r#"<div class="hint"><strong>Inspect a commit or range.</strong> Select one commit, then Shift-click another to span a range. The filtered diff is read-only; return to the full diff to comment.</div>"#,
    );
  }
  out.push_str(r#"<table class="commits" role="grid" aria-label="Commits in range">"#);
  for (i, c) in doc.commits.iter().enumerate() {
    let check = if doc.snapshots.is_some() {
      format!(
        r#"<td class="sel"><input type="checkbox" class="commit-check" data-index="{index}" aria-label="Select commit {short}"></td>"#,
        index = i + 1,
        short = esc(&c.short),
      )
    } else {
      String::new()
    };
    out.push_str(&format!(
      r#"<tr class="commit{selectable}" data-index="{index}" data-sha="{sha}" aria-selected="false">{check}<td class="sha"><code>{short}</code><button class="copy-sha" type="button" title="Copy the full commit hash" aria-label="Copy full hash of {short}">copy</button></td><td class="subject">{subject}</td><td class="author">{author}</td><td class="date">{date}</td></tr>"#,
      index = i + 1,
      sha = esc(&c.sha),
      short = esc(&c.short),
      subject = esc(&c.subject),
      author = esc(&c.author),
      date = esc(&c.date),
      check = check,
    ));
  }
  out.push_str("</table>");
  if doc.snapshots.is_some() {
    out.push_str(
      r#"<div id="range-bar" hidden>
<span id="range-label"></span>
<span class="range-readonly">— read-only; comments attach to the full diff</span>
<button id="range-prev" type="button" title="Previous commit">Prev</button>
<button id="range-next" type="button" title="Next commit">Next</button>
<button id="range-reset" type="button">Show full diff</button>
</div>"#,
    );
  }
  out
}

/// True for paths the page offers a rendered-markdown view for.
fn is_markdown_path(path: &str) -> bool {
  path
    .rsplit_once('.')
    .is_some_and(|(_, ext)| matches!(ext.to_ascii_lowercase().as_str(), "md" | "markdown" | "mdown" | "mkd"))
}

/// Compact status letter for the sidebar (M / A / D / R).
fn status_letter(status: FileStatus) -> &'static str {
  match status {
    FileStatus::Modified => "M",
    FileStatus::Added => "A",
    FileStatus::Deleted => "D",
    FileStatus::Renamed => "R",
  }
}

/// The rendered-markdown view of a file's diff: consecutive runs of
/// context / added / removed lines render separately, added runs tinted
/// green and removed runs red. Modified files show hunk EXCERPTS with gap
/// markers; added files show the whole file. Multi-line markdown constructs
/// that straddle a change boundary render as separate blocks — inherent to
/// rendering a diff, not a bug.
///
/// Every rendered top-level block carries the comment anchor of the source
/// line it starts at (context and added lines anchor to the `New` side,
/// removed lines to `Old`), so commenting works in the preview too — the
/// same `(file, side, line)` coordinates as the diff table rows.
fn render_markdown_preview(f: &FileDiff) -> String {
  #[derive(PartialEq, Clone, Copy)]
  enum Run {
    Ctx,
    Add,
    Del,
  }
  let anchor = esc(f.anchor_path());
  let mut chunks: Vec<String> = Vec::new();
  for (i, hunk) in f.hunks.iter().enumerate() {
    if i > 0 {
      chunks.push(r#"<div class="md-gap">⋯</div>"#.to_string());
    }
    let mut runs: Vec<(Run, Vec<(u32, &str)>)> = Vec::new();
    for line in &hunk.lines {
      let (kind, no, text) = match line {
        Line::Ctx { new, text, .. } => (Run::Ctx, *new, text.as_str()),
        Line::Add { new, text } => (Run::Add, *new, text.as_str()),
        Line::Del { old, text } => (Run::Del, *old, text.as_str()),
        Line::Meta { .. } => continue,
      };
      match runs.last_mut() {
        Some((k, lines)) if *k == kind => lines.push((no, text)),
        _ => runs.push((kind, vec![(no, text)])),
      }
    }
    for (kind, lines) in runs {
      let text: Vec<&str> = lines.iter().map(|(_, t)| *t).collect();
      let blocks = markdown::to_html_blocks(&text.join("\n"));
      if blocks.is_empty() {
        continue;
      }
      let (class, side) = match kind {
        Run::Ctx => ("md-run", "New"),
        Run::Add => ("md-run md-run-add", "New"),
        Run::Del => ("md-run md-run-del", "Old"),
      };
      let mut inner = String::new();
      for (offset, html) in blocks {
        let line = lines[offset].0;
        // Gutter button is a stable comment target; the block keeps the same
        // anchor attributes for placement and keyboard flow.
        inner.push_str(&format!(
          r#"<div class="md-block-wrap"><div class="md-gutter"><button type="button" class="gutter-btn" aria-label="Add comment on line {line}" data-file="{anchor}" data-side="{side}" data-line="{line}">+</button></div><div class="md-block" data-file="{anchor}" data-side="{side}" data-line="{line}">{html}</div></div>"#,
        ));
      }
      chunks.push(format!(r#"<div class="{class}">{inner}</div>"#));
    }
  }
  let note = if f.status == FileStatus::Added {
    ""
  } else {
    r#"<div class="muted note">Rendered from the diff hunks — green added, red removed; unchanged text outside the hunks is not included.</div>"#
  };
  // Preview is a markdown file's default view; the diff is one pill-click
  // away (the page JS swaps the `hidden` attributes from then on).
  format!(r#"<div class="md-preview">{note}{}</div>"#, chunks.join(""))
}

/// The lifted PR description as a page panel: rendered markdown whose
/// top-level blocks carry comment anchors — `(path, New, 1-based line)` —
/// exactly like the file previews, so the page JS needs nothing new for
/// commenting here. A "fake file": not part of the reviewed diff, and shown
/// with NO provenance chrome — no file name, no notes author. It is simply
/// there, the way the pull request will present it; the path exists only
/// inside the comment anchors.
///
/// A Preview | Raw pill switches between the rendered card and a line-numbered
/// source view (same anchors), so reviewers can quote exact markdown syntax.
fn render_description(d: &packdiff_dto::diff::NotesFile) -> String {
  let anchor = esc(&d.path);
  let mut blocks = String::new();
  for (offset, html) in markdown::to_html_blocks(&d.text) {
    let line = offset + 1;
    blocks.push_str(&format!(
      r#"<div class="md-block-wrap"><div class="md-gutter"><button type="button" class="gutter-btn" aria-label="Add comment on line {line}" data-file="{anchor}" data-side="New" data-line="{line}">+</button></div><div class="md-block" data-file="{anchor}" data-side="New" data-line="{line}">{html}</div></div>"#,
    ));
  }
  // Raw source: one commentable row per line (including blank lines), so
  // anchors match the rendered blocks' starting lines and the export model.
  let mut raw_rows = String::new();
  let lines: Vec<&str> = if d.text.is_empty() { vec![""] } else { d.text.split('\n').collect() };
  // split leaves a trailing empty entry when the file ends with \n; keep
  // the logical source lines (same as editors: no phantom final blank).
  let lines = if lines.len() > 1 && lines.last() == Some(&"") { &lines[..lines.len() - 1] } else { &lines[..] };
  for (i, line) in lines.iter().enumerate() {
    let line_no = (i + 1) as u32;
    raw_rows.push_str(&format!(
      r#"<tr class="ctx commentable" data-file="{anchor}" data-side="New" data-line="{line_no}">{gutter}<td class="ln"></td><td class="ln">{line_no}</td><td class="code"> {text}</td></tr>"#,
      gutter = gutter_cell(&anchor, "New", line_no),
      text = esc(line),
    ));
  }
  format!(
    r#"<div class="md-preview">{blocks}</div>
<div class="desc-raw" hidden><div class="diff-scroll"><table class="diff unified desc-source" aria-label="Description source"><thead class="visually-hidden"><tr><th>Comment</th><th></th><th>Line</th><th>Source</th></tr></thead>{raw_rows}</table></div></div>"#,
    blocks = blocks,
    raw_rows = raw_rows,
  )
}

/// The status badge shown before a file's name; empty for a plain edit.
fn status_badge(status: FileStatus) -> &'static str {
  match status {
    FileStatus::Modified => "",
    FileStatus::Added => r#"<span class="badge badge-added">added</span> "#,
    FileStatus::Deleted => r#"<span class="badge badge-deleted">deleted</span> "#,
    FileStatus::Renamed => r#"<span class="badge badge-renamed">renamed</span> "#,
  }
}

/// Plain in-document file list (no-JS / print fallback); the interactive
/// sidebar is built by page JS from the same anchors.
fn render_file_list(files: &[FileDiff]) -> String {
  if files.is_empty() {
    return r#"<p class="muted">No changes between these refs.</p>"#.to_string();
  }
  let mut out = String::from(r#"<table class="filelist">"#);
  for (i, f) in files.iter().enumerate() {
    out.push_str(&format!(
      r##"<tr><td class="fl-path">{badge}<a href="#file-{i}">{path}</a></td><td class="fl-stats"><span class="adds">+{adds}</span> <span class="dels">−{dels}</span></td></tr>"##,
      badge = status_badge(f.status),
      path = esc(&f.display_path()),
      adds = f.additions,
      dels = f.deletions,
    ));
  }
  out.push_str("</table>");
  out
}

/// Sidebar seed markup: one button per file; JS enriches counts / viewed state.
fn render_sidebar_rows(files: &[FileDiff]) -> String {
  if files.is_empty() {
    return r#"<div class="sidebar-empty">No files changed.</div>"#.to_string();
  }
  let mut out = String::new();
  for (i, f) in files.iter().enumerate() {
    let path = f.display_path();
    let letter = status_letter(f.status);
    out.push_str(&format!(
      r##"<button type="button" class="sidebar-row" data-file-index="{i}" data-anchor="{anchor}" data-path="{path}" data-status="{letter}" data-adds="{adds}" data-dels="{dels}" title="{path}" aria-label="{letter} {path}, +{adds} −{dels}">
<span class="sb-status {letter}" aria-hidden="true">{letter}</span>
<span class="sb-path">{path}</span>
<span class="sb-meta"><span class="sb-comments" data-role="comment-count"></span><span class="adds">+{adds}</span> <span class="dels">−{dels}</span></span>
</button>
"##,
      anchor = esc(f.anchor_path()),
      path = esc(&path),
      letter = letter,
      adds = f.additions,
      dels = f.deletions,
    ));
  }
  out
}

/// Unified-table comment gutter cell for a commentable line.
fn gutter_cell(anchor: &str, side: &str, line_no: u32) -> String {
  format!(
    r#"<td class="gutter"><button type="button" class="gutter-btn" aria-label="Add comment on {side} line {line_no}" data-file="{anchor}" data-side="{side}" data-line="{line_no}">+</button></td>"#,
  )
}

fn render_file(index: usize, f: &FileDiff, nfiles: usize) -> String {
  let badge = status_badge(f.status);
  let notes: String = f.notes.iter().map(|n| format!(r#"<div class="muted note">{}</div>"#, esc(n))).collect();
  let renderable_markdown =
    is_markdown_path(f.anchor_path()) && !f.binary && f.status != FileStatus::Deleted && !f.hunks.is_empty();
  let toggle = if renderable_markdown {
    r#"<span class="seg md-seg" role="group" aria-label="Markdown view"><button type="button" data-mdview="preview" class="active" aria-pressed="true">Preview</button><button type="button" data-mdview="diff" aria-pressed="false">Diff</button></span>"#
  } else {
    ""
  };
  let path = f.display_path();
  let anchor_path = f.anchor_path();
  let prev = if index > 0 {
    format!(
      r##"<button type="button" class="file-prev" data-target="file-{}" aria-label="Previous file">↑</button>"##,
      index - 1
    )
  } else {
    String::new()
  };
  let next = if index + 1 < nfiles {
    format!(
      r##"<button type="button" class="file-next" data-target="file-{}" aria-label="Next file">↓</button>"##,
      index + 1
    )
  } else {
    String::new()
  };

  let body = if f.binary {
    r#"<p class="muted">Binary file — contents not shown.</p>"#.to_string()
  } else if f.hunks.is_empty() {
    r#"<p class="muted">No textual changes (mode/rename only).</p>"#.to_string()
  } else {
    let anchor = esc(anchor_path);
    let mut rows = String::new();
    for hunk in &f.hunks {
      rows.push_str(&format!(
        r#"<tr class="hunk"><td class="gutter"></td><td class="ln"></td><td class="ln"></td><td>{}</td></tr>"#,
        esc(&hunk.header)
      ));
      for line in &hunk.lines {
        let (class, side, line_no, old, new, sign, text) = match line {
          Line::Add { new, text } => ("add", "New", *new, String::new(), new.to_string(), '+', text),
          Line::Del { old, text } => ("del", "Old", *old, old.to_string(), String::new(), '-', text),
          Line::Ctx { old, new, text } => ("ctx", "New", *new, old.to_string(), new.to_string(), ' ', text),
          Line::Meta { text } => {
            rows.push_str(&format!(
              r#"<tr class="meta-line"><td class="gutter"></td><td class="ln"></td><td class="ln"></td><td>{}</td></tr>"#,
              esc(text)
            ));
            continue;
          }
        };
        rows.push_str(&format!(
          r#"<tr class="{class} commentable" data-file="{anchor}" data-side="{side}" data-line="{line_no}">{gutter}<td class="ln">{old}</td><td class="ln">{new}</td><td class="code">{sign}{}</td></tr>"#,
          esc(text),
          gutter = gutter_cell(&anchor, side, line_no),
        ));
      }
    }
    // Wrapped so the page JS can build a side-by-side `table.diff.split`
    // sibling and swap which one shows; the Preview | Diff pill swaps the
    // whole wrap against the preview. Markdown files open in Preview, so
    // their wrap starts hidden.
    let table = format!(
      r#"<div class="diff-wrap"{hidden}><div class="diff-scroll"><table class="diff unified" aria-label="Diff for {path}"><thead class="visually-hidden"><tr><th>Comment</th><th>Old</th><th>New</th><th>Code</th></tr></thead>{rows}</table></div></div>"#,
      hidden = if renderable_markdown { " hidden" } else { "" },
      path = esc(&path),
      rows = rows,
    );
    if renderable_markdown {
      format!("{table}{}", render_markdown_preview(f))
    } else {
      table
    }
  };

  format!(
    r##"<details class="file" id="file-{index}" open data-anchor="{anchor}" data-path="{path}" data-status="{letter}" data-adds="{adds}" data-dels="{dels}">
<summary class="file-summary">
<span class="file-left">{badge}<span class="path" title="{path}">{path}</span></span>
<span class="file-right">
{toggle}
<span class="file-comment-count" data-role="file-comment-count" hidden></span>
<span class="stats"><span class="adds">+{adds}</span> <span class="dels">−{dels}</span></span>
<label class="viewed-label"><input type="checkbox" class="file-viewed" data-anchor="{anchor}" aria-label="Mark {path} as viewed"> Viewed</label>
<button type="button" class="icon-btn copy-path" data-path="{path}" title="Copy path" aria-label="Copy path {path}">⎘</button>
<span class="file-nav">{prev}{next}</span>
</span>
</summary>
{notes}{body}
</details>"##,
    index = index,
    anchor = esc(anchor_path),
    path = esc(&path),
    letter = status_letter(f.status),
    badge = badge,
    toggle = toggle,
    adds = f.additions,
    dels = f.deletions,
    prev = prev,
    next = next,
    notes = notes,
    body = body,
  )
}

pub fn render_page(doc: &DiffDocument, title: Option<&str>, wasm_bytes: &[u8]) -> String {
  let page_title =
    title.map(str::to_string).unwrap_or_else(|| format!("{}: {} → {}", doc.repo, doc.base.name, doc.head.name));
  let config = serde_json::json!({
      "tool": doc.tool,
      "schema_version": doc.schema_version,
      "repo": doc.repo,
      "base": doc.base,
      "head": doc.head,
      "merge_base": doc.merge_base,
      "generated_at": doc.generated_at,
  });
  // Every `<` in embedded JSON becomes the \u003c escape (identical after
  // JSON.parse): the data can never form `</script>` or any tag in the page.
  let config_json = config.to_string().replace('<', "\\u003c");
  let nfiles = doc.files.len();
  let files_html: String = if doc.files.is_empty() {
    r#"<p class="muted">No changes between these refs.</p>"#.to_string()
  } else {
    doc.files.iter().enumerate().map(|(i, f)| render_file(i, f, nfiles)).collect()
  };
  let file_list_html = render_file_list(&doc.files);
  let sidebar_rows = render_sidebar_rows(&doc.files);
  let short = |sha: &str| sha[..sha.len().min(12)].to_string();
  let tool_label = format!("{} v{}", doc.tool, env!("CARGO_PKG_VERSION"));
  let ncommits = doc.commits.len();
  let nfiles_count = doc.files.len();
  let adds = doc.additions();
  let dels = doc.deletions();

  let mut page = String::with_capacity(64 * 1024 + wasm_bytes.len() * 4 / 3);
  page.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
  page.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
  // Inline SVG favicon — a `❯` prompt chevron on a dark rounded tile, as a `data:` URI so the
  // page stays a single self-contained file with no external requests.
  page.push_str("<link rel=\"icon\" href=\"data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Crect width='16' height='16' rx='3' fill='%23181a20'/%3E%3Ctext x='3.5' y='12.5' font-size='11' fill='%234493f8'%3E%E2%9D%AF%3C/text%3E%3C/svg%3E\">\n");
  page.push_str(&format!("<title>{}</title>\n", esc(&page_title)));
  page.push_str("<style>");
  page.push_str(CSS);
  // Visually-hidden utility used by table headers for a11y.
  page.push_str(
    "\n.visually-hidden{position:absolute!important;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0;}\n",
  );
  page.push_str("</style>\n</head>\n<body>\n");

  let desc_link =
    if doc.description.is_some() { r##"<a class="chrome-link" href="#description">Description</a>"## } else { "" };

  page.push_str(&format!(
    r##"<nav id="topnav" class="app-chrome" aria-label="Review chrome">
<button type="button" id="sidebar-toggle" class="icon-btn" aria-expanded="true" aria-controls="sidebar" title="Toggle file list" aria-label="Toggle file list">☰</button>
<div class="chrome-identity" title="{title}">
<span class="repo">{repo}</span>
<code>{base}</code>
<span class="range-arrow">→</span>
<code>{head}</code>
</div>
<div class="chrome-stats">
<span><strong id="nav-files">{nfiles}</strong> files</span>
<span class="chrome-delta" id="nav-diff"><span class="adds">+{adds}</span> <span class="dels">−{dels}</span></span>
<span class="muted" id="nav-commits" hidden>{ncommits}</span>
</div>
<div class="chrome-actions">
{desc_link}
<a class="chrome-link" href="#commits">Commits</a>
<a class="chrome-link" href="#diff">Diff</a>
<button type="button" id="comment-count" class="review-summary" aria-haspopup="dialog" aria-controls="summary-drawer">Review · 0 comments</button>
<details class="menu" id="actions-menu">
<summary aria-label="Review actions">Actions</summary>
<div class="menu-panel" role="menu">
<h3>Export / transfer</h3>
<button type="button" id="copy-md" role="menuitem">Copy Markdown</button>
<button type="button" id="export-json" role="menuitem">Export JSON</button>
<button type="button" id="export-md" role="menuitem">Export Markdown</button>
<button type="button" id="export-csv" role="menuitem">Export CSV</button>
<label class="btn" role="menuitem">Import JSON<input id="import-json" type="file" accept="application/json,.json" hidden></label>
<h3>View</h3>
<button type="button" id="view-toggle" role="menuitem" disabled aria-pressed="false">Side-by-side</button>
<button type="button" id="wrap-toggle" role="menuitem" aria-pressed="false">Wrap long lines</button>
<button type="button" id="theme-system" role="menuitem">Theme: System</button>
<button type="button" id="theme-light" role="menuitem">Theme: Light</button>
<button type="button" id="theme-dark" role="menuitem">Theme: Dark</button>
<button type="button" id="help-open" role="menuitem">Keyboard shortcuts (?)</button>
</div>
</details>
<details class="menu" id="details-menu">
<summary>Details</summary>
<div class="menu-panel left">
<div class="meta-row"><span>Repository</span><span>{repo}</span></div>
<div class="meta-row"><span>Base</span><code>{base} ({base_sha})</code></div>
<div class="meta-row"><span>Head</span><code>{head} ({head_sha})</code></div>
<div class="meta-row"><span>Merge-base</span><code>{mb}</code></div>
<div class="meta-row"><span>Generated</span><span>{gen}</span></div>
<div class="meta-row"><span>Tool</span><span>{tool}</span></div>
<div class="meta-row"><span>Schema</span><span>v{schema}</span></div>
</div>
</details>
</nav>
<div id="alerts">
<div id="storage-warning" class="alert" hidden role="alert">localStorage unavailable — comments last only for this page view; export before closing.</div>
<div id="wasm-error" class="alert" hidden role="alert"></div>
</div>
<div id="toast" hidden role="status" aria-live="polite"></div>
"##,
    title = esc(&page_title),
    repo = esc(&doc.repo),
    base = esc(&doc.base.name),
    base_sha = esc(&short(&doc.base.sha)),
    head = esc(&doc.head.name),
    head_sha = esc(&short(&doc.head.sha)),
    mb = esc(&short(&doc.merge_base)),
    gen = esc(&doc.generated_at),
    tool = esc(&tool_label),
    schema = doc.schema_version,
    desc_link = desc_link,
    ncommits = ncommits,
    nfiles = nfiles_count,
    adds = adds,
    dels = dels,
  ));

  page.push_str(r#"<div class="workspace">"#);
  page.push_str(
    r#"<div id="sidebar-backdrop" hidden></div>
<aside id="sidebar" aria-label="Files changed">
<div class="sidebar-head">
<span>Files</span>
<button type="button" id="sidebar-close" class="icon-btn" aria-label="Close file list">✕</button>
</div>
<div class="sidebar-tools">
<label class="visually-hidden" for="file-search">Search files</label>
<input type="search" id="file-search" placeholder="Search files…" autocomplete="off">
<div class="sidebar-filters" role="group" aria-label="File filters">
<button type="button" data-filter="all" aria-pressed="true">All</button>
<button type="button" data-filter="M" aria-pressed="false">Modified</button>
<button type="button" data-filter="A" aria-pressed="false">Added</button>
<button type="button" data-filter="D" aria-pressed="false">Deleted</button>
<button type="button" data-filter="R" aria-pressed="false">Renamed</button>
<button type="button" data-filter="comments" aria-pressed="false">Comments</button>
<button type="button" data-filter="unviewed" aria-pressed="false">Unviewed</button>
</div>
</div>
<div id="sidebar-list">
"#,
  );
  page.push_str(&sidebar_rows);
  page.push_str(
    r#"</div>
<div class="sidebar-foot">
<div id="viewed-progress">0 of 0 files viewed</div>
<div class="foot-actions">
<button type="button" id="next-unviewed">Next unviewed</button>
<button type="button" id="hide-viewed" aria-pressed="false">Hide viewed</button>
</div>
</div>
</aside>
"#,
  );

  page.push_str(r#"<main id="content">"#);
  page.push_str(
    r#"<div id="unanchored" hidden><strong>Unanchored comments</strong> <span class="muted">(their diff lines are not in this rendering — e.g. regenerated with different context)</span></div>
"#,
  );
  if let Some(d) = &doc.description {
    page.push_str(
      r#"<section id="description"><h2 class="desc-heading">Description <span class="seg md-seg desc-seg" role="group" aria-label="Description view"><button type="button" data-mdview="preview" class="active" aria-pressed="true">Preview</button><button type="button" data-mdview="raw" aria-pressed="false">Raw</button></span></h2>"#,
    );
    page.push_str(&render_description(d));
    page.push_str("</section>\n");
  }
  page.push_str("<section id=\"commits\" class=\"collapsible-meta\"><h2>Commits</h2>");
  page.push_str(&render_commits(doc));
  page.push_str("</section>\n");
  // Fallback file index for no-JS / print; interactive nav is the sidebar.
  page.push_str("<section id=\"files\" class=\"fallback-files\"><h2>Files changed</h2>");
  page.push_str("<div id=\"filelist-full\">");
  page.push_str(&file_list_html);
  page.push_str("</div><div id=\"filelist-range\" hidden></div>");
  page.push_str("</section>\n");
  page.push_str("<section id=\"diff\"><h2 class=\"visually-hidden\">Diff</h2>");
  page.push_str("<div id=\"files-full\">");
  page.push_str(&files_html);
  page.push_str("</div><div id=\"files-range\" hidden></div>");
  page.push_str("</section>\n</main>\n</div>\n");

  // Review summary drawer
  page.push_str(
    r#"<div id="summary-backdrop" hidden></div>
<aside id="summary-drawer" hidden role="dialog" aria-modal="true" aria-labelledby="summary-title">
<div class="summary-head">
<h2 id="summary-title">Review summary</h2>
<button type="button" id="summary-close" class="icon-btn" aria-label="Close review summary">✕</button>
</div>
<div class="summary-body" id="summary-body"></div>
<div class="summary-foot">
<button type="button" id="summary-copy-md" class="primary">Copy Markdown</button>
</div>
</aside>
"#,
  );

  // Keyboard help
  page.push_str(
    r#"<div id="help-dialog" hidden role="dialog" aria-modal="true" aria-labelledby="help-title">
<div class="help-card">
<h2 id="help-title">Keyboard shortcuts</h2>
<dl>
<dt>j / k</dt><dd>Next / previous file</dd>
<dt>n / p</dt><dd>Next / previous comment</dd>
<dt>f</dt><dd>Focus file search</dd>
<dt>?</dt><dd>Show this help</dd>
<dt>Esc</dt><dd>Close dialogs / cancel editor</dd>
<dt>Ctrl/⌘+Enter</dt><dd>Save comment</dd>
</dl>
<button type="button" class="help-close primary" id="help-close">Close</button>
</div>
</div>
"#,
  );

  page.push_str(&format!("<script type=\"application/json\" id=\"packdiff-config\">{config_json}</script>\n"));
  if let Some(snap) = &doc.snapshots {
    let snap_json = serde_json::to_string(snap)
      .expect("RangeSnapshots serializes: string keys only, no fallible types")
      .replace('<', "\\u003c");
    page.push_str(&format!("<script type=\"application/json\" id=\"packdiff-snapshots\">{snap_json}</script>\n"));
  }
  page.push_str(&format!(
    "<script type=\"application/wasm-base64\" id=\"packdiff-wasm\">{}</script>\n",
    base64(wasm_bytes)
  ));
  page.push_str("<script>");
  page.push_str(JS);
  page.push_str("</script>\n</body>\n</html>\n");
  page
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn escapes_html() {
    assert_eq!(esc("<script>&\"'"), "&lt;script&gt;&amp;&quot;&#x27;");
  }

  #[test]
  fn base64_known_vectors() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64(b"\0asm"), "AGFzbQ==");
  }

  #[test]
  fn assets_are_embedded() {
    assert!(CSS.contains("--accent"), "page.css must be embedded");
    assert!(JS.contains("pd_storage_key") || JS.contains("'use strict'"), "page.js must be embedded");
  }
}
