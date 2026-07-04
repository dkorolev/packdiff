# `packdiff`

Dependency-free zero-latency browser-first github-looking diff for `git`.

Rationale:

1. Browser is a good enough diff tool.
2. Github is far too slow.
3. Not everything is worthy of pushing.

So this tool is mostly to make it more comfortable to review (and comment on!) agents-generated code.

---

**Turn any two branches of a git repository into one self-contained HTML
review page — with line comments that live in your browser and export as
JSON, Markdown, or CSV.**

```console
$ packdiff main my-feature -C ~/src/myrepo -o review.html
Wrote review.html (12 files, +345 −67, 5 commits)
```

Open `review.html` anywhere — from disk (`file://`), over any static host, or
mailed to a colleague. The page makes **zero network requests**: CSS, the
diff, and the comment engine are all inside the one file.

- **Click any diff line to comment** (Ctrl/Cmd+Enter saves; markdown
  supported). Comments persist in the browser's localStorage, keyed to the
  exact repo + commit SHAs.
- **Markdown files render**: `.md` files in the diff get a *View rendered*
  toggle switching between the diff text and the rendered markdown view.
- **Filter by commits**: click a commit to see only its diff, shift-click to
  select a contiguous range (computed in-page by the WASM engine), and copy
  any commit's full hash with one button.
- **Export** the review as lossless JSON, Markdown grouped by file, or
  RFC 4180 CSV — and **import** JSON back on another machine (merge by
  comment id, newer edit wins).
- **PR-style diffs by default**: `merge-base(BASE, HEAD)..HEAD`, so drift on
  the base branch doesn't pollute the review (`--no-merge-base` for the
  literal two-dot diff).
- **Script- and agent-native**: piped stdout automatically switches to
  machine mode — exactly one two-space JSON document per run, success and
  failure alike, as single-key unions (`{ "Packed": { … } }`,
  `{ "UnknownRef": { … } }`) — plus `--dump-json` for the full typed diff
  document and a documented exit-code table (`packdiff help exitcodes`).

## How it works

The interesting part of the design: the **data model is one Rust crate,
compiled twice**. The CLI links it natively to parse git output; every
generated page carries it compiled to WebAssembly (~124 KB, base64-inlined,
no wasm-bindgen, empty import object) as the in-browser comment engine. The
page's JavaScript is a view layer only — validation, ordering, merge
semantics, and export formats are defined once, in Rust, and run identically
in tests, in the CLI, and in your browser.

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
packdiff 0.1.0
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

To publish to crates.io (maintainer note; order matters — the CLI's build
script pulls the wasm engine from the registry):

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

`HEAD` defaults to whatever is checked out, so the everyday form is just
`packdiff main`. Git-style ranges work the way you expect: `..` means the
literal diff, `...` the merge-base diff.

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

Humans at a terminal may use relaxed spellings (`--no-color`, bare
`version`); the CLI resolves them and nudges toward the canonical form.
Scripts must use canonical syntax — machine mode refuses aliases.

Exit codes: `0` success · `2` usage · `3` not a git repository · `4` unknown
ref · `5` git/io failure · `130` interrupted — `packdiff help exitcodes` has
the full table, and every machine-mode error carries a `stage` field.

## Documentation

| Doc | Contents |
| --- | --- |
| [docs/cli.md](docs/cli.md) | Full CLI reference: options, exit codes, the `--json` contract, examples |
| [docs/page.md](docs/page.md) | Using the review page: commenting, storage, exports/imports, carrying comments across regenerated diffs |
| [docs/data-model.md](docs/data-model.md) | The schema spec: document shapes, validation, ordering, merge semantics, storage key, versioning |
| [docs/wasm-abi.md](docs/wasm-abi.md) | The WASM ABI: memory protocol, envelopes, every export, a minimal JS bridge |
| [docs/architecture.md](docs/architecture.md) | Crate boundaries, the wasm-embedding build pipeline, testing strategy |

Rust API docs: `cargo doc -p packdiff-dto --open`.

## Tests and gates

```console
$ ./test.sh
```

Runs `cargo fmt --check`, the dto unit tests, the CLI integration tests
(driving the real binary against a scratch git repository, including the
machine-mode contract), release-mode tests, and the Node tests exercising the
actual compiled wasm module under the page's exact calling convention. A
clean checkout stays green — tests missing a prerequisite skip with a hint.

The same pass gates pushes (`git config core.hooksPath .githooks` enables the
pre-push hook) and merges (GitHub Actions CI) — see
[CONTRIBUTING.md](CONTRIBUTING.md).

## Limitations (v0.1)

Unified diff view only (no side-by-side, no syntax or word-level
highlighting); comments carry across regenerated diffs via export → import
rather than automatically; localStorage is per-browser-profile by nature.

## License

MIT — see [LICENSE](LICENSE). © 2026 Dmitry "Dima" Korolev.
