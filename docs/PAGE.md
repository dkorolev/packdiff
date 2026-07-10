# Using the review page

The HTML file packdiff writes is the whole product: one file, no server, no network. Open it from disk (`file://`) or serve it — either works. Everything below happens locally in your browser.

## Layout

- **Sticky chrome** — repository identity (`base → head`), live file and line-delta counts, comment count (opens the review summary), unified/side-by-side toggle, wrap preference, and menus for **Actions** (export/import, theme, shortcuts) and **Details** (full SHAs, merge-base, generation time, tool version).
- **File sidebar** — persistent on wide screens (collapsible), drawer on medium/mobile. Search, status filters, comment counts, viewed checkboxes, and `N of M files viewed` progress. Click a row to jump to that file.
- **Main workspace** — Description (when present), Commits, then the file diffs. The first changed file is near the top of a typical desktop viewport.
- **Sticky file headers** — status, path, `+/−`, comment count, Markdown Preview/Diff, viewed checkbox, copy-path, and prev/next file controls.

A plain “Files changed” list remains in the document as a no-JavaScript fallback; with JS enabled, the sidebar is the primary navigator.

## Unified vs side-by-side

The **Side-by-side** control toggles the whole diff between the default unified view and a two-column split (old on the left, new on the right). It is available only when the **diff workspace** is at least ~90em wide relative to the current font size; on narrow layouts the control is hidden. Commenting works in both views via the gutter. Split tables are built in the browser from the unified rows.

**Wrap** toggles between horizontal scrolling (default) and soft wrapping of long lines (`overflow-wrap: anywhere`, not `break-all`).

## Filtering by commits

When the range has two or more commits and snapshots are present:

- Use the **checkbox** on a commit row (or click the row) to select it; Shift-click or multi-check spans a contiguous range.
- **Prev / Next** on the range bar step through single commits.
- **Show full diff** clears the selection.
- Selection uses the accent color (not addition green). The bar shows `Showing commits i–j of N` and reminds you the sub-range is **read-only**.

Sub-range diffs are computed inside the page (`pd_range_diff`). Comments attach only to the full diff.

## Commenting

1. Click the **`+` gutter** on a commentable line (or on a rendered Markdown block). An editor opens with Write/Preview tabs, an anchor header, and **Add comment** / **Cancel**.
2. Type; **Ctrl/Cmd+Enter** saves, **Escape** cancels. Empty text is discarded. Comment text is **markdown**; Preview uses the same `pd_markdown_html` engine as saved cards.
3. Saved comments appear under the line with Edit / Delete. Lines with comments show a count in the gutter.
4. The chrome comment count opens the **Review summary** drawer: every comment once, grouped by file, with jump links and export.

Whole-line code clicks no longer open editors (so selecting text is safe). Unanchored comments appear in the summary and in an in-page unanchored section when their lines are missing from the rendering.

Drafts survive view-mode switches and short reloads via a separate browser-preferences record (not the portable review document).

## Review progress (viewed files)

Mark a file **Viewed** on its header. State is local to this browser, keyed to the same repo + endpoint SHAs as comments, and is **not** exported with review feedback. Use **Next unviewed** and **Hide viewed** in the sidebar footer.

## Markdown

Markdown appears in two places, both rendered by `packdiff-dto::markdown` (build-time for file previews; WASM `pd_markdown_html` for comments and editor preview):

- **Comment bodies**
- **Markdown files** — default to Preview; the **Preview | Diff** pill switches views while preserving scroll position approximately
- **Description panel** — PR-like card when a notes commit is present; **Preview | Raw** switches between rendered markdown and the source (line-numbered, still commentable via gutters)

## Where comments live

In this browser's localStorage, under `packdiff:v1:<repo>:<baseSha12>..<headSha12>` ([spec](DATA-MODEL.md#storage-key)).

UI preferences (sidebar open, wrap, theme, viewed files, drafts) use a sibling key `…:prefs` and never alter review JSON exports.

### Unanchored comments

Comments whose anchors are not in the current rendering stay editable/deletable in the review summary (and an in-page unanchored list).

## Toolbar / Actions menu

| Action | Effect |
| --- | --- |
| **Copy Markdown** | Primary completion action (also in the summary drawer). |
| **Export JSON** | Lossless, re-importable review document. |
| **Export Markdown** | Comments grouped by file. |
| **Export CSV** | RFC 4180 spreadsheet. |
| **Import JSON** | Merge by comment id; newer `updated_at` wins. |

Download names: `packdiff-comments-<repo>-<baseSha7>-<headSha7>.{json,md,csv}`.

## Keyboard shortcuts

Press **`?`** when focus is not in an input:

| Key | Action |
| --- | --- |
| `j` / `k` | Next / previous file |
| `n` / `p` | Next / previous comment |
| `f` | Focus file search (opens the drawer on narrow layouts) |
| `Esc` | Close dialogs / cancel editor |
| Ctrl/⌘+Enter | Save comment |

Every shortcut has a visible control equivalent.

## Theme

**Actions → Theme** offers System (default), Light, and Dark. System follows `prefers-color-scheme`.

## Browser support

Any current Chrome, Firefox, or Safari. Requirements: WebAssembly, `BigInt`, and (for persistence) localStorage. If WebAssembly is blocked, the page still renders the full diff read-only and shows a “Comment engine failed to load” alert under the chrome.
