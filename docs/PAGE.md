# Using the review page

The HTML file packdiff writes is the whole product: one file, no server, no network. Open it from disk (`file://`) or serve it — either works. Everything below happens locally in your browser.

## Layout

The review reads top to bottom as one document — summary first, then Description (when present), Commits, Files changed, Diff, Activity:

- **Review summary** — a plain-language line (commit and file counts, `+/−` totals, how to comment) with a **Details** disclosure underneath (repository, refs with SHAs, merge-base, generation time, tool version, schema). Both scroll away.
- **Pinned navigation** — the ONE row that stays: **← Back**, scroll-tracking section links, the current file's context while you scroll the diff (path breadcrumbs, `+/−`, comment count), the **Approve | Require changes** verdict, the **Side-by-side** toggle, the comment count (click steps to the next comment), the review-change count, **Undo**, and the **Actions** menu. **Description**, **Commits**, **Files changed**, and **Diff** are clickable tabs whose blue active state follows the visible section.
- **Description** — the lifted PR description (the [notes-commit convention](CLI.md#the-notes-commit-convention)), a rendered card with no provenance chrome. A **Rendered | Source** pill switches to a line-numbered source view; both are commentable.
- **Commits** — the commit table; rows become selectable for range filtering when snapshots are embedded.
- **Files changed** — the file index (status, path, per-file `+/−`). This section IS the navigator; there is no separate sidebar.
- **Diff** — one collapsible panel per file. Headers carry the status badge, path, the Markdown view pill (for `.md` files), **Wrap | Scroll**, comment and draft counts, `+/−`, and the **Viewed** checkbox.
- **Activity** — the journal of review changes, newest first.

Breadcrumb segments in the pinned row are actionable: selecting a directory lands on its rows in Files changed and briefly highlights them.

## Unified vs side-by-side

**Side-by-side** toggles the whole diff between the default unified view and a two-column split (old on the left, new on the right). The control stays in place but is disabled — “Not enough horizontal space to render side-by-side” — until the workspace is at least ~90em wide at the current font size. Split tables are built in the browser from the unified rows, lazily, as file panels approach the viewport. Commenting works in both views.

**Wrap | Scroll** is a per-file choice on each file header: long lines soft-wrap by default (`overflow-wrap: anywhere`, not `break-all`); Scroll switches that file to horizontal scrolling. The choice persists.

## Syntax highlighting

Every code-line surface uses the same safe lexical highlighter: the initial unified diff is highlighted at build time, and the embedded WASM engine supplies identical markup for commit-range views and expanded context; side-by-side copies that engine-authored markup from the unified rows. Profiles cover Rust, C/C++, Java, Kotlin, Go, Python, JavaScript/TypeScript, Ruby, shell, SQL, CSS, TOML, YAML, and JSON. HTML/XML and Markdown source remain deliberately plain, as does any unknown extension.

Highlighting classifies comments, strings, numbers, keywords, word literals, and likely function calls by color without changing font weight, line metrics, wrapping, or diff meaning. It is lexical presentation only: it never compares old and new text and is not intraline diff emphasis. Source is HTML-escaped inside the engine before fixed `tok-*` spans are added, so highlighting cannot turn source text into page markup.

## Commenting

1. Click the **`+` gutter** — or anywhere on a commentable line, or on a rendered Markdown block; the gutter is a visual affordance, the entire valid line is the target. An editor opens with **Write | Preview** tabs and an anchor header.
2. Type; **Ctrl/Cmd+Enter** saves, **Escape** cancels. Empty text is discarded. Comment text is **markdown**; Preview uses the same `pd_markdown_html` engine as saved cards.
3. Saved comments appear under the line with **Edit** / **Resolve** / **Delete**. Delete is a two-step inline confirm; everything remains undoable.
4. Lines with comments show a count in the gutter; file headers (and the pinned row's context) show per-file counts that jump to the file's first comment.

**Resolving.** A resolved comment dims in place (hover restores legibility) and its meta line records when; **Reopen** brings it back. The chrome count splits — `3 comments · 1 open` — whenever some comments are resolved. Resolution travels with exports and imports like any other comment state.

**The review outcome.** **Comment** leaves no verdict action. **Approve** or **Require changes** applies a verdict to the whole change and contributes exactly one final Activity action; switching between those verdicts replaces that action rather than adding another. Returning to Comment removes it. Verdicts are exported (JSON and the Markdown header), merged on import (the later decision wins), and handled by Undo.

Drafts persist per anchor in the browser-preferences record (never the portable review document): they survive view-mode switches and reloads, and reopen visibly at their anchors on load.

Comments whose anchors are not in the current rendering (e.g. a page regenerated with different context) appear in an **Unanchored comments** section at the top, still editable and deletable.

## Expanding hunk context

Every gap the diff hides — above a hunk, between hunks, after the last one — carries a quiet expander row. A click reveals up to 20 unchanged lines adjacent to the diff; repeated clicks keep going, and the control retires when the gap is empty. Revealed lines are real context rows: commentable, numbered on both sides, present in unified and side-by-side views alike.

The data comes from the embedded endpoint snapshots (`pd_context_slice`), and the engine verifies the requested region is byte-identical at both endpoints — expansion can never invent or hide a change. Files whose contents were not snapshotted (binary, oversized) do not offer expansion.

## Filtering by commits

When the range has two or more commits, commit rows become selectable (snapshots are embedded for any non-empty range, but a single commit has nothing to filter):

- **Click** a commit to inspect it alone; **click a second commit** to span the range between them; click the selected commit again to clear. **Press-and-drag** across rows selects a range in one gesture — the view applies when the gesture completes, with no separate Apply step.
- **Prev / Next** on the range bar step through single commits; **Show full diff** clears the selection.
- The bar shows `Showing commits i–j of N` and reminds you the sub-range is **read-only** — commenting there shows a toast pointing back to the full diff.

Sub-range diffs are computed inside the page (`pd_range_diff` over the embedded snapshots — a pure Myers line diff). Renames are not re-detected within a sub-range: a rename shows as a delete plus an add.

## Review changes, Activity, and Undo

Material review changes — comments added, updated, resolved, reopened, or deleted, the single final verdict action, and files marked viewed — are journaled as stable-ID operations in their own `…:activity` record. The pinned row shows the change count (a link to the Activity section) and **Undo**, which inverts the most recent operation through the same engine calls that made it. View preferences (wrap, theme, collapse, drafts) are comfort state, not journal material.

## Review progress (viewed files)

Mark a file **Viewed** on its header. The state is local to this browser and is **not** exported with review feedback; toggles are journaled and undoable.

## Markdown

Markdown appears in three places, all rendered by `packdiff-dto::markdown` (build-time for file previews and the Description; WASM `pd_markdown_html` for comments and the editor preview):

- **Comment bodies.**
- **Markdown files** — open in the **Rendered** view, built from the diff hunks (added runs tinted green, removed red); the **Rendered | Source** pill switches to the diff. The choice persists per file.
- **The Description panel** — **Rendered | Source**, both commentable (rendered blocks anchor to the source line they start at).

The dialect is deliberately a subset: ATX headings, fenced code blocks, nested ordered and unordered lists, blockquotes, thematic breaks, paragraphs; inline `` `code` ``, `**bold**`, `*italic*`, and `[links](https://…)` (`http` / `https` / `mailto` targets only). Underscores are NOT emphasis, so `snake_case` identifiers survive verbatim. A single newline inside a paragraph is a hard break, matching how people write review comments. Every input character is HTML-escaped first — hostile input cannot smuggle markup or `javascript:` URLs.

## Where review state lives

All state is keyed by **review identity**: a fingerprint of the canonical reviewable diff content (repository name plus the typed file diffs — not the refs, filename, title, or generation timestamp), computed at build time and embedded in the page config as `review_id`. Renaming, moving, or regenerating the HTML for an identical diff finds the same state; a different diff starts clean — comments are never fuzzily relocated between diffs (carry them over with export → import instead).

Three sibling localStorage records, keyed by identity **and schema generation** (`v2` today):

| Key | Contents |
| --- | --- |
| `packdiff:v2:diff:<review_id>` | The portable review document (comments, verdict) — what export/import round-trips ([spec](DATA-MODEL.md#reviewdocument)) |
| `…:prefs` | Browser preferences: viewed files, per-file wrap, collapse state, Markdown view choices, theme, drafts. Never exported. |
| `…:activity` | The review-change journal behind Undo and the Activity section |

A page only ever writes the generation its engine speaks. On load it **absorbs** every older store for the same diff — earlier generations (`packdiff:v1:diff:<review_id>`) and the pre-`review_id` SHA-pinned key — by engine merge (union by id, newer wins), re-absorbing a source whenever its content changes. The old stores are never written or removed, so a page generated by an older packdiff keeps working against its own store, and anything written there flows forward the next time a current page opens. Still: prefer regenerating pages after upgrading — an old page cannot show verdicts or resolution state written by a new one.

If localStorage is unavailable, a visible alert warns that comments last only for this page view — export before closing.

## Actions menu

| Action | Effect |
| --- | --- |
| **Copy as Markdown** | The review as Markdown, grouped by file, onto the clipboard — the primary completion action. |
| **Copy as JSON** | The lossless, re-importable review document. |
| **Import JSON** | Merge a review in by comment id; newer `updated_at` wins. The toast reports how many comments landed and how many fell outside this diff (those appear in the unanchored section). |
| **Expand all / Collapse all** | Open or close every file panel; persisted, like per-file collapse. |
| **Theme** | System (default, follows `prefers-color-scheme`) / Light / Dark. |
| **Keyboard shortcuts** | Opens the shortcut reference (same as `?`). |

## Keyboard shortcuts

Press **`?`** when focus is not in an input:

| Key | Action |
| --- | --- |
| `j` / `k` | Next / previous file |
| `n` / `p` | Next / previous comment |
| `Esc` | Close dialogs / cancel editor |
| Ctrl/⌘+Enter | Save comment |

Every shortcut has a visible control equivalent, and single-key shortcuts are inert while typing.

## Navigation behavior

Explicit navigation (section links, breadcrumbs, comment jumps) scrolls to the exact target and pushes history; passive scrolling updates the URL fragment with `replaceState`, so a copied URL stays useful without polluting Back/Forward. Programmatic landings get a subtle, temporary highlight. The **← Back** control returns to the page that opened the review (or closes a standalone window when the browser permits).

## Browser support

Any current Chrome, Firefox, or Safari. Requirements: WebAssembly, `BigInt`, and (for persistence) localStorage. If WebAssembly is blocked, the page still renders the full diff read-only and shows a “Comment engine failed to load” alert under the chrome.
