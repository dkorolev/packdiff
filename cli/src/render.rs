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
      r#"<div class="hint">Click a commit to see only its diff; shift-click extends the selection to a range. Commenting works on the full diff only.</div>"#,
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

fn render_file(f: &FileDiff) -> String {
  let badge = match f.status {
    FileStatus::Modified => String::new(),
    FileStatus::Added => r#"<span class="badge badge-added">added</span> "#.into(),
    FileStatus::Deleted => r#"<span class="badge badge-deleted">deleted</span> "#.into(),
    FileStatus::Renamed => r#"<span class="badge badge-renamed">renamed</span> "#.into(),
  };
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
    let table = format!(r#"<table class="diff">{rows}</table>"#);
    if renderable_markdown {
      format!("{table}{}", render_markdown_preview(f))
    } else {
      table
    }
  };
  format!(
    r#"<details class="file" open><summary>{badge}<span class="path">{}</span> <span class="stats">{toggle}<span class="adds">+{}</span> <span class="dels">−{}</span></span></summary>{notes}{body}</details>"#,
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
    doc.files.iter().map(render_file).collect()
  };
  let short = |sha: &str| sha[..sha.len().min(12)].to_string();

  let mut page = String::with_capacity(64 * 1024 + wasm_bytes.len() * 4 / 3);
  page.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
  page.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
  page.push_str(&format!("<title>{}</title>\n", esc(&page_title)));
  page.push_str("<style>");
  page.push_str(CSS);
  page.push_str("</style>\n</head>\n<body>\n");
  page.push_str(&format!(
        r#"<header class="top">
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
"#,
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
  page.push_str("<section><h2>Commits</h2>");
  page.push_str(&render_commits(doc));
  page.push_str("</section>\n<section><h2>Files changed</h2>");
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
header.top { border-bottom:1px solid var(--border); background:var(--panel);
  padding:16px; position:sticky; top:0; z-index:10; }
header.top h1 { margin:0 0 4px; font-size:18px; }
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
details.file { border:1px solid var(--border); border-radius:8px; margin:12px 0; overflow:hidden; }
details.file > summary { cursor:pointer; padding:8px 12px; background:var(--panel);
  font-family:ui-monospace,SFMono-Regular,Menlo,Consolas,monospace; font-size:13px; }
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
  function cssAttr(v) { return String(v).replace(/\\/g, '\\\\').replace(/"/g, '\\"'); }
  function rowFor(c) {
    const sel = 'tr.commentable[data-file="' + cssAttr(c.file) + '"]' +
      '[data-side="' + cssAttr(c.side) + '"][data-line="' + cssAttr(String(c.line)) + '"]';
    return document.querySelector(sel);
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
      if (row) openEditor(row, c);
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
    td.colSpan = 3;
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
  function openEditor(row, existing) {
    closeEditors();
    const tr = document.createElement('tr');
    tr.className = 'editor-row';
    const td = document.createElement('td');
    td.colSpan = 3;
    const ta = document.createElement('textarea');
    ta.placeholder = 'Leave a comment… (Ctrl/Cmd+Enter to save)';
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
        : { id: genId(), file: row.dataset.file, side: row.dataset.side,
            line: Number(row.dataset.line), text: text,
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
    row.after(tr);
    ta.focus();
  }

  document.addEventListener('click', (ev) => {
    const cell = ev.target.closest('td.code');
    if (!cell) return;
    const row = cell.parentElement;
    if (!row.classList.contains('commentable')) return;
    openEditor(row, null);
  });

  // ---- rendered-markdown toggle for .md files (pure view state) ----
  document.addEventListener('click', (ev) => {
    const btn = ev.target.closest('button.md-toggle');
    if (!btn) return;
    ev.preventDefault(); // the button lives inside <summary>: do not fold the panel
    const file = btn.closest('details.file');
    const table = file.querySelector('table.diff');
    const preview = file.querySelector('.md-preview');
    if (!table || !preview) return;
    const showRendered = preview.hidden;
    preview.hidden = !showRendered;
    table.style.display = showRendered ? 'none' : '';
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
    if (ev.shiftKey && rangeFrom !== null) {
      rangeFrom = Math.min(rangeFrom, idx);
      rangeTo = Math.max(rangeTo, idx);
    } else if (rangeFrom === idx && rangeTo === idx) {
      rangeFrom = rangeTo = null; // clicking the sole selected commit deselects
    } else {
      rangeFrom = rangeTo = idx;
    }
    applyRange();
  });
  const rangeReset = document.getElementById('range-reset');
  if (rangeReset) rangeReset.addEventListener('click', () => {
    rangeFrom = rangeTo = null;
    applyRange();
  });

  function applyRange() {
    const bar = document.getElementById('range-bar');
    const full = document.getElementById('files-full');
    const ranged = document.getElementById('files-range');
    document.querySelectorAll('tr.commit').forEach((tr) => {
      const i = Number(tr.dataset.index);
      tr.classList.toggle('selected', rangeFrom !== null && i >= rangeFrom && i <= rangeTo);
    });
    if (rangeFrom === null) {
      bar.hidden = true;
      ranged.hidden = true;
      ranged.textContent = '';
      full.style.display = '';
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
    ranged.textContent = '';
    if (files.length === 0) {
      const p = document.createElement('p');
      p.className = 'muted';
      p.textContent = 'No changes in the selected commits.';
      ranged.appendChild(p);
    }
    for (const f of files) ranged.appendChild(rangeFileEl(f));
    const shortOf = (i) =>
      document.querySelector('tr.commit[data-index="' + i + '"] code').textContent;
    const n = rangeTo - rangeFrom + 1;
    document.getElementById('range-label').textContent = n === 1
      ? 'Showing commit ' + shortOf(rangeFrom) + ' only'
      : 'Showing commits ' + shortOf(rangeFrom) + '..' + shortOf(rangeTo) + ' (' + n + ' commits)';
    bar.hidden = false;
    full.style.display = 'none';
    ranged.hidden = false;
  }

  // Build a file panel for a wasm-computed FileDiff. DOM-built (values land
  // via textContent, never markup), and NOT commentable — range rows carry
  // no anchors on purpose.
  function rangeFileEl(f) {
    const details = document.createElement('details');
    details.className = 'file';
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
    const table = document.createElement('table');
    table.className = 'diff';
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
    details.appendChild(table);
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
