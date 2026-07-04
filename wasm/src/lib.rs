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

use packdiff_dto::review::{Comment, ReviewDocument};
use packdiff_dto::{export, storage_key, RefInfo};
use serde_json::json;

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
#[no_mangle]
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

fn ok(value: serde_json::Value) -> u64 {
  pack(json!({ "Ok": value }).to_string())
}

fn err(message: impl std::fmt::Display) -> u64 {
  pack(json!({ "Error": { "message": message.to_string() } }).to_string())
}

fn doc_value(doc: &ReviewDocument) -> serde_json::Value {
  serde_json::to_value(doc).expect("document serializes")
}

// --------------------------------------------------------------------- api

/// `meta` = `{"repo": "...", "base": {"name","sha"}, "head": {"name","sha"}}`.
/// Returns a fresh empty review document.
#[no_mangle]
pub extern "C" fn pd_new_document(meta_ptr: *const u8, meta_len: u32) -> u64 {
  #[derive(serde::Deserialize)]
  struct Meta {
    repo: String,
    base: RefInfo,
    head: RefInfo,
  }
  match serde_json::from_str::<Meta>(&read_arg(meta_ptr, meta_len)) {
    Ok(m) => ok(doc_value(&ReviewDocument::new(m.repo, m.base, m.head))),
    Err(e) => err(format!("invalid meta: {e}")),
  }
}

/// Parse, validate, and normalize a stored document (the load path).
#[no_mangle]
pub extern "C" fn pd_parse_document(doc_ptr: *const u8, doc_len: u32) -> u64 {
  match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(doc) => ok(doc_value(&doc)),
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
  let comment: Comment = match serde_json::from_str(&read_arg(comment_ptr, comment_len)) {
    Ok(c) => c,
    Err(e) => return err(format!("invalid comment: {e}")),
  };
  match doc.upsert(comment) {
    Ok(()) => ok(doc_value(&doc)),
    Err(e) => err(e),
  }
}

/// Delete a comment by id (the id arrives as a plain UTF-8 string, not JSON).
/// Returns the updated document; deleting a missing id is not an error.
#[no_mangle]
pub extern "C" fn pd_delete_comment(doc_ptr: *const u8, doc_len: u32, id_ptr: *const u8, id_len: u32) -> u64 {
  let mut doc = match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(d) => d,
    Err(e) => return err(e),
  };
  doc.delete(&read_arg(id_ptr, id_len));
  ok(doc_value(&doc))
}

/// Merge an imported document into the current one (union by id, newer
/// `updated_at` wins). Returns the merged document.
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
  ok(doc_value(&doc))
}

#[no_mangle]
pub extern "C" fn pd_export_json(doc_ptr: *const u8, doc_len: u32) -> u64 {
  match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(doc) => ok(serde_json::Value::String(export::to_json(&doc))),
    Err(e) => err(e),
  }
}

#[no_mangle]
pub extern "C" fn pd_export_markdown(doc_ptr: *const u8, doc_len: u32) -> u64 {
  match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(doc) => ok(serde_json::Value::String(export::to_markdown(&doc))),
    Err(e) => err(e),
  }
}

#[no_mangle]
pub extern "C" fn pd_export_csv(doc_ptr: *const u8, doc_len: u32) -> u64 {
  match ReviewDocument::parse(&read_arg(doc_ptr, doc_len)) {
    Ok(doc) => ok(serde_json::Value::String(export::to_csv(&doc))),
    Err(e) => err(e),
  }
}

/// Render markdown to safe HTML (the subset in `packdiff_dto::markdown`).
/// The input is the raw markdown text — a plain UTF-8 string, not JSON —
/// and `Ok` carries the HTML string. Never fails on any input.
#[no_mangle]
pub extern "C" fn pd_markdown_html(text_ptr: *const u8, text_len: u32) -> u64 {
  ok(serde_json::Value::String(packdiff_dto::markdown::to_html(&read_arg(text_ptr, text_len))))
}

/// `meta` as in [`pd_new_document`]; returns the localStorage key string.
#[no_mangle]
pub extern "C" fn pd_storage_key(meta_ptr: *const u8, meta_len: u32) -> u64 {
  #[derive(serde::Deserialize)]
  struct Meta {
    repo: String,
    base: RefInfo,
    head: RefInfo,
  }
  match serde_json::from_str::<Meta>(&read_arg(meta_ptr, meta_len)) {
    Ok(m) => ok(serde_json::Value::String(storage_key(&m.repo, &m.base.sha, &m.head.sha))),
    Err(e) => err(format!("invalid meta: {e}")),
  }
}
