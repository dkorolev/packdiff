# CLI reference — `packdiff`

```
packdiff <BASE> [HEAD] [OPTIONS]
packdiff <BASE>...<HEAD> [OPTIONS]     merge-base diff (the default semantics)
packdiff <BASE>..<HEAD> [OPTIONS]      literal two-dot diff
```

Packs the diff between two git refs into one self-contained HTML review page. The page makes zero network requests and opens from `file://`; the in-page comment engine (the `packdiff-dto` crate compiled to WASM) is inlined into the file, so the output is complete on its own.

## Refs and ranges

- **Two positionals** — `packdiff main feature`.
- **One positional** — `HEAD` (the current checkout) is the head: `packdiff main` reviews your working branch against `main`.
- **Git-style range** — `packdiff main...feature` (merge-base, same as the default) or `packdiff main..feature` (literal). Matching git's own `diff` semantics, `..` implies `--no-merge-base`. Git forbids `..` inside ref names, so the split is unambiguous — tags like `v1.0.0` work fine.

Ref names are resolved with `rev-parse --verify <ref>^{commit}`, so anything git accepts works: `main`, `origin/main`, `v1.2.0`, `HEAD~3`, a full or abbreviated SHA. `HEAD` is additionally accepted case-insensitively (`head^^^^` resolves as `HEAD^^^^`, with a stderr note) — as a fallback only, so a real ref literally named `head` still wins. Ref names are data, not syntax, so this leniency applies in machine mode too.

## Options

| Option | Meaning | Default |
| --- | --- | --- |
| `-C`, `--repo REPO` | Path to the git repository (any path inside the work tree works; the repo *name* in the output is the toplevel directory name) | `.` |
| `-o`, `--out OUT` | Output HTML path. Parent directories are created. `-` streams the HTML to stdout (suppresses the human summary line) | `packdiff-<base>-<head>.html` in the current directory, ref names sanitized (`origin/main` → `origin-main`) |
| `--context N` | Context lines around each hunk (`git diff -U<N>`) | `3` |
| `--no-merge-base` | Diff `BASE..HEAD` literally (two-dot). Default is PR semantics: `merge-base(BASE, HEAD)..HEAD` (three-dot), so commits that landed on BASE after the branch point do not pollute the review | merge-base mode |
| `--title T` | Override the page `<title>`/heading | `repo: BASE → HEAD` |
| `--dump-json PATH` | Additionally write the full typed [`DiffDocument`](DATA-MODEL.md#diffdocument) as pretty JSON — the machine-readable form of everything on the page | off |
| `--json` | Force machine mode (see below). Machine mode is auto-enabled whenever stdout is not a terminal | auto |
| `--open` | After writing, open the page in the default browser (`xdg-open`/`open`; failure is a warning, not an error) | off |
| `--color MODE` | `auto` \| `always` \| `never`. `auto` colors only a terminal and honors `NO_COLOR` | `auto` |
| `--verbose` | Echo git invocations and their timing to stderr (a small whitelist, not a firehose) | off |
| `-V`, `--version` | Print `packdiff <version>` and exit 0 (works offline) | — |
| `-h`, `--help`, `help` | Print comprehensive usage and exit 0; so does running with no arguments. `help exitcodes` prints the exit-code table | — |

## Liberal for humans, canonical for machines

Help always shows the one canonical form, and at a terminal the CLI accepts unambiguous non-canonical spellings (`--no-color`, `-v`, bare `version`), resolving them and printing the canonical form to stderr as a dim nudge. In machine mode (piped stdout or `--json`) a non-canonical invocation does NOT run: the CLI exits 2 with a `NonCanonicalInvocation` document naming the canonical syntax. Aliases in scripts rot; this keeps them out.

## Exit codes

`packdiff help exitcodes` prints this same table.

| Code | Stage | Meaning |
| --- | --- | --- |
| `0` | — | Success |
| `2` | `usage` | Malformed or (in machine mode) non-canonical invocation |
| `3` | `repo` | `--repo` does not point inside a git repository |
| `4` | `ref` | `BASE` or `HEAD` did not resolve to a commit |
| `5` | `git` / `io` | git failed, hung (watchdog), or a local I/O failure |
| `130` | — | Interrupted (Ctrl+C); a broken pipe instead ends the program quietly |

The `stage` string rides inside every machine-mode error document, so scripts can branch on it without memorizing codes.

## Machine mode: the output contract

Machine mode is on whenever `--json` is given OR stdout is not a terminal — scripts and pipelines get machine-readable output for free. Every run then emits exactly ONE two-space-indented, single-key union document on stdout, success and failure alike; the exit code remains the authoritative signal.

Success — `{ "Packed": { ... } }`:

```json
{
  "Packed": {
    "out": "review.html",
    "repo": "myrepo",
    "base": { "name": "main", "sha": "<40-hex>" },
    "head": { "name": "my-feature", "sha": "<40-hex>" },
    "merge_base": "<40-hex>",
    "commits": 5,
    "files": 12,
    "additions": 345,
    "deletions": 67,
    "binary_files": 0,
    "warnings": []
  }
}
```

Failure — a request-specific variant with `stage` and `exit_code` inside:

```json
{
  "UnknownRef": {
    "repo": "~/src/myrepo",
    "ref": "no-such-branch",
    "message": "unknown ref in \"~/src/myrepo\": no-such-branch",
    "stage": "ref",
    "exit_code": 4
  }
}
```

Variants: `Packed`, `UsageError`, `NonCanonicalInvocation`, `NotAGitRepository`, `UnknownRef`, `GitError`, `IoError`. Scripts branch on the top-level key (`jq -r 'keys[0]'`) or on `stage`.

Warnings land in BOTH channels: the `warnings` array in the document AND stderr lines, so neither scripts nor humans miss them.

Two exceptions, by design: `-o -` streams the HTML page itself as the stdout data and prints no document (`--json -o -` is refused — both would claim stdout), and `help`/`--version` print their text as-is.

At a terminal without `--json`, errors go to stderr as `error: <message>` and success prints one human line — `Wrote packdiff-main-feature.html (5 files, +6 −1, 2 commits)` — colored per `--color`.

## Liveness and progress

Every run reports live progress **on stderr** (stdout stays reserved for data), in the mode-appropriate form:

- **At a terminal**: an [indicatif](https://crates.io/crates/indicatif) progress bar with the current stage, work counts, and the estimated time remaining. It clears itself on completion, leaving only the one-line summary. Hidden automatically when stderr is redirected.
- **In machine mode** (`--json` or piped stdout): one single-key `{ "Progress": { ... } }` JSON document per line — emitted immediately at every stage change and at least once per second in between — so a harness always knows the stage, the counts, and the ETA:

```json
{ "Progress": { "stage": "Snapshots", "detail": "blob 439f6df7", "done": 36, "total": 50, "elapsed_ms": 2010, "eta_ms": 781 } }
```

Fields: `stage` ∈ `Resolve | MergeBase | Diff | Commits | Snapshots | Render | Write | Done`; `detail` is the current work item (absent between items); `done`/`total` are work units, where `total` grows as snapshot work is discovered and never shrinks; `elapsed_ms` counts from run start; `eta_ms` is the linear extrapolation (absent until at least one unit is done). The stream ends with a `Done` report where `done == total` — a failed run never reports `Done`, so its absence plus the exit code is a reliable failure signal.

Separately, every git invocation runs under a watchdog: after 10 seconds of runtime a status line goes to stderr every ~10 s (`packdiff: \`git diff ...\` still running (20s elapsed)`), and a process that produces NO output for 5 minutes is killed with a `GitError` explaining that silence.

## Examples

```console
# Review the branch you are on against main, and open it.
$ packdiff main --open
Wrote packdiff-main-HEAD.html (12 files, +345 −67, 5 commits)

# Review a feature branch of another repo, PR-style, explicit output.
$ packdiff main my-feature -C ~/src/myrepo -o review.html
Wrote review.html (12 files, +345 −67, 5 commits)

# Remote-only branches work with their qualified names.
$ packdiff origin/main origin/pr-123 -C ~/src/myrepo

# Literal two-branch difference (includes changes that only landed on BASE).
$ packdiff main..my-feature

# Agent pipeline: piped stdout is already machine mode; add the typed diff too.
$ packdiff main my-feature --dump-json diff.json -o review.html | jq -r 'keys[0]'
Packed

# Stream the page (e.g. straight into another tool).
$ packdiff v1.0.0...v1.1.0 -o - > /tmp/release-diff.html
```

## Behavior details

- **Renames** are detected (`--find-renames`) and shown as `old → new`; the comment anchor path is always the post-image path.
- **Binary files** appear with a "contents not shown" notice and count toward `binary_files` in the summary.
- **Identical refs / empty range** produce a valid page stating "No changes between these refs." and a summary with zero files — not an error.
- The commit list covers `merge_base..HEAD` (or `BASE..HEAD` in two-dot mode), oldest first.
- `generated_at` is stamped once per run (RFC 3339 UTC); it is the only non-deterministic byte source in the output — two runs against the same SHAs differ only in that field.
