# Data model reference — crate `packdiff-dto`

The `dto/` crate is the single source of truth for every piece of data packdiff handles. It is pure logic: no filesystem, no subprocess, no clock, no randomness. The CLI links it natively; the generated page runs the exact same crate compiled to WebAssembly. Rust API docs: `cargo doc -p packdiff-dto --open`.

Both document families carry `schema_version` (currently `1`) and `tool: "packdiff"`.

## Encoding and compatibility rules

- **Field names are `snake_case`; union variants are `CamelCase`** and encode as **single-key objects** — `{ "Add": { "new": 2, "text": "…" } }` — never a `"type"` discriminator alongside the payload. Payload-less enums (`FileStatus`, `Side`) serialize as the bare variant-name string.
- **Unknown fields are strict-rejected** (`deny_unknown_fields`) on every struct: a typo or a drifted producer fails loudly at the boundary instead of silently becoming "field never set."
- Parsers **reject documents from a newer schema** than they understand (`unsupported schema_version N`) and accept older ones. `schema_version` is the one deliberate opt-in to long-term compatibility — review documents live in users' localStorage.
- Determinism: identical inputs produce byte-identical outputs. Ordering is always explicit, and timestamps/ids are caller-supplied inputs, never generated inside the model.

## `DiffDocument`

The immutable build artifact — everything the CLI extracted from git. Written by `--dump-json`; the page embeds a reduced form of its header in `#packdiff-config`.

```json
{
  "schema_version": 1,
  "tool": "packdiff",
  "repo": "myrepo",
  "base": { "name": "main",       "sha": "<40-hex>" },
  "head": { "name": "my-feature", "sha": "<40-hex>" },
  "merge_base": "<40-hex>",
  "generated_at": "2026-07-03T04:14:09Z",
  "commits": [
    {
      "sha": "<40-hex>", "short": "<7+ hex>",
      "author": "Jane Dev", "email": "jane@example.com",
      "date": "2026-07-01T12:34:56+01:00",
      "subject": "feature change one"
    }
  ],
  "files": [
    {
      "old_path": "hello.py",
      "new_path": "hello.py",
      "status": "Modified",
      "binary": false,
      "additions": 3,
      "deletions": 1,
      "notes": [],
      "hunks": [
        {
          "header": "@@ -1,2 +1,4 @@",
          "lines": [
            { "Ctx":  { "old": 1, "new": 1, "text": "def hello():" } },
            { "Del":  { "old": 2, "text": "    return 'hi'" } },
            { "Add":  { "new": 2, "text": "    return 'hello'" } },
            { "Meta": { "text": "\\ No newline at end of file" } }
          ]
        }
      ]
    }
  ]
}
```

Field notes:

- `status` ∈ `Added | Deleted | Modified | Renamed`. For `added`, `old_path` is `null`; for `deleted`, `new_path` is `null`; for `renamed`, both are set and differ.
- `notes` carries surfaced extended headers (currently mode changes, e.g. `"old mode 100644"`).
- Lines are single-key unions (`Add`/`Del`/`Ctx`/`Meta`). Line numbers are 1-based: `old` counts in the pre-image, `new` in the post-image. `ctx` lines carry both; `meta` lines carry neither.
- `binary: true` files have no hunks.
- The **anchor path** of a file — the `file` string comments use — is `new_path` when present, else `old_path`.
- It is produced by `diff::parse_unified_diff`, a pure function over `git diff --no-color --no-ext-diff --find-renames -U<N>` output.

### `snapshots` — commit-boundary file contents

Present when the range has two or more commits; powers the page's in-place commit-range filter. Contents are deduplicated by git blob id; a blob stored as `null` was not snapshotted (binary, not UTF-8, or over 2 MB) and renders as "contents not shown" in sub-range views.

```json
{
  "snapshots": {
    "blobs": { "<blob-40-hex>": "file contents…", "<blob-40-hex>": null },
    "boundaries": [
      { "sha": "<40-hex>", "files": { "hello.py": "<blob-40-hex>" } }
    ]
  }
}
```

- `boundaries[0]` is the diff's start (the merge base); `boundaries[k]` for `k > 0` is the state after the k-th commit. Only paths touched by some commit in the range appear in `files`; a missing path does not exist at that boundary.
- `snapshot::range_diff(snapshots, from, to, context)` (WASM: `pd_range_diff`) produces the `FileDiff` array between two boundaries via a pure Myers line diff. Renames are not re-detected: within a sub-range a rename is a delete plus an add.

### `description` — the lifted PR description

Present when the range carries notes commits (the notes-commit convention — see [CLI.md](CLI.md)). The lifted file and its commits are excluded from `files`, `commits`, and the snapshot boundaries; this field is the only place they appear.

```json
{
  "description": {
    "path": "PR-DESCRIPTION.md",
    "text": "# Summary\n\n…full markdown…",
    "commits": ["<40-hex>"]
  }
}
```

Comments on the page's Description panel anchor to `path`, side `New`, 1-based lines of `text`.

## `ReviewDocument`

The mutable review state. Lives in the browser's localStorage; every mutation goes through the model (via WASM in the browser). This is also the **Export JSON / Import JSON** format.

```json
{
  "schema_version": 1,
  "tool": "packdiff",
  "repo": "myrepo",
  "base": { "name": "main",       "sha": "<40-hex>" },
  "head": { "name": "my-feature", "sha": "<40-hex>" },
  "comments": [
    {
      "id": "cmcdkq8x1a2b3c",
      "file": "src/app.rs",
      "side": "New",
      "line": 42,
      "text": "multi\nline note",
      "created_at": "2026-07-03T10:00:00.000Z",
      "updated_at": "2026-07-03T10:05:00.000Z"
    }
  ]
}
```

### Identity and anchoring

- **Comment identity** is `id` (any non-empty string; the page generates time+random ids). Ids must be unique within a document — `upsert` replaces on id match.
- **Anchor identity** is `(file, side, line)`: `file` is the post-image path; `side` is `"Old"` (a deleted line, `line` counts in the pre-image) or `"New"` (an added or context line, `line` counts in the post-image). Several comments may share one anchor.

### Validation (applied on every entry into the document)

`id`, `file`, `text`, `created_at`, `updated_at` non-empty; `line >= 1`. `ReviewDocument::parse` additionally rejects newer schemas and re-sorts, so untrusted input (imports, hand-edited stores) is normalized at the boundary.

### Canonical order

Comments always sort by `(file, line, side, created_at, id)` with `old` before `new` on the same line. Every operation restores this order.

### Merge semantics (`merge` — the Import JSON path)

- Union by `id`.
- On id collision the comment with the **later `updated_at`** wins (RFC 3339 UTC strings compare correctly as strings); an exact tie keeps the existing comment.
- The receiving document's metadata (`repo`, `base`, `head`) is kept — importing never re-targets a store. This is what carries comments across regenerated diffs: export from the old page, import into the new one.

## Exports

| Format | Producer | Shape |
| --- | --- | --- |
| JSON | `export::to_json` | The canonical pretty `ReviewDocument` + trailing newline. Lossless; the only format import accepts. |
| Markdown | `export::to_markdown` | `# Review comments — <repo> <base>..<head>`, a count/SHA line, then `## <file>` sections with `- **L<line> (<side>)** — text` items; multi-line comment text is indented two spaces. |
| CSV | `export::to_csv` | RFC 4180: every field quoted (`"` doubled), CRLF row endings, header `file,side,line,created_at,updated_at,text`. |

## Storage key

```
packdiff:v1:<repo>:<base-sha-12>..<head-sha-12>
```

Derived by `storage_key()` (exposed to the page as `pd_storage_key`). The key pins exact endpoint SHAs on purpose: line numbers only mean something against the diff they were written on. Regenerating the identical diff (any title or context) finds the same comments; any new commit yields a fresh, empty store.
