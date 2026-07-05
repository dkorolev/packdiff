# Using the review page

The HTML file packdiff writes is the whole product: one file, no server, no network. Open it from disk (`file://`) or serve it — either works. Everything below happens locally in your browser.

## Layout

- **Header** — repo, `base (sha) → head (sha)`, the merge-base, generation time, and the comment toolbar.
- **Sticky nav** — always visible below the header, with three jump links carrying live counts: **Commits** (N), **Files changed** (N), and **Diff** (`+adds −dels`). The link for the section you are scrolling through is highlighted. When a commit range is selected the Files and Diff counts update to that sub-range. On the right, a **Side-by-side** button toggles the diff view (see below).
- **Commits** — the commit list for the diffed range, oldest first.
- **Files changed** — a compact index, one row per file with its status badge and `+/−` counts; click a row to jump to that file's diff panel.
- **Diff** — one collapsible panel per file (click the path row to fold/unfold), with status badges (`added` / `deleted` / `renamed`), per-file `+/−` counts, and the diff table: old line number, new line number, code. While you scroll through a file, its path row stays pinned under the nav so you can fold it away without scrolling back up. Binary files show a notice instead of content.

## Unified vs side-by-side

The **Side-by-side** button in the nav toggles the whole diff between the default unified view and a two-column split (old on the left, new on the right, with removed and added runs paired row for row). It is enabled only when the window is at least ~90em wide *relative to the current font size*, so zooming in or using a large font raises the threshold in pixels; if you narrow the window below the threshold while split, the page snaps back to unified and disables the button. Commenting works identically in both views — a comment made on a left (old) or right (new) cell anchors the same way it would in the unified view, and switching views moves existing comment cards to the matching rows. The split tables are built in the browser from the unified rows, so there is no extra data in the file.

## Filtering by commits

When the range has two or more commits, the commit list is interactive:

- **Click a commit** to see only that commit's diff. **Click a second commit** to select the contiguous range between the two (shift-click also extends an existing selection). Clicking the only selected commit again — or the *Show full diff* button in the bar that appears — returns to the full diff.
- Every commit row has a **copy** button that copies the full 40-hex hash.
- The Files-changed index and the nav counts follow the selection, so you always see the file list and totals for exactly what is shown.

Sub-range diffs are computed inside the page: the file contents at every commit boundary ship in the HTML (deduplicated by git blob id; binary and >2 MB files excluded), and the WASM model diffs the selected pair of boundaries on demand (`pd_range_diff`, a pure Myers line diff with three context lines). Notes:

- **Comments are full-diff only**: sub-range views are read-only, and per-commit comments are deliberately not a thing — anchors are only meaningful against the full diff the storage key pins.
- Renames are not re-detected inside a sub-range (they show as a delete plus an add), and binary or unsnapshotted files show a notice instead of content.
- A change made and reverted within the selected range correctly disappears; selecting the reverting commit alone shows the revert.

## Commenting

1. **Click any diff line's code cell** (added, deleted, or context — the row highlights on hover). An editor opens under the line.
2. Type; **Ctrl/Cmd+Enter** saves, **Escape** or *Cancel* discards. Empty text is discarded. Comment text is **markdown** (the subset below); the editor shows raw text, saved cards show it rendered. Exports carry the raw markdown, never HTML.
3. Saved comments appear as a card under the line with the anchor (`file · side line N · timestamp`) and **Edit** / **Delete** buttons.
4. The header counter tracks the total.

Comments on deleted (red) lines anchor to the **old** line number; added and context lines anchor to the **new** one. Multiple comments on one line stack in creation order.

Every save/edit/delete runs through the inlined WASM data model — see [WASM-ABI.md](WASM-ABI.md) — and the updated document is written to localStorage synchronously. There is no "unsaved" state.

## Markdown

Markdown appears in two places, both rendered by the same `packdiff-dto::markdown` module (natively at build time for file previews, via the WASM `pd_markdown_html` export for comments):

- **Comment bodies** are rendered as markdown.
- **Markdown files** (`.md`, `.markdown`, `.mdown`, `.mkd`) get a **View rendered** button in their panel header, toggling between the diff and the rendered text. The rendered view is diff-aware: added runs are tinted green, removed runs red, unchanged context is plain. For modified files it covers the diff hunks only, with `⋯` markers where unchanged text is elided; added files render in full. Deleted and binary files have no rendered view. A markdown construct straddling a change boundary renders as separate blocks — inherent to rendering a diff.

The supported subset: ATX headings (`#`–`######`), fenced code blocks, flat (non-nested) `-`/`*`/`+` and `1.` lists, `>` blockquotes, `---` rules, paragraphs; inline `` `code` ``, `**bold**`, `*italic*`, and `[links](https://…)` (`http`/`https`/`mailto` targets only). Underscores are never emphasis, so `snake_case` stays literal. A single newline inside a paragraph is a hard break, GitHub-comment style. Everything is HTML-escaped before rendering; unsafe link schemes render as literal text.

## Where comments live

In this browser's localStorage, under `packdiff:v1:<repo>:<baseSha12>..<headSha12>` ([spec](DATA-MODEL.md#storage-key)). Consequences:

- Reopening the same file — or a **regenerated page for the same two SHAs** (different `--title`/`--context` included) — finds your comments again.
- A different browser, profile, or machine does not. Neither does a diff with new commits (new SHAs → new key). In both cases, carry comments with **Export JSON → Import JSON**.
- If a stored document is corrupt or from a newer schema, the page logs a console warning and starts fresh rather than crashing (the old value is left in place until you save).
- If localStorage itself is unavailable (some sandboxed viewers), a red warning appears and comments last only for the current view — export before closing.

### Unanchored comments

A comment whose diff line is not present in the current rendering (e.g. the page was regenerated with `--context 0`, or an import from a slightly different diff) is never dropped: it appears in the **Unanchored comments** panel above the commit list, with full text and Edit/Delete still available (Edit requires the line to be visible again).

## Toolbar

| Button | Effect |
| --- | --- |
| **Export JSON** | Downloads the full review document — lossless, re-importable. |
| **Export Markdown** | Downloads comments grouped by file — ready to paste into a PR description or hand to an agent. |
| **Export CSV** | Downloads an RFC 4180 spreadsheet (`file,side,line,created_at,updated_at,text`). |
| **Copy Markdown** | Same as Export Markdown, to the clipboard. |
| **Import JSON** | Merges a previously exported JSON file into this store: union by comment id, the newer `updated_at` wins on conflicts. Invalid files are rejected with the reason. |

Download names are `packdiff-comments-<repo>-<baseSha7>-<headSha7>.{json,md,csv}`. Format details: [DATA-MODEL.md#exports](DATA-MODEL.md#exports).

## The comment-carry-over workflow

When the branch gains new commits and you regenerate:

1. On the **old** page: *Export JSON*.
2. Generate and open the **new** page (new SHAs → it starts empty).
3. *Import JSON* the file. Comments whose `(file, side, line)` still exists anchor inline; the rest land in *Unanchored comments* — triage from there.

## Browser support

Any current Chrome, Firefox, or Safari. Requirements: WebAssembly, `BigInt`, and (for persistence) localStorage — all present since ~2020. If WebAssembly is blocked, the page still renders the full diff read-only and shows a "Comment engine failed to load" notice.
