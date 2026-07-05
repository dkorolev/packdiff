//! HTML page assembly. Presentation only: consumes `packdiff_dto` types,
//! escapes everything, inlines CSS + JS + the base64 wasm module. The page it
//! emits makes zero network requests and works from `file://`.

use packdiff_dto::diff::{DiffDocument, FileDiff, FileStatus, Line};
use packdiff_dto::markdown;

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
      r#"<div class="hint">Click a commit to see only its diff; click a second commit to select the range between them (or shift-click to extend). Click again to reset. Commenting works on the full diff only.</div>"#,
    );
  }
  out.push_str(r#"<table class="commits">"#);
  for (i, c) in doc.commits.iter().enumerate() {
    out.push_str(&format!(
            r#"<tr class="commit{selectable}" data-index="{index}" data-sha="{sha}"><td class="sha"><code>{short}</code><button class="copy-sha" type="button" title="Copy the full commit hash">copy</button></td><td class="subject">{subject}</td><td class="author">{author}</td><td class="date">{date}</td></tr>"#,
            index = i + 1,
            sha = esc(&c.sha),
            short = esc(&c.short),
            subject = esc(&c.subject),
            author = esc(&c.author),
            date = esc(&c.date),
        ));
  }
  out.push_str("</table>");
  if doc.snapshots.is_some() {
    out.push_str(
      r#"<div id="range-bar" hidden><span id="range-label"></span> <button id="range-reset" type="button">Show full diff</button></div>"#,
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

/// The rendered-markdown view of a file's diff: consecutive runs of
/// context / added / removed lines render separately, added runs tinted
/// green and removed runs red. Modified files show hunk EXCERPTS with gap
/// markers; added files show the whole file. Multi-line markdown constructs
/// that straddle a change boundary render as separate blocks — inherent to
/// rendering a diff, not a bug.
fn render_markdown_preview(f: &FileDiff) -> String {
  #[derive(PartialEq, Clone, Copy)]
  enum Run {
    Ctx,
    Add,
    Del,
  }
  let mut chunks: Vec<String> = Vec::new();
  for (i, hunk) in f.hunks.iter().enumerate() {
    if i > 0 {
      chunks.push(r#"<div class="md-gap">⋯</div>"#.to_string());
    }
    let mut runs: Vec<(Run, Vec<&str>)> = Vec::new();
    for line in &hunk.lines {
      let (kind, text) = match line {
        Line::Ctx { text, .. } => (Run::Ctx, text.as_str()),
        Line::Add { text, .. } => (Run::Add, text.as_str()),
        Line::Del { text, .. } => (Run::Del, text.as_str()),
        Line::Meta { .. } => continue,
      };
      match runs.last_mut() {
        Some((k, lines)) if *k == kind => lines.push(text),
        _ => runs.push((kind, vec![text])),
      }
    }
    for (kind, lines) in runs {
      let html = markdown::to_html(&lines.join("\n"));
      if html.is_empty() {
        continue;
      }
      let class = match kind {
        Run::Ctx => "md-run",
        Run::Add => "md-run md-run-add",
        Run::Del => "md-run md-run-del",
      };
      chunks.push(format!(r#"<div class="{class}">{html}</div>"#));
    }
  }
  let note = if f.status == FileStatus::Added {
    ""
  } else {
    r#"<div class="muted note">Rendered from the diff hunks — green added, red removed; unchanged text outside the hunks is not included.</div>"#
  };
  format!(r#"<div class="md-preview" hidden>{note}{}</div>"#, chunks.join(""))
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

/// The "Files changed" index: one clickable row per file, linking to the
/// file's diff panel (`#file-N`) further down the page.
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

fn render_file(index: usize, f: &FileDiff) -> String {
  let badge = status_badge(f.status);
  let notes: String = f.notes.iter().map(|n| format!(r#"<div class="muted note">{}</div>"#, esc(n))).collect();
  let renderable_markdown =
    is_markdown_path(f.anchor_path()) && !f.binary && f.status != FileStatus::Deleted && !f.hunks.is_empty();
  let toggle =
    if renderable_markdown { r#"<button class="md-toggle" type="button">View rendered</button>"# } else { "" };
  let body = if f.binary {
    r#"<p class="muted">Binary file — contents not shown.</p>"#.to_string()
  } else if f.hunks.is_empty() {
    r#"<p class="muted">No textual changes (mode/rename only).</p>"#.to_string()
  } else {
    let anchor = esc(f.anchor_path());
    let mut rows = String::new();
    for hunk in &f.hunks {
      rows.push_str(&format!(
        r#"<tr class="hunk"><td class="ln"></td><td class="ln"></td><td>{}</td></tr>"#,
        esc(&hunk.header)
      ));
      for line in &hunk.lines {
        let (class, side, line_no, old, new, sign, text) = match line {
          Line::Add { new, text } => ("add", "New", *new, String::new(), new.to_string(), '+', text),
          Line::Del { old, text } => ("del", "Old", *old, old.to_string(), String::new(), '-', text),
          Line::Ctx { old, new, text } => ("ctx", "New", *new, old.to_string(), new.to_string(), ' ', text),
          Line::Meta { text } => {
            rows.push_str(&format!(
              r#"<tr class="meta-line"><td class="ln"></td><td class="ln"></td><td>{}</td></tr>"#,
              esc(text)
            ));
            continue;
          }
        };
        rows.push_str(&format!(
                    r#"<tr class="{class} commentable" data-file="{anchor}" data-side="{side}" data-line="{line_no}"><td class="ln">{old}</td><td class="ln">{new}</td><td class="code">{sign}{}</td></tr>"#,
                    esc(text)
                ));
      }
    }
    // Wrapped so the page JS can build a side-by-side `table.diff.split`
    // sibling and swap which one shows; the markdown toggle hides the wrap.
    let table = format!(r#"<div class="diff-wrap"><table class="diff unified">{rows}</table></div>"#);
    if renderable_markdown {
      format!("{table}{}", render_markdown_preview(f))
    } else {
      table
    }
  };
  format!(
    r#"<details class="file" id="file-{index}" open><summary>{badge}<span class="path">{}</span> <span class="stats">{toggle}<span class="adds">+{}</span> <span class="dels">−{}</span></span></summary>{notes}{body}</details>"#,
    esc(&f.display_path()),
    f.additions,
    f.deletions,
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
  let files_html: String = if doc.files.is_empty() {
    r#"<p class="muted">No changes between these refs.</p>"#.to_string()
  } else {
    doc.files.iter().enumerate().map(|(i, f)| render_file(i, f)).collect()
  };
  let file_list_html = render_file_list(&doc.files);
  let short = |sha: &str| sha[..sha.len().min(12)].to_string();

  let mut page = String::with_capacity(64 * 1024 + wasm_bytes.len() * 4 / 3);
  page.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
  page.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
  page.push_str(&format!("<title>{}</title>\n", esc(&page_title)));
  page.push_str("<style>");
  page.push_str(CSS);
  page.push_str("</style>\n</head>\n<body>\n");
  page.push_str(&format!(
        r##"<header class="top">
<h1>{title}</h1>
<div class="range"><code>{base}</code> ({base_sha}) → <code>{head}</code> ({head_sha}) <span class="muted">· merge-base <code>{mb}</code> · generated {gen} · {tool}</span></div>
<div class="hint">{ncommits} commit(s) · {nfiles} file(s) changed · <span class="adds">+{adds}</span> <span class="dels">−{dels}</span> · click any diff line to comment (markdown supported) — comments stay in this browser (localStorage) until you export them.</div>
<div class="toolbar">
<span id="comment-count">0 comments</span>
<button id="export-json" type="button">Export JSON</button>
<button id="export-md" type="button">Export Markdown</button>
<button id="export-csv" type="button">Export CSV</button>
<button id="copy-md" type="button">Copy Markdown</button>
<label class="btn">Import JSON<input id="import-json" type="file" accept="application/json,.json" hidden></label>
<span id="storage-warning">localStorage unavailable — comments last only for this page view; export before closing.</span>
<span id="wasm-error"></span>
</div>
</header>
<nav id="topnav">
<a href="#commits">Commits <span class="count" id="nav-commits">{ncommits}</span></a>
<a href="#files">Files changed <span class="count" id="nav-files">{nfiles}</span></a>
<a href="#diff">Diff <span class="count" id="nav-diff"><span class="adds">+{adds}</span> <span class="dels">−{dels}</span></span></a>
<span class="nav-spacer"></span>
<button id="view-toggle" type="button" disabled>Side-by-side</button>
</nav>
"##,
        title = esc(&page_title),
        base = esc(&doc.base.name),
        base_sha = esc(&short(&doc.base.sha)),
        head = esc(&doc.head.name),
        head_sha = esc(&short(&doc.head.sha)),
        mb = esc(&short(&doc.merge_base)),
        gen = esc(&doc.generated_at),
        tool = esc(&format!("{} v{}", doc.tool, env!("CARGO_PKG_VERSION"))),
        ncommits = doc.commits.len(),
        nfiles = doc.files.len(),
        adds = doc.additions(),
        dels = doc.deletions(),
    ));
  page.push_str("<main>\n");
  page.push_str(
        r#"<div id="unanchored" style="display:none"><strong>Unanchored comments</strong> <span class="muted">(their diff lines are not in this rendering — e.g. regenerated with different context)</span></div>
"#,
    );
  page.push_str("<section id=\"commits\"><h2>Commits</h2>");
  page.push_str(&render_commits(doc));
  page.push_str("</section>\n");
  page.push_str("<section id=\"files\"><h2>Files changed</h2>");
  page.push_str("<div id=\"filelist-full\">");
  page.push_str(&file_list_html);
  page.push_str("</div><div id=\"filelist-range\" hidden></div>");
  page.push_str("</section>\n");
  page.push_str("<section id=\"diff\"><h2>Diff</h2>");
  page.push_str("<div id=\"files-full\">");
  page.push_str(&files_html);
  page.push_str("</div><div id=\"files-range\" hidden></div>");
  page.push_str("</section>\n</main>\n");
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

const CSS: &str = r##"
:root { --bg:#ffffff; --fg:#1f2328; --muted:#59636e; --border:#d1d9e0;
  --panel:#f6f8fa; --add-bg:#dafbe1; --del-bg:#ffebe9; --accent:#0969da;
  --comment-bg:#fff8c5; --comment-border:#d4a72c; }
@media (prefers-color-scheme: dark) {
  :root { --bg:#0e1116; --fg:#e6edf3; --muted:#9198a1; --border:#3d444d;
    --panel:#161b22; --add-bg:#12261e; --del-bg:#25171c; --accent:#4493f8;
    --comment-bg:#2a2410; --comment-border:#7a6520; }
}
* { box-sizing: border-box; }
body { margin:0; background:var(--bg); color:var(--fg);
  font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif; }
main { max-width:1100px; margin:0 auto; padding:0 16px 64px; }
a { color:var(--accent); text-decoration:none; }
code { font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; }
.muted { color:var(--muted); }
:root { --nav-h:39px; }
header.top { border-bottom:1px solid var(--border); background:var(--panel); padding:16px; }
header.top h1 { margin:0 0 4px; font-size:18px; }
nav#topnav { position:sticky; top:0; z-index:20; display:flex; align-items:center; gap:4px;
  background:var(--panel); border-bottom:1px solid var(--border); padding:6px 16px; }
nav#topnav a { color:var(--fg); padding:3px 10px; border-radius:6px; font-size:13px; font-weight:600; }
nav#topnav a:hover { background:var(--bg); }
nav#topnav a.active { background:var(--bg); box-shadow:inset 0 -2px 0 var(--accent); }
nav#topnav .count { color:var(--muted); font-weight:400; margin-left:2px; }
nav#topnav .nav-spacer { flex:1; }
nav#topnav button { background:var(--bg); color:var(--fg); border:1px solid var(--border);
  border-radius:6px; padding:3px 10px; font-size:13px; cursor:pointer; }
nav#topnav button:hover:not(:disabled) { border-color:var(--accent); }
nav#topnav button:disabled { opacity:.5; cursor:not-allowed; }
section[id] { scroll-margin-top:var(--nav-h); }
details.file { scroll-margin-top:calc(var(--nav-h) + 4px); }
table.filelist { width:100%; border-collapse:collapse; }
table.filelist td { padding:3px 8px; border-bottom:1px solid var(--border); vertical-align:top; }
table.filelist td.fl-path { font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; font-size:13px; }
table.filelist td.fl-stats { white-space:nowrap; text-align:right; width:1%; }
header.top .range code { background:var(--bg); border:1px solid var(--border);
  border-radius:6px; padding:1px 6px; }
.toolbar { margin-top:10px; display:flex; gap:8px; flex-wrap:wrap; align-items:center; }
.toolbar button, .toolbar label.btn { background:var(--bg); color:var(--fg);
  border:1px solid var(--border); border-radius:6px; padding:4px 10px;
  font-size:13px; cursor:pointer; }
.toolbar button:hover, .toolbar label.btn:hover { border-color:var(--accent); }
#comment-count { font-weight:600; }
section { margin-top:24px; }
section > h2 { font-size:16px; border-bottom:1px solid var(--border); padding-bottom:6px; }
table.commits { width:100%; border-collapse:collapse; }
table.commits td { padding:4px 8px; border-bottom:1px solid var(--border); vertical-align:top; }
table.commits td.sha { white-space:nowrap; }
table.commits td.date, table.commits td.author { white-space:nowrap; color:var(--muted); }
tr.commit.selectable { cursor:pointer; }
tr.commit.selectable:hover td { background:var(--panel); }
tr.commit.selected td { background:var(--add-bg); }
button.copy-sha { margin-left:8px; background:var(--bg); color:var(--muted);
  border:1px solid var(--border); border-radius:6px; padding:0 6px;
  font-size:10px; cursor:pointer; }
button.copy-sha:hover { border-color:var(--accent); color:var(--fg); }
#range-bar { border:1px solid var(--border); border-radius:8px; background:var(--panel);
  padding:8px 12px; margin:10px 0; display:flex; gap:10px; align-items:center; flex-wrap:wrap; }
#range-bar[hidden] { display:none; }
#range-bar button { background:var(--bg); color:var(--fg); border:1px solid var(--border);
  border-radius:6px; padding:2px 10px; font-size:12px; cursor:pointer; }
#range-bar button:hover { border-color:var(--accent); }
#range-label { font-weight:600; }
details.file { border:1px solid var(--border); border-radius:8px; margin:12px 0; }
details.file > summary { cursor:pointer; padding:8px 12px; background:var(--panel);
  font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; font-size:13px;
  position:sticky; top:var(--nav-h); z-index:5; border-radius:8px 8px 0 0;
  border-bottom:1px solid var(--border); }
details.file:not([open]) > summary { border-bottom:none; border-radius:8px; }
details.file .stats { float:right; }
.adds { color:#1a7f37; } .dels { color:#cf222e; }
@media (prefers-color-scheme: dark) { .adds{color:#3fb950;} .dels{color:#f85149;} }
.badge { border:1px solid var(--border); border-radius:10px; padding:0 8px; font-size:11px; }
.badge-added { color:#1a7f37; } .badge-deleted { color:#cf222e; } .badge-renamed { color:var(--accent); }
.note { padding:2px 12px; font-size:12px; }
table.diff { width:100%; border-collapse:collapse;
  font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; font-size:12px; }
table.diff td { padding:0 8px; white-space:pre-wrap; word-break:break-all; vertical-align:top; }
table.diff td.ln { width:1%; min-width:36px; text-align:right; color:var(--muted);
  user-select:none; border-right:1px solid var(--border);
  white-space:nowrap; word-break:normal; }
tr.add td { background:var(--add-bg); }
tr.del td { background:var(--del-bg); }
tr.hunk td, tr.meta-line td { background:var(--panel); color:var(--muted); padding:2px 8px; }
tr.commentable td.code { cursor:pointer; }
tr.commentable:hover td.code { outline:1px dashed var(--accent); outline-offset:-1px; }
/* side-by-side: four columns (old ln, old code, new ln, new code) */
table.diff.split { table-layout:fixed; }
table.diff.split td.code { width:calc(50% - 40px); }
table.diff.split td:nth-child(2) { border-right:1px solid var(--border); }
table.diff.split td.code.add, table.diff.split td.ln.add { background:var(--add-bg); }
table.diff.split td.code.del, table.diff.split td.ln.del { background:var(--del-bg); }
table.diff.split td.empty { background:var(--panel); }
table.diff.split td.code.commentable { cursor:pointer; }
table.diff.split td.code.commentable:hover { outline:1px dashed var(--accent); outline-offset:-1px; }
tr.comment-row td, tr.editor-row td { background:var(--comment-bg);
  border-left:3px solid var(--comment-border); padding:8px 12px;
  font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; font-size:13px; }
.comment-body { margin:4px 0; }
.comment-body > :first-child { margin-top:0; }
.comment-body > :last-child { margin-bottom:0; }
.comment-meta { color:var(--muted); font-size:11px; }
.comment-actions button, .editor-row button { margin-right:6px; margin-top:4px;
  background:var(--bg); color:var(--fg); border:1px solid var(--border);
  border-radius:6px; padding:2px 8px; font-size:12px; cursor:pointer; }
.editor-row textarea { width:100%; min-height:70px; background:var(--bg); color:var(--fg);
  border:1px solid var(--border); border-radius:6px; padding:6px;
  font:13px/1.4 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; }
#unanchored { border:1px dashed var(--comment-border); border-radius:8px;
  padding:8px 12px; margin:12px 0; background:var(--comment-bg); }
button.md-toggle { background:var(--bg); color:var(--fg); border:1px solid var(--border);
  border-radius:6px; padding:1px 8px; margin-right:12px; font-size:11px; cursor:pointer;
  font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; }
button.md-toggle:hover { border-color:var(--accent); }
.md-preview { padding:4px 16px 12px;
  font:14px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",Helvetica,Arial,sans-serif; }
.md-gap { color:var(--muted); text-align:center; border-top:1px dashed var(--border);
  margin:12px 0; padding-top:4px; user-select:none; }
.md-run-add { background:var(--add-bg); border-left:3px solid #1a7f37;
  padding:2px 10px; margin:4px 0; border-radius:4px; }
.md-run-del { background:var(--del-bg); border-left:3px solid #cf222e;
  padding:2px 10px; margin:4px 0; border-radius:4px; }
@media (prefers-color-scheme: dark) {
  .md-run-add { border-left-color:#3fb950; }
  .md-run-del { border-left-color:#f85149; }
}
.md-preview pre, .comment-body pre { background:var(--panel); border:1px solid var(--border);
  border-radius:6px; padding:8px; overflow-x:auto; }
.md-preview code, .comment-body code { background:var(--panel);
  border-radius:4px; padding:1px 4px; font-size:0.9em; }
.md-preview pre code, .comment-body pre code { background:none; padding:0; font-size:12px; }
.md-preview blockquote, .comment-body blockquote { border-left:3px solid var(--border);
  margin:6px 0; padding:2px 12px; color:var(--muted); }
.md-preview h1, .md-preview h2 { border-bottom:1px solid var(--border); padding-bottom:4px; }
.md-preview p, .comment-body p { margin:6px 0; }
#storage-warning, #wasm-error { display:none; color:#cf222e; font-size:12px; }
.hint { font-size:12px; color:var(--muted); }
"##;

// The page's JavaScript is a VIEW LAYER ONLY. Every read of stored state goes
// through pd_parse_document and every mutation/export goes through the wasm
// build of packdiff-dto — JS never edits the review document itself.
const JS: &str = r##"
'use strict';
(async function () {
  const CONFIG = JSON.parse(document.getElementById('packdiff-config').textContent);
  const META = JSON.stringify({ repo: CONFIG.repo, base: CONFIG.base, head: CONFIG.head });

  // ---- wasm bridge ----
  const b64 = document.getElementById('packdiff-wasm').textContent.trim();
  const raw = atob(b64);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  let ex;
  try {
    const { instance } = await WebAssembly.instantiate(bytes, {});
    ex = instance.exports;
  } catch (e) {
    const el = document.getElementById('wasm-error');
    el.textContent = 'Comment engine failed to load: ' + e;
    el.style.display = 'inline';
    return;
  }
  const enc = new TextEncoder();
  const dec = new TextDecoder();
  function callWasm(name) {
    const inputs = Array.prototype.slice.call(arguments, 1);
    const args = [];
    const allocs = [];
    for (const s of inputs) {
      const b = enc.encode(s);
      const ptr = ex.pd_alloc(b.length);
      new Uint8Array(ex.memory.buffer, ptr, b.length).set(b);
      args.push(ptr, b.length);
      allocs.push([ptr, b.length]);
    }
    const packed = ex[name].apply(null, args);
    for (const [p, l] of allocs) ex.pd_free(p, l);
    const rptr = Number(packed >> 32n);
    const rlen = Number(packed & 0xffffffffn);
    const out = dec.decode(new Uint8Array(ex.memory.buffer, rptr, rlen).slice());
    ex.pd_free(rptr, rlen);
    const env = JSON.parse(out);
    if ('Ok' in env) return env.Ok;
    throw new Error(env.Error && env.Error.message ? env.Error.message : 'malformed engine response');
  }

  const KEY = callWasm('pd_storage_key', META);

  // ---- storage (localStorage with in-memory fallback) ----
  let doc = null;          // current ReviewDocument object (owned by wasm ops)
  let memoryOnly = false;
  function showStorageWarning() {
    const el = document.getElementById('storage-warning');
    if (el) el.style.display = 'inline';
    memoryOnly = true;
  }
  function initDoc() {
    let raw = null;
    try { raw = localStorage.getItem(KEY); } catch (e) { showStorageWarning(); }
    if (raw !== null) {
      try { return callWasm('pd_parse_document', raw); }
      catch (e) { console.warn('packdiff: stored document rejected (' + e.message + '); starting fresh'); }
    }
    return callWasm('pd_new_document', META);
  }
  function saveDoc(next) {
    doc = next;
    if (!memoryOnly) {
      try { localStorage.setItem(KEY, JSON.stringify(doc)); }
      catch (e) { showStorageWarning(); }
    }
    renderAll();
  }
  function genId() {
    return 'c' + Date.now().toString(36) + Math.random().toString(36).slice(2, 8);
  }

  // ---- anchoring ----
  // A comment anchors to (file, side, line). In the unified view the anchor
  // lives on a whole `tr.commentable`; in the side-by-side view it lives on a
  // single `td.code.commentable` half-cell. rowFor resolves either to the
  // <tr> the comment card is inserted after, targeting whichever view is live.
  function cssAttr(v) { return String(v).replace(/\\/g, '\\\\').replace(/"/g, '\\"'); }
  function anchorSel(prefix, c) {
    return prefix + '[data-file="' + cssAttr(c.file) + '"]' +
      '[data-side="' + cssAttr(c.side) + '"][data-line="' + cssAttr(String(c.line)) + '"]';
  }
  function rowFor(c) {
    if (document.body.classList.contains('view-split')) {
      const cell = document.querySelector(anchorSel('td.code.commentable', c));
      return cell ? cell.parentElement : null;
    }
    return document.querySelector(anchorSel('tr.commentable', c));
  }

  // ---- rendering ----
  function renderAll() {
    document.querySelectorAll('tr.comment-row').forEach((el) => el.remove());
    const un = document.getElementById('unanchored');
    un.style.display = 'none';
    un.querySelectorAll('.comment-card').forEach((el) => el.remove());
    let unanchored = 0;
    for (const c of doc.comments) {
      const row = rowFor(c);
      if (row) insertCommentRow(row, c);
      else { unanchored += 1; un.appendChild(commentCard(c)); }
    }
    if (unanchored > 0) un.style.display = 'block';
    const n = doc.comments.length;
    document.getElementById('comment-count').textContent =
      n + ' comment' + (n === 1 ? '' : 's');
  }

  function commentCard(c) {
    const wrap = document.createElement('div');
    wrap.className = 'comment-card';
    const meta = document.createElement('div');
    meta.className = 'comment-meta';
    meta.textContent = c.file + ' · ' + c.side.toLowerCase() + ' line ' + c.line + ' · ' + c.updated_at;
    const body = document.createElement('div');
    body.className = 'comment-body';
    // Markdown rendering happens in the wasm model (pd_markdown_html), which
    // escapes ALL input; the assigned HTML contains only tags it emitted.
    try { body.innerHTML = callWasm('pd_markdown_html', c.text); }
    catch (e) { body.textContent = c.text; }
    const actions = document.createElement('div');
    actions.className = 'comment-actions';
    const edit = document.createElement('button');
    edit.textContent = 'Edit';
    edit.addEventListener('click', () => {
      const row = rowFor(c);
      if (row) openEditor(row, { file: c.file, side: c.side, line: c.line }, c);
    });
    const del = document.createElement('button');
    del.textContent = 'Delete';
    del.addEventListener('click', () => {
      saveDoc(callWasm('pd_delete_comment', JSON.stringify(doc), c.id));
    });
    actions.appendChild(edit);
    actions.appendChild(del);
    wrap.appendChild(meta);
    wrap.appendChild(body);
    wrap.appendChild(actions);
    return wrap;
  }

  function insertCommentRow(row, c) {
    const tr = document.createElement('tr');
    tr.className = 'comment-row';
    const td = document.createElement('td');
    // Span the anchor row's own column count (3 unified, 4 side-by-side).
    td.colSpan = row.cells ? row.cells.length : 3;
    td.appendChild(commentCard(c));
    tr.appendChild(td);
    let after = row;
    while (after.nextElementSibling &&
           after.nextElementSibling.classList.contains('comment-row')) {
      after = after.nextElementSibling;
    }
    after.after(tr);
  }

  // ---- editor ----
  function closeEditors() {
    document.querySelectorAll('tr.editor-row').forEach((el) => el.remove());
  }
  // openEditor(afterRow, anchor, existing): `anchor` is { file, side, line }
  // resolved from whichever view was clicked, so a new comment carries the
  // right coordinates regardless of unified vs side-by-side.
  function openEditor(afterRow, anchor, existing) {
    closeEditors();
    const tr = document.createElement('tr');
    tr.className = 'editor-row';
    const td = document.createElement('td');
    td.colSpan = afterRow.cells ? afterRow.cells.length : 3;
    const ta = document.createElement('textarea');
    ta.placeholder = 'Leave a comment… (markdown supported, Ctrl/Cmd+Enter to save)';
    if (existing) ta.value = existing.text;
    const save = document.createElement('button');
    save.textContent = 'Save';
    const cancel = document.createElement('button');
    cancel.textContent = 'Cancel';
    function doSave() {
      const text = ta.value.trim();
      if (!text) { closeEditors(); return; }
      const now = new Date().toISOString();
      const comment = existing
        ? Object.assign({}, existing, { text: text, updated_at: now })
        : { id: genId(), file: anchor.file, side: anchor.side,
            line: Number(anchor.line), text: text,
            created_at: now, updated_at: now };
      closeEditors();
      try {
        saveDoc(callWasm('pd_upsert_comment', JSON.stringify(doc), JSON.stringify(comment)));
      } catch (e) {
        alert('Comment rejected: ' + e.message);
      }
    }
    save.addEventListener('click', doSave);
    cancel.addEventListener('click', closeEditors);
    ta.addEventListener('keydown', (ev) => {
      if ((ev.ctrlKey || ev.metaKey) && ev.key === 'Enter') doSave();
      if (ev.key === 'Escape') closeEditors();
    });
    td.appendChild(ta);
    td.appendChild(save);
    td.appendChild(cancel);
    tr.appendChild(td);
    afterRow.after(tr);
    ta.focus();
  }

  // The anchor may sit on the clicked cell (side-by-side) or on its row
  // (unified). Returns { file, side, line } or null for a non-commentable spot.
  function anchorOf(cell) {
    if (cell.dataset.file) {
      return { file: cell.dataset.file, side: cell.dataset.side, line: cell.dataset.line };
    }
    const row = cell.parentElement;
    if (row.classList.contains('commentable')) {
      return { file: row.dataset.file, side: row.dataset.side, line: row.dataset.line };
    }
    return null;
  }

  document.addEventListener('click', (ev) => {
    const cell = ev.target.closest('td.code');
    if (!cell) return;
    const anchor = anchorOf(cell);
    if (!anchor) return;
    openEditor(cell.parentElement, anchor, null);
  });

  // ---- rendered-markdown toggle for .md files (pure view state) ----
  document.addEventListener('click', (ev) => {
    const btn = ev.target.closest('button.md-toggle');
    if (!btn) return;
    ev.preventDefault(); // the button lives inside <summary>: do not fold the panel
    const file = btn.closest('details.file');
    const wrap = file.querySelector('.diff-wrap');
    const preview = file.querySelector('.md-preview');
    if (!wrap || !preview) return;
    const showRendered = preview.hidden;
    preview.hidden = !showRendered;
    wrap.hidden = showRendered;
    btn.textContent = showRendered ? 'View diff' : 'View rendered';
  });

  // ---- commit range filtering ----
  // View state only: the sub-range diff itself is computed by the wasm model
  // (pd_range_diff) from the embedded snapshots. Boundary i is the state
  // after commit i (0 = merge base), so commits [from..to] diff as
  // pd_range_diff(from - 1, to). Comments are full-diff only by design.
  const snapTag = document.getElementById('packdiff-snapshots');
  const SNAPSHOTS = snapTag ? snapTag.textContent : null;
  let rangeFrom = null, rangeTo = null; // 1-based commit indices, inclusive

  document.addEventListener('click', (ev) => {
    const copy = ev.target.closest('button.copy-sha');
    if (copy) {
      copyText(copy.closest('tr').dataset.sha, copy);
      return;
    }
    const row = ev.target.closest('tr.commit.selectable');
    if (!row || !SNAPSHOTS) return;
    const idx = Number(row.dataset.index);
    const single = rangeFrom !== null && rangeFrom === rangeTo;
    if (ev.shiftKey && rangeFrom !== null) {
      // Shift-click extends the current selection's span to include idx.
      rangeFrom = Math.min(rangeFrom, idx);
      rangeTo = Math.max(rangeTo, idx);
    } else if (rangeFrom === idx && rangeTo === idx) {
      rangeFrom = rangeTo = null; // clicking the sole selected commit deselects
    } else if (single) {
      // A single commit is selected; a plain click on another spans the range.
      rangeFrom = Math.min(rangeFrom, idx);
      rangeTo = Math.max(rangeTo, idx);
    } else {
      rangeFrom = rangeTo = idx; // no selection, or a range was selected: (re)start at idx
    }
    applyRange();
  });
  const rangeReset = document.getElementById('range-reset');
  if (rangeReset) rangeReset.addEventListener('click', () => {
    rangeFrom = rangeTo = null;
    applyRange();
  });

  // Full-diff nav counts, captured once so resetting the range restores them.
  const FULL_COUNTS = {
    files: document.getElementById('nav-files').textContent,
    diff: document.getElementById('nav-diff').innerHTML,
  };
  function setNavCounts(filesText, diffHtml) {
    document.getElementById('nav-files').textContent = filesText;
    document.getElementById('nav-diff').innerHTML = diffHtml;
  }

  function applyRange() {
    const bar = document.getElementById('range-bar');
    const fullFiles = document.getElementById('files-full');
    const rangedFiles = document.getElementById('files-range');
    const fullList = document.getElementById('filelist-full');
    const rangedList = document.getElementById('filelist-range');
    document.querySelectorAll('tr.commit').forEach((tr) => {
      const i = Number(tr.dataset.index);
      tr.classList.toggle('selected', rangeFrom !== null && i >= rangeFrom && i <= rangeTo);
    });
    if (rangeFrom === null) {
      bar.hidden = true;
      rangedFiles.hidden = true;
      rangedFiles.textContent = '';
      rangedList.hidden = true;
      rangedList.textContent = '';
      fullFiles.style.display = '';
      fullList.style.display = '';
      setNavCounts(FULL_COUNTS.files, FULL_COUNTS.diff);
      return;
    }
    let files;
    try {
      files = callWasm('pd_range_diff', SNAPSHOTS,
        JSON.stringify({ from: rangeFrom - 1, to: rangeTo, context: 3 }));
    } catch (e) {
      alert('Range diff failed: ' + e.message);
      return;
    }
    rangedFiles.textContent = '';
    rangedList.textContent = '';
    if (files.length === 0) {
      const p = document.createElement('p');
      p.className = 'muted';
      p.textContent = 'No changes in the selected commits.';
      rangedFiles.appendChild(p);
      rangedList.appendChild(p.cloneNode(true));
    }
    let adds = 0, dels = 0;
    const list = document.createElement('table');
    list.className = 'filelist';
    files.forEach((f, i) => {
      rangedFiles.appendChild(rangeFileEl(f, i));
      list.appendChild(rangeFileListRow(f, i));
      adds += f.additions;
      dels += f.deletions;
    });
    if (files.length) rangedList.appendChild(list);
    setNavCounts(String(files.length),
      '<span class="adds">+' + adds + '</span> <span class="dels">−' + dels + '</span>');
    const shortOf = (i) =>
      document.querySelector('tr.commit[data-index="' + i + '"] code').textContent;
    const n = rangeTo - rangeFrom + 1;
    document.getElementById('range-label').textContent = n === 1
      ? 'Showing commit ' + shortOf(rangeFrom) + ' only'
      : 'Showing commits ' + shortOf(rangeFrom) + '..' + shortOf(rangeTo) + ' (' + n + ' commits)';
    bar.hidden = false;
    fullFiles.style.display = 'none';
    fullList.style.display = 'none';
    rangedFiles.hidden = false;
    rangedList.hidden = false;
    // Freshly built range panels are unified; re-apply the diff view so they
    // pick up side-by-side when it is active.
    if (document.body.classList.contains('view-split')) applyDiffView();
  }

  // One row of the range-mode file index, linking to the range file panel.
  function rangeFileListRow(f, i) {
    const tr = document.createElement('tr');
    const path = document.createElement('td');
    path.className = 'fl-path';
    if (f.status !== 'Modified') {
      const badge = document.createElement('span');
      badge.className = 'badge badge-' + f.status.toLowerCase();
      badge.textContent = f.status.toLowerCase();
      path.appendChild(badge);
      path.appendChild(document.createTextNode(' '));
    }
    const a = document.createElement('a');
    a.href = '#rfile-' + i;
    a.textContent = f.old_path && f.new_path && f.old_path !== f.new_path
      ? f.old_path + ' → ' + f.new_path : (f.new_path || f.old_path);
    path.appendChild(a);
    const stats = document.createElement('td');
    stats.className = 'fl-stats';
    const adds = document.createElement('span');
    adds.className = 'adds';
    adds.textContent = '+' + f.additions;
    const dels = document.createElement('span');
    dels.className = 'dels';
    dels.textContent = '−' + f.deletions;
    stats.appendChild(adds);
    stats.appendChild(document.createTextNode(' '));
    stats.appendChild(dels);
    tr.appendChild(path);
    tr.appendChild(stats);
    return tr;
  }

  // Build a file panel for a wasm-computed FileDiff. DOM-built (values land
  // via textContent, never markup), and NOT commentable — range rows carry
  // no anchors on purpose.
  function rangeFileEl(f, i) {
    const details = document.createElement('details');
    details.className = 'file';
    details.id = 'rfile-' + i;
    details.open = true;
    const summary = document.createElement('summary');
    if (f.status !== 'Modified') {
      const badge = document.createElement('span');
      badge.className = 'badge badge-' + f.status.toLowerCase();
      badge.textContent = f.status.toLowerCase();
      summary.appendChild(badge);
      summary.appendChild(document.createTextNode(' '));
    }
    const path = document.createElement('span');
    path.className = 'path';
    path.textContent = f.old_path && f.new_path && f.old_path !== f.new_path
      ? f.old_path + ' → ' + f.new_path : (f.new_path || f.old_path);
    summary.appendChild(path);
    const stats = document.createElement('span');
    stats.className = 'stats';
    const adds = document.createElement('span');
    adds.className = 'adds';
    adds.textContent = '+' + f.additions;
    const dels = document.createElement('span');
    dels.className = 'dels';
    dels.textContent = '−' + f.deletions;
    stats.appendChild(adds);
    stats.appendChild(document.createTextNode(' '));
    stats.appendChild(dels);
    summary.appendChild(document.createTextNode(' '));
    summary.appendChild(stats);
    details.appendChild(summary);
    if (f.binary) {
      const p = document.createElement('p');
      p.className = 'muted';
      p.textContent = 'Binary (or unsnapshotted oversized) file — contents not shown.';
      details.appendChild(p);
      return details;
    }
    // Wrapped and classed exactly like the build-time unified tables so the
    // side-by-side builder handles range panels identically (no anchors —
    // range diffs stay read-only).
    const wrap = document.createElement('div');
    wrap.className = 'diff-wrap';
    const table = document.createElement('table');
    table.className = 'diff unified';
    const cell = (cls, text) => {
      const td = document.createElement('td');
      if (cls) td.className = cls;
      td.textContent = text;
      return td;
    };
    for (const hunk of f.hunks) {
      const hr = document.createElement('tr');
      hr.className = 'hunk';
      hr.appendChild(cell('ln', ''));
      hr.appendChild(cell('ln', ''));
      hr.appendChild(cell('', hunk.header));
      table.appendChild(hr);
      for (const line of hunk.lines) {
        const kind = Object.keys(line)[0];
        const p = line[kind];
        const tr = document.createElement('tr');
        if (kind === 'Meta') {
          tr.className = 'meta-line';
          tr.appendChild(cell('ln', ''));
          tr.appendChild(cell('ln', ''));
          tr.appendChild(cell('', p.text));
        } else {
          tr.className = kind === 'Add' ? 'add' : (kind === 'Del' ? 'del' : 'ctx');
          const sign = kind === 'Add' ? '+' : (kind === 'Del' ? '-' : ' ');
          tr.appendChild(cell('ln', p.old !== undefined ? String(p.old) : ''));
          tr.appendChild(cell('ln', p.new !== undefined ? String(p.new) : ''));
          tr.appendChild(cell('code', sign + p.text));
        }
        table.appendChild(tr);
      }
    }
    wrap.appendChild(table);
    details.appendChild(wrap);
    return details;
  }

  // ---- exports (all produced by the wasm model, not by JS) ----
  function download(name, mime, text) {
    const a = document.createElement('a');
    a.href = URL.createObjectURL(new Blob([text], { type: mime }));
    a.download = name;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 5000);
  }
  function stem() {
    return 'packdiff-comments-' + CONFIG.repo + '-' +
      CONFIG.base.sha.slice(0, 7) + '-' + CONFIG.head.sha.slice(0, 7);
  }
  function copyText(text, btn) {
    const done = () => {
      const old = btn.textContent;
      btn.textContent = 'Copied!';
      setTimeout(() => { btn.textContent = old; }, 1200);
    };
    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(text).then(done, () => fallbackCopy(text, done));
    } else {
      fallbackCopy(text, done);
    }
  }
  function fallbackCopy(text, done) {
    const ta = document.createElement('textarea');
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    try { document.execCommand('copy'); } catch (e) { /* best effort */ }
    ta.remove();
    done();
  }

  document.getElementById('export-json').addEventListener('click', () =>
    download(stem() + '.json', 'application/json',
      callWasm('pd_export_json', JSON.stringify(doc))));
  document.getElementById('export-md').addEventListener('click', () =>
    download(stem() + '.md', 'text/markdown',
      callWasm('pd_export_markdown', JSON.stringify(doc))));
  document.getElementById('export-csv').addEventListener('click', () =>
    download(stem() + '.csv', 'text/csv',
      callWasm('pd_export_csv', JSON.stringify(doc))));
  document.getElementById('copy-md').addEventListener('click', (ev) =>
    copyText(callWasm('pd_export_markdown', JSON.stringify(doc)), ev.target));

  document.getElementById('import-json').addEventListener('change', (ev) => {
    const file = ev.target.files && ev.target.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = () => {
      try {
        saveDoc(callWasm('pd_merge', JSON.stringify(doc), String(reader.result)));
      } catch (e) {
        alert('Import failed: ' + e.message);
      }
      ev.target.value = '';
    };
    reader.readAsText(file);
  });

  // ---- side-by-side (split) diff view ----
  // The unified tables are the source of truth; a `table.diff.split` sibling
  // is built lazily from each and the two are swapped by a body class. The
  // build reads the unified rows, so it works the same for build-time panels
  // and JS-built range panels, inheriting commentability where it exists.
  const SPLIT_MIN_EM = 90; // side-by-side needs this many em of width to read

  function splitLnCell(no, kind) {
    const td = document.createElement('td');
    td.className = 'ln' + (kind ? ' ' + kind : '');
    td.textContent = no;
    return td;
  }
  function splitCodeCell(text, kind, anchor) {
    const td = document.createElement('td');
    td.className = 'code' + (kind ? ' ' + kind : '');
    if (anchor) {
      td.classList.add('commentable');
      td.dataset.file = anchor.file;
      td.dataset.side = anchor.side;
      td.dataset.line = anchor.line;
    }
    td.textContent = text;
    return td;
  }
  function splitEmpty(tr) {
    const ln = document.createElement('td');
    ln.className = 'ln empty';
    const code = document.createElement('td');
    code.className = 'code empty';
    tr.appendChild(ln);
    tr.appendChild(code);
  }
  // Read a unified code row into { no, text, anchor } for the given side.
  function unifiedCell(row, side) {
    const isOld = side === 'Old';
    const no = (isOld ? row.cells[0] : row.cells[1]).textContent;
    const text = row.cells[2].textContent.slice(1); // drop the +/-/space sign
    const anchor = (row.classList.contains('commentable') && row.dataset.side === side)
      ? { file: row.dataset.file, side: side, line: row.dataset.line } : null;
    return { no: no, text: text, anchor: anchor };
  }

  function buildSplit(unified) {
    const split = document.createElement('table');
    split.className = 'diff split';
    let dels = [], adds = [];
    function flush() {
      const n = Math.max(dels.length, adds.length);
      for (let i = 0; i < n; i++) {
        const tr = document.createElement('tr');
        if (dels[i]) {
          const d = unifiedCell(dels[i], 'Old');
          tr.appendChild(splitLnCell(d.no, 'del'));
          tr.appendChild(splitCodeCell(d.text, 'del', d.anchor));
        } else { splitEmpty(tr); }
        if (adds[i]) {
          const a = unifiedCell(adds[i], 'New');
          tr.appendChild(splitLnCell(a.no, 'add'));
          tr.appendChild(splitCodeCell(a.text, 'add', a.anchor));
        } else { splitEmpty(tr); }
        split.appendChild(tr);
      }
      dels = []; adds = [];
    }
    for (const row of unified.rows) {
      if (row.classList.contains('comment-row') || row.classList.contains('editor-row')) continue;
      if (row.classList.contains('hunk') || row.classList.contains('meta-line')) {
        flush();
        const tr = document.createElement('tr');
        tr.className = row.className;
        const td = document.createElement('td');
        td.colSpan = 4;
        td.textContent = row.cells[row.cells.length - 1].textContent;
        tr.appendChild(td);
        split.appendChild(tr);
      } else if (row.classList.contains('ctx')) {
        flush();
        const c = unifiedCell(row, 'New');
        const oldNo = row.cells[0].textContent;
        const tr = document.createElement('tr');
        tr.appendChild(splitLnCell(oldNo, ''));      // left: old number, non-commentable
        tr.appendChild(splitCodeCell(c.text, '', null));
        tr.appendChild(splitLnCell(c.no, ''));       // right: new number, carries the anchor
        tr.appendChild(splitCodeCell(c.text, '', c.anchor));
        split.appendChild(tr);
      } else if (row.classList.contains('del')) {
        dels.push(row);
      } else if (row.classList.contains('add')) {
        adds.push(row);
      }
    }
    flush();
    return split;
  }

  function ensureSplitBuilt() {
    document.querySelectorAll('table.diff.unified').forEach((u) => {
      const wrap = u.parentElement;
      if (!wrap.querySelector('table.diff.split')) wrap.appendChild(buildSplit(u));
    });
  }
  function applyDiffView() {
    const split = document.body.classList.contains('view-split');
    if (split) ensureSplitBuilt();
    document.querySelectorAll('.diff-wrap').forEach((w) => {
      const u = w.querySelector('table.diff.unified');
      const s = w.querySelector('table.diff.split');
      if (u) u.style.display = split ? 'none' : '';
      if (s) s.style.display = split ? '' : 'none';
    });
    closeEditors();
    renderAll(); // re-home comment rows into the now-visible tables
  }

  const viewToggle = document.getElementById('view-toggle');
  function splitFits() {
    const fs = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
    return window.innerWidth >= SPLIT_MIN_EM * fs;
  }
  function refreshToggle() {
    if (!viewToggle) return;
    const fits = splitFits();
    viewToggle.disabled = !fits;
    viewToggle.title = fits
      ? 'Switch between unified and side-by-side diff'
      : 'Window too narrow for side-by-side — widen it (or reduce the font size)';
    if (!fits && document.body.classList.contains('view-split')) {
      document.body.classList.remove('view-split');
      viewToggle.textContent = 'Side-by-side';
      applyDiffView();
    }
  }
  if (viewToggle) {
    viewToggle.addEventListener('click', () => {
      const nowSplit = !document.body.classList.contains('view-split');
      document.body.classList.toggle('view-split', nowSplit);
      viewToggle.textContent = nowSplit ? 'Unified' : 'Side-by-side';
      applyDiffView();
    });
    window.addEventListener('resize', refreshToggle);
    refreshToggle();
  }

  // ---- nav scrollspy: highlight the section currently under the nav bar ----
  (function () {
    const links = Array.prototype.map.call(
      document.querySelectorAll('nav#topnav a'),
      (a) => ({ a: a, sec: document.getElementById(a.getAttribute('href').slice(1)) })
    ).filter((x) => x.sec);
    if (!links.length) return;
    const navH = document.getElementById('topnav').offsetHeight;
    let raf = 0;
    function update() {
      raf = 0;
      let current = links[0];
      for (const l of links) {
        if (l.sec.getBoundingClientRect().top <= navH + 4) current = l;
      }
      for (const l of links) l.a.classList.toggle('active', l === current);
    }
    window.addEventListener('scroll', () => { if (!raf) raf = requestAnimationFrame(update); }, { passive: true });
    update();
  })();

  doc = initDoc();
  renderAll();
})();
"##;

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
}
