# WASM ABI reference — crate `packdiff-wasm`

`wasm/` compiles the data model to `wasm32-unknown-unknown` behind a small,
hand-rolled C ABI — **no wasm-bindgen, no JS glue, empty import object** — so
the module instantiates from `file://` with `WebAssembly.instantiate(bytes, {})`.
The CLI inlines it base64-encoded into every generated page
(`<script type="application/wasm-base64" id="packdiff-wasm">`), where it is the
comment engine: the page's JavaScript is a view layer and never edits review
state itself.

The built artifact lives at
`target-wasm/wasm32-unknown-unknown/release/packdiff_wasm.wasm` (~124 KB).
`tests/wasm_abi.test.mjs` drives it exactly as described here.

## Calling convention

**Inputs.** Every string crosses as a `(ptr: u32, len: u32)` pair naming a
UTF-8 buffer in the module's linear memory. The caller:

1. `ptr = pd_alloc(len)` — allocate.
2. Write the UTF-8 bytes at `ptr` (re-view `exports.memory.buffer` *after*
   each alloc; growth detaches old views).
3. Pass `(ptr, len)`. After the call returns, `pd_free(ptr, len)`.

**Outputs.** Every API function returns a packed `u64` (a `BigInt` in JS):

```
result = (ptr << 32) | len          // ptr, len of a fresh UTF-8 buffer
ptr = Number(result >> 32n)
len = Number(result & 0xffffffffn)
```

The caller copies the bytes out, then **must** `pd_free(ptr, len)`.

**Envelope.** Every returned buffer is a single-key union document:

```json
{ "Ok": <result> }
{ "Error": { "message": "<what went wrong>" } }
```

`Ok` carries a `ReviewDocument` **object** for constructive/mutating calls and
a plain **string** for exports and the storage key. Errors never trap; invalid
input comes back as an `Error` document (the model is panic-free by
construction on these paths).

## Exports

Memory management:

| Export | Signature | Notes |
| --- | --- | --- |
| `pd_alloc` | `(len: u32) -> ptr: u32` | Returns null for `len == 0` |
| `pd_free` | `(ptr: u32, len: u32)` | Must be called with the exact length; null/0 is a no-op |
| `memory` | exported linear memory | — |

API (all return the packed-u64 envelope; `meta` and `doc`/`comment`/`incoming`
arguments are JSON strings):

| Export | Arguments | `value` on success |
| --- | --- | --- |
| `pd_new_document` | `meta` | fresh empty `ReviewDocument` object |
| `pd_parse_document` | `doc` | validated + normalized `ReviewDocument` object (the load path; rejects newer `schema_version`, invalid comments, garbage) |
| `pd_upsert_comment` | `doc`, `comment` | updated document (insert, or replace by `id`) |
| `pd_delete_comment` | `doc`, `id` *(plain string, not JSON)* | updated document; deleting a missing id is not an error |
| `pd_merge` | `doc`, `incoming` | merged document (union by id, later `updated_at` wins) — the Import JSON path |
| `pd_export_json` | `doc` | canonical pretty JSON **string** |
| `pd_export_markdown` | `doc` | Markdown **string** |
| `pd_export_csv` | `doc` | RFC 4180 CSV **string** |
| `pd_storage_key` | `meta` | localStorage key **string** |

`meta` shape (used by `pd_new_document` and `pd_storage_key`):

```json
{ "repo": "myrepo",
  "base": { "name": "main",       "sha": "<40-hex>" },
  "head": { "name": "my-feature", "sha": "<40-hex>" } }
```

Document/comment shapes and all semantics (validation, ordering, merge rules)
are specified in [data-model.md](data-model.md) — this layer adds nothing but
the transport.

## Purity

The module has no clock and no entropy source. Comment `id`s and RFC 3339
timestamps arrive from the caller inside the comment JSON; the same call with
the same bytes always returns the same bytes.

## Minimal JS bridge

This is the page's actual bridge, reduced to its core (also mirrored in
`tests/wasm_abi.test.mjs`):

```js
const { instance } = await WebAssembly.instantiate(bytes, {});
const ex = instance.exports;
const enc = new TextEncoder(), dec = new TextDecoder();

function callWasm(name, ...strs) {
  const allocs = [], args = [];
  for (const s of strs) {
    const b = enc.encode(s);
    const ptr = ex.pd_alloc(b.length);
    new Uint8Array(ex.memory.buffer, ptr, b.length).set(b);
    args.push(ptr, b.length); allocs.push([ptr, b.length]);
  }
  const packed = ex[name](...args);
  for (const [p, l] of allocs) ex.pd_free(p, l);
  const rptr = Number(packed >> 32n), rlen = Number(packed & 0xffffffffn);
  const out = dec.decode(new Uint8Array(ex.memory.buffer, rptr, rlen).slice());
  ex.pd_free(rptr, rlen);
  const env = JSON.parse(out);
  if ('Ok' in env) return env.Ok;
  throw new Error(env.Error.message);
}

// e.g.:
let doc = callWasm('pd_new_document', JSON.stringify(meta));
doc = callWasm('pd_upsert_comment', JSON.stringify(doc), JSON.stringify(comment));
const md = callWasm('pd_export_markdown', JSON.stringify(doc));
```

## Stability

The ABI is versioned implicitly through the documents' `schema_version`: the
function set may grow (additive) within v1; renaming or re-typing an existing
export is a breaking change and would accompany a schema bump.
