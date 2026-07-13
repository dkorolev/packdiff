# Architecture

## The one idea

Ship the **same compiled data model to both sides of the tool**. The `packdiff-dto` crate is pure logic (no I/O, no clock, no entropy); the CLI links it natively to parse git output, and every generated page carries it compiled to WebAssembly as the comment engine. There is exactly one implementation of validation, ordering, merge, and export semantics — the page's JavaScript is a dumb view layer that round-trips JSON through WASM calls and touches only the DOM and localStorage.

```
             ┌────────────────────── dto/  (packdiff-dto) ──────────────────────┐
             │ schema types · diff parser · highlighting · comment ops · exports  │
             └──────────────┬───────────────────────────────┬───────────────────── ┘
                    native  │                        wasm32 │  (no_std not needed;
                            ▼                               ▼   no imports either)
             cli/  (binary `packdiff`)            wasm/  (packdiff-wasm, cdylib)
             git subprocess → DiffDocument       C ABI: pd_* over UTF-8 JSON buffers
             → render HTML/CSS/JS                        │
                            │                            │ base64
                            └────────► one HTML file ◄───┘
                                       (works from file://, zero requests)
```

## Crates

| Crate | Dir | Kind | Deps | Role |
| --- | --- | --- | --- | --- |
| `packdiff-dto` | `dto/` | rlib | serde, serde_json | All data + semantics ([spec](DATA-MODEL.md)) |
| `packdiff-wasm` | `wasm/` | cdylib+rlib | dto, serde, serde_json | Transport shim only ([ABI](WASM-ABI.md)) |
| `packdiff` | `cli/` | bin `packdiff` | dto, serde_json | argv, git, HTML rendering ([CLI](CLI.md)) |

Boundary rules that keep the focus honest:

- `dto/` never does I/O; time and ids are function inputs.
- `wasm/` contains no semantics — every function is parse-envelope → one dto call → serialize-envelope.
- `cli/` contains no data semantics — `git.rs` shells out and returns raw text; `parse_unified_diff` (dto) interprets it; `render.rs` is presentation (escaping, tables, inlining).

## Build pipeline

`cli/build.rs` makes a plain `cargo build`/`cargo test` self-sufficient from a clean checkout:

1. It invokes `cargo build -p packdiff-wasm --release --target wasm32-unknown-unknown` with a **separate `--target-dir target-wasm/`** — separate because the outer cargo holds a lock on `target/`, and a nested build into the same dir would deadlock.
2. It passes the artifact path to the CLI via the `PACKDIFF_WASM_PATH` rustc-env; `main.rs` does `include_bytes!(env!("PACKDIFF_WASM_PATH"))`.
3. `rerun-if-changed` on `wasm/src` and `dto/src` keeps the embedded module fresh.

The workspace release profile is size-tuned (`opt-level="z"`, `lto`, `panic="abort"`, `strip`, one codegen unit) because the wasm module ships inside every page. The lexical highlighter increased the module from 242,395 to 270,250 bytes raw (+27,855 bytes, 11.5%) and from 323,196 to 360,336 bytes base64. Note `panic="abort"` means `cargo test --release` won't work; tests run in the default dev profile.

### Toolchain note

The only prerequisite beyond stable Rust is the wasm32 std: `rustup target add wasm32-unknown-unknown`. If your system carries a distro cargo/rustc pair that shadows rustup's (symptom: `check-cfg` flag-mismatch errors, or thousands of "can't find crate" errors from the wasm build), make sure the rustup toolchain is first on PATH — the nested cargo in `build.rs` resolves `rustc` from the environment it inherits.

## The generated page

Authored frontend assets live in `cli/assets/page.css` and `cli/assets/page.js` and are embedded at compile time with `include_str!`. `render.rs` assembles a single HTML document containing, in order: inline CSS, the review chrome and diff markup (all Git-derived strings HTML-escaped at render time), and script tags:

1. `#packdiff-config` (`application/json`) — repo, refs+SHAs, merge-base, generation time, and the content-fingerprint `review_id` (with `<` escaped to `\u003c`).
2. `#packdiff-snapshots` (optional) — commit-boundary blob snapshots for in-page range diffs.
3. `#packdiff-wasm` (`application/wasm-base64`) — the module bytes.
4. The view-layer JS: decode base64 → `WebAssembly.instantiate(bytes, {})` → derive the storage key from the config's `review_id` and schema generation (absorbing older-generation stores, including the legacy `pd_storage_key`-keyed one, by engine merge) → load/normalize the stored document via `pd_parse_document` → render comment rows; then delegate every user action to the corresponding `pd_*` call and re-render from its result.

UI preferences (wrap, collapse state, Markdown views, theme, viewed files, drafts) use a separate `…:prefs` localStorage record, and the undo journal a `…:activity` record; neither enters the portable review document.

Diff rows carry the anchor as data attributes (`data-file`, `data-side`, `data-line`), which is the entire coupling between the rendered diff and the review model. Comment creation targets the explicit gutter `+` control.

## Testing strategy

Each layer is tested through its real interface, and the heavier layers test the layers beneath them again end-to-end:

| Layer | Where | What it proves |
| --- | --- | --- |
| model | `dto/src/*` unit tests (60) | Parser line-numbering and statuses, safe lexical highlighting and parallel hunk state, validation, ordering, merge conflict rules, schema rejection, export shapes, canonical round-trips |
| CLI | `cli/tests/cli.rs` (real binary + scratch git repo) | merge-base vs two-dot semantics, `--json`/`--dump-json` contracts, exit codes 2/3/4, highlighted HTML, self-containment, XSS escaping, wasm inlining (skips with a hint if git is absent) |
| WASM | `tests/wasm_abi.test.mjs` (Node ≥18) | The real `.wasm` under the exact page calling convention: alloc/free protocol, envelopes, highlighting and every other export, error paths (skips with a hint until built) |
| Browser UI | `tests/browser/ui.spec.mjs` (Playwright) | Generated HTML over `file://`: chrome, gutter comments, highlighting in split/range/expanded-context views, Preview/Diff, viewed state, range filter, no network after load (installs Chromium on first run) |

`./test.sh` runs all of it, plus `cargo fmt --check` and a release-mode test pass. The same script is the pre-push hook (`.githooks/pre-push`; enable with `git config core.hooksPath .githooks`) and the CI gate (`.github/workflows/ci.yml`). A clean checkout stays green: tests missing a prerequisite (git, Node) skip with a hint rather than fail.

CI runs two further jobs that need the network and so stay out of `test.sh`: **package** verify-builds all three crate tarballs with the cli's build script on its registry-shim path (the crates.io install path — see [PUBLISHING.md](../PUBLISHING.md)), and **semver** runs `cargo semver-checks` on `packdiff-dto` against the latest published release, so a breaking API change cannot ride a patch-level bump into the auto-publishing merge.

## Web layer stance

**Strict Rust for the engine, vanilla JS for the player.** The boundary is drawn by responsibility, not by size. The **engine** is everything with review or source semantics — validation, ordering, merge, exports, markdown, lexical highlighting, storage keys, range diffs — strongly-typed Rust in `packdiff-dto`, compiled to WASM for the page. The litmus test: if a behavior changes what an export contains, what a stored review document means, or how source text is classified, it is engine work and lands in Rust. The **player** — `cli/assets/page.js` — is deliberately vanilla JavaScript owning presentation and browser state only: DOM assembly, event wiring, view preferences (wrap, theme, viewed files, drafts), localStorage I/O. The player may be substantial — a review workspace has a lot of view — but it stays framework-free, build-step-free, and semantics-free: it round-trips JSON through `pd_*` calls and never edits the review document itself. This split is a settled decision, not a migration way-station — the player is not waiting to be rewritten in `wasm-bindgen`.

## External processes and liveness

git is the one external process. Every invocation is fallible and liveness-bounded (`cli/src/git.rs`): output is drained on reader threads, a watchdog kills the process after 5 minutes of total silence, and once a command exceeds 10 seconds a status line is emitted to stderr every ~10 s so callers can tell packdiff is alive. `--verbose` additionally echoes each git command line and its wall time.

Above that, `cli/src/progress.rs` reports whole-run progress on stderr: an indicatif bar (stage, counts, ETA) at a terminal, and in machine mode one `{ "Progress": { ... } }` JSON document per line — on every stage change and at least once per second — ending with a `Done` report only on success (see [CLI.md](CLI.md#liveness-and-progress)).

## Publishing

The three crates go to crates.io in dependency order:

```console
$ cargo publish -p packdiff-dto
$ cargo publish -p packdiff-wasm
$ cargo publish -p packdiff
```

The `packdiff` crate's verification build (and every user's `cargo install packdiff`) compiles the wasm engine via the shim in cli/build.rs against the registry, so the two library crates must exist there first. For a full pre-publish dry run before anything is on the registry:

```console
$ PACKDIFF_WASM_SRC=$PWD/wasm cargo package --workspace
```

which verifies all three tarballs with the shim pointed at the local wasm crate instead of crates.io.

The shared version is defined once in `[workspace.package]`, and the internal `packdiff-dto` dependency is pinned once in `[workspace.dependencies]` (the `cli` and `wasm` crates reference it as `{ workspace = true }`). The full release playbook — version reconciliation, ordering, and the crates.io policy — is in [../PUBLISHING.md](../PUBLISHING.md).
