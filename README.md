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

- **Comment from the gutter** (`+` on every commentable line, or the line itself; Ctrl/Cmd+Enter saves; markdown with Write/Preview) — works in unified, split, and rendered Markdown. Comments persist in localStorage, keyed to the exact diff content, so a regenerated identical diff keeps them.
- **GitHub-shaped review state**: choose **Comment**, **Approve**, or **Require changes**, and resolve individual comments. Comment adds no verdict action; either verdict contributes one final action and is carried by exports and imports (the later decision wins on merge).
- **Document-first layout**: a top-pinned navigation row, then Summary, Description, Commits, Files changed, Diff, and Activity in one readable sequence; the row tracks the current file (breadcrumbs, stats, comment count) as you scroll, with Undo and a review-change journal one click away.
- **Markdown files render**: `.md` files open in the rendered view (added green, removed red); a **Rendered | Source** pill switches while preserving place.
- **Filter by commits**: click two endpoints or drag across commit rows to select a commit or range (computed in-page by the WASM engine); range views are clearly read-only.
- **Unified or side-by-side**, plus a per-file **Wrap | Scroll** choice; split enables when the workspace is wide enough. Hunk gaps **expand in place** — 20 unchanged lines per click, commentable like any other line.
- **Syntax highlighting everywhere code appears**: one safe Rust lexical scanner covers Rust, C/C++, Java, Kotlin, Go, Python, JavaScript/TypeScript, Ruby, shell, SQL, CSS, TOML, YAML, and JSON; unknown languages remain plain text.
- **Copy** the review out as lossless JSON or Markdown from the Actions menu — and **import** JSON back. The merge is a real CRDT join (Lamport-versioned registers, tombstones): exchange order never changes where two reviews converge, and a deleted comment stays deleted.
- **PR-style diffs by default**: `merge-base(BASE, HEAD)..HEAD`, so drift on the base branch doesn't pollute the review (`--no-merge-base` for the literal two-dot diff).
- **A PR description panel, and journaled decisions**: a commit that touches nothing but `PR-DESCRIPTION.md` is notes, not code, whoever authored it — it is hidden from the page, and the file renders as a commentable **Description** panel on top, like the pull request it will become. `PR-DECISION-<topic>.md` files lift the same way into a **Decisions** section below it: what was decided while the change was made, and why, commentable alongside the code. Write the description in more than one commit and the page says so in a standing banner, then shows every version — newest first, each commentable on its own.
- **Live progress**: a progress bar with ETA at a terminal; in machine mode one `{ "Progress": ... }` JSON line per second on stderr, ending with `Done` — agents always know the stage and what remains.
- **Script- and agent-native**: piped stdout automatically switches to machine mode — exactly one two-space JSON document per run, success and failure alike, as single-key unions (`{ "Packed": { … } }`, `{ "UnknownRef": { … } }`) — plus `--dump-json` for the full typed diff document and a documented exit-code table (`packdiff help exitcodes`).
- **A `packdiff-pr` agent skill ships in-repo**: point a coding agent at a GitHub pull request and it packs the whole PR into one offline page — cloning into a temporary directory, baking the PR's GitHub description in as the notes commit so the Description panel survives offline, and cleaning up after itself. [`.skills/`](.skills/README.md) is the canonical skill source, symlinked into Claude Code, Cursor, Codex, and OpenCode discovery paths.

## How it works

The interesting part of the design: the **data model is one Rust crate, compiled twice**. The CLI links it natively to parse git output and highlight code; every generated page carries it compiled to WebAssembly (base64-inlined, no wasm-bindgen, empty import object) as the in-browser comment and highlighting engine. The page's JavaScript is a view layer only — validation, ordering, merge semantics, export formats, and lexical highlighting are defined once, in Rust, and run identically in tests, in the CLI, and in your browser.

That boundary is deliberate and permanent: **strict Rust for the engine, vanilla JS for the player**. The player (`cli/assets/page.js`) owns presentation and browser state — DOM, events, wrap/theme/viewed/drafts, localStorage — and stays framework-free and build-step-free; anything that touches review semantics lives in Rust and reaches the page only through the WASM ABI ([the stance, spelled out](docs/ARCHITECTURE.md#web-layer-stance)).

```
packdiff/
├── dto/      the data model (DTOs): schema types, unified-diff parser, comment
│             operations, exports — pure logic, no I/O, no clock
├── wasm/     thin C ABI over dto/, built for wasm32-unknown-unknown
├── cli/      the `packdiff` binary: git + HTML rendering; embeds the wasm
│   └── assets/  authored page CSS/JS (inlined at compile time)
├── docs/     reference documentation
└── tests/    wasm ABI tests + Playwright browser UI tests
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
packdiff 0.8.1
```

From crates.io:

```console
$ cargo install packdiff
```

From a local checkout:

```console
$ git clone https://github.com/dkorolev/packdiff && cd packdiff
$ cargo build --release            # builds the wasm module too (cli/build.rs)
$ cargo install --path cli         # puts `packdiff` on your PATH
```

Publishing to crates.io is automated (maintainer note): merging a PR whose last commit bumps the workspace version publishes the crates in dependency order and tags the GitHub release. The full playbook, including the manual fallback and how the shared version is kept in one place, is in [PUBLISHING.md](PUBLISHING.md).

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
packdiff = { version = "0.8", default-features = false }
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

## Limitations

No syntax or word-level highlighting; the side-by-side view needs a wide-enough window; comments carry across *changed* diffs via export → import (an identical regenerated diff keeps them automatically); localStorage is per-browser-profile by nature.

## License

MIT — see [LICENSE](LICENSE). © 2026 Dmitry "Dima" Korolev.
