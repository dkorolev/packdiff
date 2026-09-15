//! WASM ABI over `packdiff-dto` — the comment engine the generated HTML page
//! calls into. Every model mutation and export the browser performs goes
//! through here; the page's JavaScript is only a view layer.
//!
//! ABI (no wasm-bindgen, empty import object, works from `file://`):
//!
//! - Strings cross the boundary as `(ptr: u32, len: u32)` UTF-8 buffers.
//!   JS allocates inputs with [`pd_alloc`], writes, calls, then frees them
//!   with [`pd_free`].
//! - Every API function returns a packed `u64` = `(ptr << 32) | len` naming a
//!   fresh UTF-8 buffer the CALLER must free with [`pd_free`] after copying.
//! - Every returned buffer is a single-key union document (§ house style):
//!   `{ "Ok": <result> }` on success or `{ "Error": { "message": "…" } }` on
//!   failure. The `Ok` payload is a document object for mutations and a plain
//!   string for exports/keys.
//!
//! Purity: the module has no clock and no entropy — comment ids and RFC 3339
//! timestamps come in from JS as part of the comment JSON.

use std::alloc::{alloc, dealloc, Layout};

use packdiff_dto::json::{self, Fields, FromJson, ToJson, Value};
use packdiff_dto::review::{Comment, ReviewDocument, Verdict};
use packdiff_dto::{export, storage_key, RefInfo};

// ------------------------------------------------------------------ memory

/// Allocate `len` bytes for the caller to write an input buffer into.
#[no_mangle]
pub extern "C" fn pd_alloc(len: u32) -> *mut u8 {
  if len == 0 {
    return core::ptr::null_mut();
  }
  unsafe { alloc(Layout::from_size_align_unchecked(len as usize, 1)) }
}

/// Free a buffer previously returned by [`pd_alloc`] or by any API function.
/// The raw-pointer signature is the stable C ABI; callers must return exactly
/// a pointer/length pair obtained from this module.
#[no_mangle]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn pd_free(ptr: *mut u8, len: u32) {
  if ptr.is_null() || len == 0 {
    return;
  }
  unsafe { dealloc(ptr, Layout::from_size_align_unchecked(len as usize, 1)) }
}

fn read_arg(ptr: *const u8, len: u32) -> String {
  if ptr.is_null() || len == 0 {
    return String::new();
  }
  let bytes = unsafe { core::slice::from_raw_parts(ptr, len as usize) };
  String::from_utf8_lossy(bytes).into_owned()
}

fn pack(s: String) -> u64 {
  let boxed = s.into_bytes().into_boxed_slice();
  let len = boxed.len() as u64;
  let ptr = Box::leak(boxed).as_mut_ptr() as u32 as u64;
  (ptr << 32) | len
}

fn ok(value: impl Into<Value>) -> u64 {
  pack(Value::object([("Ok", value.into())]).to_string())
}

fn err(message: impl std::fmt::Display) -> u64 {
  pack(Value::object([("Error", Value::object([("message", message.to_string())]))]).to_string())
}

/// The `(repo, base, head)` triple of [`pd_new_document`] and
/// [`pd_storage_key`]: `{"repo": "...", "base": {"name","sha"}, "head":
/// {"name","sha"}}`, nothing else.
fn read_meta(text: &str) -> json::Result<(String, RefInfo, RefInfo)> {
  let value = json::parse(text)?;
  let mut f = Fields::of(&value, "meta")?;
  let meta = (f.required("repo")?, f.required("base")?, f.required("head")?);
  f.finish()?;
  Ok(meta)
}

/// A typed request object parsed from a buffer, strictly.
fn read_request<T: FromJson>(text: &str) -> json::Result<T> {
  json::from_str(text)
}

// --------------------------------------------------------------------- api

/// `meta` = `{"repo": "...", "base": {"name","sha"}, "head": {"name","sha"}}`.
/// Returns a fresh empty review document.
#[no_mangle]
pub extern "C" fn pd_new_document(meta_ptr: *const u8, meta_len: u32) -> u64 {
  match read_meta(&read_arg(meta_ptr, meta_len)) {
    Ok((repo, base, head)) => ok(ReviewDocument::new(repo, base, head).to_json()),
    Err(e) => err(format!("invalid meta: {e}")),
  }
}

/// Parse, validate, and normalize a stored document (the load path).
#[no_mangle]
pub extern "C" fn pd_parse_document(doc_ptr: *const u8, doc_len: u32) -> u64 {
  match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(doc) => ok(doc.to_json()),
    Err(e) => err(e),
  }
}

/// Insert or replace (by id) one comment. Returns the updated document.
#[no_mangle]
pub extern "C" fn pd_upsert_comment(doc_ptr: *const u8, doc_len: u32, comment_ptr: *const u8, comment_len: u32) -> u64 {
  let mut doc = match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(d) => d,
    Err(e) => return err(e),
  };
  let comment: Comment = match read_request(&read_arg(comment_ptr, comment_len)) {
    Ok(c) => c,
    Err(e) => return err(format!("invalid comment: {e}")),
  };
  match doc.upsert(comment) {
    Ok(()) => ok(doc.to_json()),
    Err(e) => err(e),
  }
}

/// `{"id": "…", "actor": "…"}` — the request of [`pd_delete_comment`].
struct DeleteRequest {
  id: String,
  actor: String,
}

impl FromJson for DeleteRequest {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "delete request")?;
    let req = DeleteRequest { id: f.required("id")?, actor: f.or_default("actor")? };
    f.finish()?;
    Ok(req)
  }
}

/// Delete a comment, leaving the versioned tombstone the CRDT merge needs.
/// The request is JSON — `{"id": "…", "actor": "…"}` — because a delete is
/// a write and every write names its actor. Returns the updated document;
/// deleting a missing id is not an error.
#[no_mangle]
pub extern "C" fn pd_delete_comment(doc_ptr: *const u8, doc_len: u32, req_ptr: *const u8, req_len: u32) -> u64 {
  let mut doc = match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(d) => d,
    Err(e) => return err(e),
  };
  let req: DeleteRequest = match read_request(&read_arg(req_ptr, req_len)) {
    Ok(r) => r,
    Err(e) => return err(format!("invalid delete request: {e}")),
  };
  doc.delete_by(&req.id, &req.actor);
  ok(doc.to_json())
}

/// `{"verdict": <union or null>, "actor": "…"}` — the request of
/// [`pd_set_verdict`].
struct VerdictRequest {
  verdict: Option<Verdict>,
  actor: String,
}

impl FromJson for VerdictRequest {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "verdict request")?;
    let req = VerdictRequest { verdict: f.optional("verdict")?, actor: f.or_default("actor")? };
    f.finish()?;
    Ok(req)
  }
}

/// Set, replace, or clear the review verdict. The request is JSON —
/// `{"verdict": <single-key union or null>, "actor": "…"}` — where the
/// union is `{"Approved": {"at": "…"}}` / `{"ChangesRequired": {"at":
/// "…"}}` and `null` returns the review to in-progress; the clear is a
/// stamped write like any other, which is what lets it merge. Returns the
/// updated document.
#[no_mangle]
pub extern "C" fn pd_set_verdict(doc_ptr: *const u8, doc_len: u32, req_ptr: *const u8, req_len: u32) -> u64 {
  let mut doc = match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(d) => d,
    Err(e) => return err(e),
  };
  let req: VerdictRequest = match read_request(&read_arg(req_ptr, req_len)) {
    Ok(r) => r,
    Err(e) => return err(format!("invalid verdict request: {e}")),
  };
  match doc.set_verdict_by(req.verdict, &req.actor) {
    Ok(()) => ok(doc.to_json()),
    Err(e) => err(e),
  }
}

/// Merge an imported document into the current one — the CRDT join:
/// commutative, associative, idempotent; registers decide by Lamport
/// version, tombstones keep deletions deleted. Returns the merged document.
#[no_mangle]
pub extern "C" fn pd_merge(doc_ptr: *const u8, doc_len: u32, incoming_ptr: *const u8, incoming_len: u32) -> u64 {
  let mut doc = match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(d) => d,
    Err(e) => return err(e),
  };
  let incoming = match ReviewDocument::parse(&read_arg(incoming_ptr, incoming_len)) {
    Ok(d) => d,
    Err(e) => return err(format!("import rejected: {e}")),
  };
  doc.merge(&incoming);
  ok(doc.to_json())
}

#[no_mangle]
pub extern "C" fn pd_export_json(doc_ptr: *const u8, doc_len: u32) -> u64 {
  match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(doc) => ok(export::to_json(&doc)),
    Err(e) => err(e),
  }
}

#[no_mangle]
pub extern "C" fn pd_export_markdown(doc_ptr: *const u8, doc_len: u32) -> u64 {
  match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(doc) => ok(export::to_markdown(&doc)),
    Err(e) => err(e),
  }
}

#[no_mangle]
pub extern "C" fn pd_export_csv(doc_ptr: *const u8, doc_len: u32) -> u64 {
  match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(doc) => ok(export::to_csv(&doc)),
    Err(e) => err(e),
  }
}

/// `{"from": N, "to": N, "context": N}` — the request of [`pd_range_diff`].
struct RangeParams {
  from: usize,
  to: usize,
  context: usize,
}

impl FromJson for RangeParams {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "params")?;
    let params = RangeParams { from: f.required("from")?, to: f.required("to")?, context: f.required("context")? };
    f.finish()?;
    Ok(params)
  }
}

/// Diff a contiguous commit sub-range from the page's embedded snapshots.
/// `snapshots` is a `RangeSnapshots` JSON; `params` is
/// `{"from": <boundary index>, "to": <boundary index>, "context": N}` with
/// `from < to` (commit `k` alone is `from = k - 1, to = k`). `Ok` carries
/// the `FileDiff` array — the same shape the build-time parser emits.
#[no_mangle]
pub extern "C" fn pd_range_diff(snap_ptr: *const u8, snap_len: u32, params_ptr: *const u8, params_len: u32) -> u64 {
  let snap: packdiff_dto::snapshot::RangeSnapshots = match read_request(&read_arg(snap_ptr, snap_len)) {
    Ok(s) => s,
    Err(e) => return err(format!("invalid snapshots: {e}")),
  };
  let params: RangeParams = match read_request(&read_arg(params_ptr, params_len)) {
    Ok(p) => p,
    Err(e) => return err(format!("invalid params: {e}")),
  };
  match packdiff_dto::snapshot::range_diff(&snap, params.from, params.to, params.context) {
    Ok(files) => ok(files.to_json()),
    Err(e) => err(e),
  }
}

/// `{"old_path", "new_path", "old_start", "new_start", "count"}` — the
/// request of [`pd_context_slice`].
struct SliceParams {
  old_path: String,
  new_path: String,
  old_start: u32,
  new_start: u32,
  count: u32,
}

impl FromJson for SliceParams {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "params")?;
    let params = SliceParams {
      old_path: f.required("old_path")?,
      new_path: f.required("new_path")?,
      old_start: f.required("old_start")?,
      new_start: f.required("new_start")?,
      count: f.required("count")?,
    };
    f.finish()?;
    Ok(params)
  }
}

/// Unchanged lines shared by a file's two endpoint snapshots — the page's
/// expand-context data. `snapshots` is a `RangeSnapshots` JSON; `params` is
/// `{"old_path": "...", "new_path": "...", "old_start": N, "new_start": N,
/// "count": N}` (1-based starts; paths differ for renames). `Ok` carries a
/// `Line` array of `Ctx` entries, clamped at either file's end; a region
/// that is not identical at both endpoints is rejected.
#[no_mangle]
pub extern "C" fn pd_context_slice(snap_ptr: *const u8, snap_len: u32, params_ptr: *const u8, params_len: u32) -> u64 {
  let snap: packdiff_dto::snapshot::RangeSnapshots = match read_request(&read_arg(snap_ptr, snap_len)) {
    Ok(s) => s,
    Err(e) => return err(format!("invalid snapshots: {e}")),
  };
  let params: SliceParams = match read_request(&read_arg(params_ptr, params_len)) {
    Ok(p) => p,
    Err(e) => return err(format!("invalid params: {e}")),
  };
  match packdiff_dto::snapshot::context_slice(
    &snap,
    &params.old_path,
    &params.new_path,
    params.old_start,
    params.new_start,
    params.count,
  ) {
    Ok(lines) => ok(lines.to_json()),
    Err(e) => err(e),
  }
}

/// Render markdown to safe HTML (the subset in `packdiff_dto::markdown`).
/// The input is the raw markdown text — a plain UTF-8 string, not JSON —
/// and `Ok` carries the HTML string. Never fails on any input.
#[no_mangle]
pub extern "C" fn pd_markdown_html(text_ptr: *const u8, text_len: u32) -> u64 {
  ok(packdiff_dto::markdown::to_html(&read_arg(text_ptr, text_len)))
}

/// Highlight a contiguous source-line run. `lines` is a JSON string array;
/// `Ok` carries an HTML string array, or `null` for an unknown path.
#[no_mangle]
pub extern "C" fn pd_highlight_lines(path_ptr: *const u8, path_len: u32, lines_ptr: *const u8, lines_len: u32) -> u64 {
  let path = read_arg(path_ptr, path_len);
  let lines: Vec<String> = match read_request(&read_arg(lines_ptr, lines_len)) {
    Ok(lines) => lines,
    Err(e) => return err(format!("invalid lines: {e}")),
  };
  let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
  ok(packdiff_dto::highlight::highlight_lines(&path, &refs).to_json())
}

/// Highlight a typed diff hunk with independent old/new lexer streams.
/// `lines` is a JSON `Line`-union array; success has the same fallback shape
/// as [`pd_highlight_lines`].
#[no_mangle]
pub extern "C" fn pd_highlight_hunk(path_ptr: *const u8, path_len: u32, lines_ptr: *const u8, lines_len: u32) -> u64 {
  let path = read_arg(path_ptr, path_len);
  let lines: Vec<packdiff_dto::diff::Line> = match read_request(&read_arg(lines_ptr, lines_len)) {
    Ok(lines) => lines,
    Err(e) => return err(format!("invalid hunk lines: {e}")),
  };
  ok(packdiff_dto::highlight::highlight_hunk(&path, &lines).to_json())
}

/// `meta` as in [`pd_new_document`]; returns the legacy SHA-pinned
/// localStorage key string. Pages now key state by the content-fingerprint
/// `review_id` and call this once to migrate pre-`review_id` state.
#[no_mangle]
pub extern "C" fn pd_storage_key(meta_ptr: *const u8, meta_len: u32) -> u64 {
  match read_meta(&read_arg(meta_ptr, meta_len)) {
    Ok((repo, base, head)) => ok(storage_key(&repo, &base.sha, &head.sha)),
    Err(e) => err(format!("invalid meta: {e}")),
  }
}
