# packdiff UI concepts

Please follow the shared [engineering principles](https://github.com/dkorolev/principles/blob/main/ENG-PRINCIPLES.md) and [web UI principles](https://github.com/dkorolev/principles/blob/main/WEB-UI-PRINCIPLES.md) first. This document records packdiff-specific product decisions and deferred ideas; it does not restate the shared rules.

## The review is a document

- Present the review in a human-readable sequence: Description, Commits, Files changed, Diff, and, when implemented, Activity.
- Put a plain-language review summary above the navigation. It scrolls away; only one stable navigation row remains pinned.
- The Files changed section is the file index. Do not duplicate it with a permanent sidebar.
- Details such as repository, refs, merge-base, generation time, tool version, and schema belong near the summary in disclosure that scrolls away.

## Pinned navigation and file context

- Keep the pinned navigation to one row. As the user scrolls through the diff, reserved space in that row may show the current file's path, additions, deletions, comment and draft counts, Viewed state, Markdown presentation, and wrapping choice.
- Render the current path as horizontally scrollable breadcrumb segments. Show the most specific end initially; each ancestor is actionable.
- Selecting a directory breadcrumb lands on the relevant directory rows in Files changed and briefly highlights the matching files.
- A nonzero file comment count is orange and navigates directly to that file's first comment. Zero comments occupy reserved space without a visible label. Drafts use a distinct, non-color-only indicator.
- Keep Unified / Side-by-side prominent. When side-by-side cannot fit, leave the control in place but disabled with: “Not enough horizontal space to render side-by-side.”

## Diff and file interaction

- Keep the modern `+` comment gutter, but make the entire valid diff line a comment target.
- Wrap long lines by default. Make `Wrap | Scroll` a persistent per-file choice.
- Use the same persistent `Rendered | Source` choice for Markdown files and the Description section.
- File headers remain one stable row. Keep path, stats, Viewed, comment/draft counts, and relevant view controls; omit previous/next arrows.
- Files and top-level sections start expanded. Persist explicit collapse state. A collapsed file with review data shows comment and draft counts.
- Reopen persisted drafts visibly at their original anchors.
- When a direct inline action expands or contracts content, preserve its local interaction anchor when naturally possible; do not globally fight scrolling.

## Commit ranges

- Model selection as From / To endpoints, not independent commit checkboxes.
- Support press-and-drag across commit rows, with two endpoint clicks as the precise and keyboard-accessible equivalent.
- Render the selected range when the gesture completes; do not add an Apply step.
- If a range excludes comments, report how many are outside it and provide a direct Show full diff action.
- Treat applying a range as browser-history-worthy view navigation. Back restores the prior range, viewport, and focus.

## Local review identity and transfer

- Identify local review state by a hash of the canonical reviewable diff content. Exclude HTML filename, generated timestamp, page title, and visual presentation from identity.
- An identical diff restores identical local state even when its HTML file is renamed, moved, or regenerated. A different diff starts with clean state; do not fuzzily relocate comments between diffs.
- For now, the only outward transfer actions are `Copy as Markdown` and `Copy as JSON`, under Actions. Do not expose download or JSON-import flows.
- Default to the system theme. Put a compact `System | Light | Dark` chooser one click away under Actions.

## Review changes, local state, and Activity

- Keep two durable tiers in the data model:
  - **Review changes** are material, auditable, reversible, and eventually pushable: comments and their resolution, the review verdict, Viewed status, deletion/restoration, and future shared events.
- Review state mimics GitHub (decision 2026-07-13): the entire change carries one verdict — Approved or ChangesRequired — and individual comments are resolvable. There is no per-comment kind taxonomy.
  - **Local state** is durable comfort and recovery data: drafts, wrapping, Markdown presentation, collapse state, theme, and similar preferences.
- Reserve a stable pinned-row status for the number of Review changes and Undo. Clicking the count eventually navigates to a real Activity section, not a drawer.
- Activity should minimally but visibly distinguish Review changes from Local state. Its detailed UX is not designed yet.
- Model material changes as stable-ID journal operations rather than whole document replacement. Keep the schema compatible with deterministic, CRDT-style merging of future new comments and commits; do not implement sync yet.
- Confirm data-sensitive mutations inline and maintain a persistent per-diff undo ledger. Draft keystrokes are local checkpoints, not material Activity events.

## Navigation behavior

- Explicit navigation scrolls and focuses the exact named target, updates the URL, and uses `pushState`.
- Passive scrolling updates the current URL fragment with `replaceState` so a copied URL remains useful without polluting Back/Forward history.
- Give programmatic landings a subtle, temporary highlight.
- Persist presentation choices without adding them to browser history or the material mutation ledger.

## Messages and shortcuts

- Operational messages must not reflow the document. Success gets brief local feedback; warnings and errors may collapse but leave a visible status until acknowledged.
- Keep keyboard navigation, with every action also available by pointer/touch. Disable unmodified single-key shortcuts in typing contexts. Surface important Command/Control accelerators at their actions and list important single-key shortcuts at the bottom of Actions.

## Deferred deliberately

- The final Activity UI, upstream synchronization, CRDT merging, file filters, next-unviewed navigation, repository-authored collapsed-first defaults, and rich commit metadata placement remain TODOs.
- If filtering is added, it must execute instantly in the local JavaScript/WASM player. There is no filtering UI in the current packdiff scope.
