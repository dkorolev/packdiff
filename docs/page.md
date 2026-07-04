# Using the review page

The HTML file packdiff writes is the whole product: one file, no server, no
network. Open it from disk (`file://`) or serve it — either works. Everything
below happens locally in your browser.

## Layout

- **Header** — repo, `base (sha) → head (sha)`, the merge-base, generation
  time, totals, and the comment toolbar.
- **Commits** — the commit list for the diffed range, oldest first.
- **Files changed** — one collapsible panel per file (click the path row to
  fold/unfold), with status badges (`added` / `deleted` / `renamed`), per-file
  `+/−` counts, and the diff table: old line number, new line number, code.
  Binary files show a notice instead of content.

## Commenting

1. **Click any diff line's code cell** (added, deleted, or context — the row
   highlights on hover). An editor opens under the line.
2. Type; **Ctrl/Cmd+Enter** saves, **Escape** or *Cancel* discards. Empty
   text is discarded. Comment text is **markdown** (the subset below); the
   editor shows raw text, saved cards show it rendered. Exports carry the
   raw markdown, never HTML.
3. Saved comments appear as a card under the line with the anchor
   (`file · side line N · timestamp`) and **Edit** / **Delete** buttons.
4. The header counter tracks the total.

Comments on deleted (red) lines anchor to the **old** line number; added and
context lines anchor to the **new** one. Multiple comments on one line stack
in creation order.

Every save/edit/delete runs through the inlined WASM data model — see
[wasm-abi.md](wasm-abi.md) — and the updated document is written to
localStorage synchronously. There is no "unsaved" state.

## Markdown

Markdown appears in two places, both rendered by the same
`packdiff-dto::markdown` module (natively at build time for file previews,
via the WASM `pd_markdown_html` export for comments):

- **Comment bodies** are rendered as markdown.
- **Markdown files** (`.md`, `.markdown`, `.mdown`, `.mkd`) get a
  **View rendered** button in their panel header, toggling between the diff
  and the rendered post-image text. For modified files the rendered view
  covers the diff hunks only (context + added lines), with `⋯` markers where
  unchanged text is elided; added files render in full. Deleted and binary
  files have no rendered view.

The supported subset: ATX headings (`#`–`######`), fenced code blocks,
flat (non-nested) `-`/`*`/`+` and `1.` lists, `>` blockquotes, `---` rules,
paragraphs; inline `` `code` ``, `**bold**`, `*italic*`, and
`[links](https://…)` (`http`/`https`/`mailto` targets only). Underscores are
never emphasis, so `snake_case` stays literal. A single newline inside a
paragraph is a hard break, GitHub-comment style. Everything is HTML-escaped
before rendering; unsafe link schemes render as literal text.

## Where comments live

In this browser's localStorage, under
`packdiff:v1:<repo>:<baseSha12>..<headSha12>`
([spec](data-model.md#storage-key)). Consequences:

- Reopening the same file — or a **regenerated page for the same two SHAs**
  (different `--title`/`--context` included) — finds your comments again.
- A different browser, profile, or machine does not. Neither does a diff with
  new commits (new SHAs → new key). In both cases, carry comments with
  **Export JSON → Import JSON**.
- If a stored document is corrupt or from a newer schema, the page logs a
  console warning and starts fresh rather than crashing (the old value is
  left in place until you save).
- If localStorage itself is unavailable (some sandboxed viewers), a red
  warning appears and comments last only for the current view — export before
  closing.

### Unanchored comments

A comment whose diff line is not present in the current rendering (e.g. the
page was regenerated with `--context 0`, or an import from a slightly
different diff) is never dropped: it appears in the **Unanchored comments**
panel above the commit list, with full text and Edit/Delete still available
(Edit requires the line to be visible again).

## Toolbar

| Button | Effect |
| --- | --- |
| **Export JSON** | Downloads the full review document — lossless, re-importable. |
| **Export Markdown** | Downloads comments grouped by file — ready to paste into a PR description or hand to an agent. |
| **Export CSV** | Downloads an RFC 4180 spreadsheet (`file,side,line,created_at,updated_at,text`). |
| **Copy Markdown** | Same as Export Markdown, to the clipboard. |
| **Import JSON** | Merges a previously exported JSON file into this store: union by comment id, the newer `updated_at` wins on conflicts. Invalid files are rejected with the reason. |

Download names are
`packdiff-comments-<repo>-<baseSha7>-<headSha7>.{json,md,csv}`. Format details:
[data-model.md#exports](data-model.md#exports).

## The comment-carry-over workflow

When the branch gains new commits and you regenerate:

1. On the **old** page: *Export JSON*.
2. Generate and open the **new** page (new SHAs → it starts empty).
3. *Import JSON* the file. Comments whose `(file, side, line)` still exists
   anchor inline; the rest land in *Unanchored comments* — triage from there.

## Browser support

Any current Chrome, Firefox, or Safari. Requirements: WebAssembly, `BigInt`,
and (for persistence) localStorage — all present since ~2020. If WebAssembly
is blocked, the page still renders the full diff read-only and shows a
"Comment engine failed to load" notice.
