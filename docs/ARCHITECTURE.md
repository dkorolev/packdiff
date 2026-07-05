# Architecture

## The one idea

Ship the **same compiled data model to both sides of the tool**. The `packdiff-dto` crate is pure logic (no I/O, no clock, no entropy); the CLI links it natively to parse git output, and every generated page carries it compiled to WebAssembly as the comment engine. There is exactly one implementation of validation, ordering, merge, and export semantics — the page's JavaScript is a dumb view layer that round-trips JSON through WASM calls and touches only the DOM and localStorage.

```
             ┌────────────────────── dto/  (packdiff-dto) ──────────────────────┐
             │  schema types · unified-diff parser · comment ops · exports · key   │
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

The workspace release profile is size-tuned (`opt-level="z"`, `lto`, `panic="abort"`, `strip`, one codegen unit) because the wasm module ships inside every page: ~124 KB raw, ~165 KB as base64. Note `panic="abort"` means `cargo test --release` won't work; tests run in the default dev profile.

### Toolchain note

The only prerequisite beyond stable Rust is the wasm32 std: `rustup target add wasm32-unknown-unknown`. If your system carries a distro cargo/rustc pair that shadows rustup's (symptom: `check-cfg` flag-mismatch errors, or thousands of "can't find crate" errors from the wasm build), make sure the rustup toolchain is first on PATH — the nested cargo in `build.rs` resolves `rustc` from the environment it inherits.

## The generated page

`render.rs` emits a single HTML document containing, in order: inline CSS (light/dark via `prefers-color-scheme`), the header/commits/files markup (all crawled strings HTML-escaped at render time), and three `<script>` tags:

1. `#packdiff-config` (`application/json`) — repo, refs+SHAs, merge-base (with `</` escaped to `<\/`).
2. `#packdiff-wasm` (`application/wasm-base64`) — the module bytes.
3. The view-layer JS: decode base64 → `WebAssembly.instantiate(bytes, {})` → derive the storage key via `pd_storage_key` → load/normalize the stored document via `pd_parse_document` → render comment rows; then delegate every user action to the corresponding `pd_*` call and re-render from its result.

Diff rows carry the anchor as data attributes (`data-file`, `data-side`, `data-line`), which is the entire coupling between the rendered diff and the review model.

## Testing strategy

Each layer is tested through its real interface, and the heavier layers test the layers beneath them again end-to-end:

| Layer | Where | What it proves |
| --- | --- | --- |
| model | `dto/src/*` unit tests (43) | Parser line-numbering and statuses, validation, ordering, merge conflict rules, schema rejection, export shapes, canonical round-trips |
| CLI | `cli/tests/cli.rs` (8, real binary + scratch git repo) | merge-base vs two-dot semantics, `--json`/`--dump-json` contracts, exit codes 2/3/4, HTML self-containment, XSS escaping, wasm inlining (skips with a hint if git is absent) |
| WASM | `tests/wasm_abi.test.mjs` (9, Node ≥18) | The real `.wasm` under the exact page calling convention: alloc/free protocol, envelopes, every export, error paths (skips with a hint until built) |

`./test.sh` runs all of it, plus `cargo fmt --check` and a release-mode test pass. The same script is the pre-push hook (`.githooks/pre-push`; enable with `git config core.hooksPath .githooks`) and the CI gate (`.github/workflows/ci.yml`). A clean checkout stays green: tests missing a prerequisite (git, Node) skip with a hint rather than fail.

## Web layer stance

The page follows a binary heuristic: anything with real logic or state is strongly-typed Rust compiled to WASM (the whole review model); what remains in JavaScript is deliberately trivial view-glue — DOM assembly, event wiring, localStorage I/O — kept framework-free and small. Nothing lives in between.

## External processes and liveness

git is the one external process. Every invocation is fallible and liveness-bounded (`cli/src/git.rs`): output is drained on reader threads, a watchdog kills the process after 5 minutes of total silence, and once a command exceeds 10 seconds a status line is emitted to stderr every ~10 s so callers can tell packdiff is alive. `--verbose` additionally echoes each git command line and its wall time.

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
