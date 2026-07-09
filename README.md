# `packdiff`

Dependency-free zero-latency browser-first github-looking diff for `git`.

Rationale:

1. Browser is a good enough diff tool.
2. Github is far too slow.
3. Not everything is worthy of pushing.

So this tool is mostly to make it more comfortable to review (and comment on!) agents-generated code.

---

**Turn any two branches of a git repository into one self-contained HTML review page — with line comments that live in your browser and export as JSON, Markdown, or CSV.**

```console
$ packdiff main my-feature -C ~/src/myrepo -o review.html
Wrote review.html (12 files, +345 −67, 5 commits)
```

Open `review.html` anywhere — from disk (`file://`), over any static host, or mailed to a colleague. The page makes **zero network requests**: CSS, the diff, and the comment engine are all inside the one file.

- **Click any diff line to comment** (Ctrl/Cmd+Enter saves; markdown supported) — in rendered markdown previews, click a block to comment on the source line it starts at. Comments persist in the browser's localStorage, keyed to the exact repo + commit SHAs.
- **Markdown files render**: `.md` files in the diff open in the rendered markdown view (added lines tinted green, removed lines red); a two-sided **Preview | Diff** pill switches to the diff text and back.
- **Filter by commits**: click a commit to see only its diff, click a second to select the range between them (computed in-page by the WASM engine), and copy any commit's full hash with one button.
- **Navigate quickly**: a sticky nav with live counts (commits · files · LOC delta) and a *Files changed* index that jumps straight to any file's diff.
- **Unified or side-by-side**: a nav toggle switches the whole diff between unified and split views (enabled once the window is wide enough for the font size); commenting works on either.
- **Export** the review as lossless JSON, Markdown grouped by file, or RFC 4180 CSV — and **import** JSON back on another machine (merge by comment id, newer edit wins).
- **PR-style diffs by default**: `merge-base(BASE, HEAD)..HEAD`, so drift on the base branch doesn't pollute the review (`--no-merge-base` for the literal two-dot diff).
- **A PR description panel**: commits authored by the notes author (`PACKDIFF_SYSTEM_USER_EMAIL`) are notes, not code — they are hidden from the page, and the `PR-DESCRIPTION.md` they committed renders as a commentable **Description** panel on top, like the pull request it will become.
- **Live progress**: a progress bar with ETA at a terminal; in machine mode one `{ "Progress": ... }` JSON line per second on stderr, ending with `Done` — agents always know the stage and what remains.
- **Script- and agent-native**: piped stdout automatically switches to machine mode — exactly one two-space JSON document per run, success and failure alike, as single-key unions (`{ "Packed": { … } }`, `{ "UnknownRef": { … } }`) — plus `--dump-json` for the full typed diff document and a documented exit-code table (`packdiff help exitcodes`).

## How it works

The interesting part of the design: the **data model is one Rust crate, compiled twice**. The CLI links it natively to parse git output; every generated page carries it compiled to WebAssembly (~124 KB, base64-inlined, no wasm-bindgen, empty import object) as the in-browser comment engine. The page's JavaScript is a view layer only — validation, ordering, merge semantics, and export formats are defined once, in Rust, and run identically in tests, in the CLI, and in your browser.

```
packdiff/
├── dto/      the data model (DTOs): schema types, unified-diff parser, comment
│             operations, exports — pure logic, no I/O, no clock
├── wasm/     thin C ABI over dto/, built for wasm32-unknown-unknown
├── cli/      the `packdiff` binary: git + HTML rendering; embeds the wasm
├── docs/     reference documentation
└── tests/    Node test driving the real .wasm exactly like the page does
```

## Install

Prerequisites: stable Rust with the wasm32 target, and git.

```console
$ rustup target add wasm32-unknown-unknown
```

Straight from this repository:

```console
$ cargo install --git https://github.com/dkorolev/packdiff packdiff
$ packdiff --version
packdiff 0.1.2
```

From crates.io (once the crates are published there):

```console
$ cargo install packdiff
```

From a local checkout:

```console
$ git clone https://github.com/dkorolev/packdiff && cd packdiff
$ cargo build --release            # builds the wasm module too (cli/build.rs)
$ cargo install --path cli         # puts `packdiff` on your PATH
```

To publish to crates.io (maintainer note; order matters — the CLI's build script pulls the wasm engine from the registry). The full playbook, including how the shared version is kept in one place, is in [PUBLISHING.md](PUBLISHING.md):

```console
$ cargo publish -p packdiff-dto
$ cargo publish -p packdiff-wasm
$ cargo publish -p packdiff
```

To see it end to end without pointing at your own repo:

```console
$ ./demo.sh
Wrote out/demo.html (2 files, +5 −2, 2 commits)
Open out/demo.html in a browser (file:// is fine). Click a diff line to comment.
```

## Usage

```
packdiff <BASE> [HEAD] [OPTIONS]
packdiff <BASE>...<HEAD> [OPTIONS]     merge-base diff (the default semantics)
packdiff <BASE>..<HEAD> [OPTIONS]      literal two-dot diff
```

`HEAD` defaults to whatever is checked out, so the everyday form is just `packdiff main`. Git-style ranges work the way you expect: `..` means the literal diff, `...` the merge-base diff.

| Option | Meaning | Default |
| --- | --- | --- |
| `-C`, `--repo` | Path to the git repository | `.` |
| `-o`, `--out` | Output HTML path; `-` for stdout | `packdiff-<base>-<head>.html` |
| `--context N` | Diff context lines | `3` |
| `--no-merge-base` | Literal `BASE..HEAD` instead of PR semantics | off |
| `--title T` | Override the page title | `repo: BASE → HEAD` |
| `--dump-json PATH` | Also write the typed diff document as JSON | off |
| `--json` | Force machine mode (auto when stdout is not a terminal) | auto |
| `--open` | Open the generated page in the default browser | off |
| `--color MODE` | `auto` \| `always` \| `never` (`NO_COLOR` honored) | `auto` |
| `--verbose` | Echo git invocations and timing to stderr | off |

Humans at a terminal may use relaxed spellings (`--no-color`, bare `version`); the CLI resolves them and nudges toward the canonical form. Scripts must use canonical syntax — machine mode refuses aliases.

Exit codes: `0` success · `2` usage · `3` not a git repository · `4` unknown ref · `5` git/io failure · `130` interrupted — `packdiff help exitcodes` has the full table, and every machine-mode error carries a `stage` field.

## Use as a library

The binary's two halves — extract a typed diff document from git, render it into the one-file page — are the crate's public Rust API:

```rust,no_run
let opts = packdiff::PackOptions::new(".", "main", "HEAD");
let out = packdiff::pack(&opts, &())?; // &(): no progress reporting
std::fs::write("review.html", &out.html)?;
```

```toml
[dependencies]
packdiff = { version = "0.2", default-features = false }
```

`default-features = false` drops the binary-only terminal machinery (`indicatif`). The build-time wasm prerequisite above still applies, and `git` must be on `PATH` at run time. The typed data model is re-exported as `packdiff::dto`; progress can be observed by implementing `packdiff::progress::ProgressObserver` (`&()` observes nothing). `cargo run --example pack -- main` runs the [worked example](cli/examples/pack.rs), and consumers that only read packdiff's artifacts — exported comments, `--dump-json` documents — need just the pure-logic [`packdiff-dto`](dto/) crate.

## Documentation

| Doc | Contents |
| --- | --- |
| [docs/CLI.md](docs/CLI.md) | Full CLI reference: options, exit codes, the `--json` contract, examples |
| [docs/PAGE.md](docs/PAGE.md) | Using the review page: commenting, storage, exports/imports, carrying comments across regenerated diffs |
| [docs/DATA-MODEL.md](docs/DATA-MODEL.md) | The schema spec: document shapes, validation, ordering, merge semantics, storage key, versioning |
| [docs/WASM-ABI.md](docs/WASM-ABI.md) | The WASM ABI: memory protocol, envelopes, every export, a minimal JS bridge |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate boundaries, the wasm-embedding build pipeline, testing strategy |
| [PUBLISHING.md](PUBLISHING.md) | Maintainer playbook: crate order, version reconciliation, the dry run, releasing to crates.io |

Rust API docs: `cargo doc -p packdiff -p packdiff-dto --open`.

## Tests and gates

```console
$ ./test.sh
```

Runs `cargo fmt --check`, the dto unit tests, the CLI integration tests (driving the real binary against a scratch git repository, including the machine-mode contract), release-mode tests, and the Node tests exercising the actual compiled wasm module under the page's exact calling convention. A clean checkout stays green — tests missing a prerequisite skip with a hint.

The same pass gates pushes (`git config core.hooksPath .githooks` enables the pre-push hook) and merges (GitHub Actions CI) — see [CONTRIBUTING.md](CONTRIBUTING.md).

## Limitations (v0.1)

No syntax or word-level highlighting; the side-by-side view needs a wide-enough window; comments carry across regenerated diffs via export → import rather than automatically; localStorage is per-browser-profile by nature.

## License

MIT — see [LICENSE](LICENSE). © 2026 Dmitry "Dima" Korolev.
