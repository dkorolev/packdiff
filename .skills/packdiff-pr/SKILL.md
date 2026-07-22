---
name: packdiff-pr
description: Generates a self-contained packdiff HTML review page from a real GitHub pull request. Confirms `gh` is installed and authenticated first, clones the PR's repository into a temporary directory, fetches the PR head, commits the PR's GitHub description as the fake `PR-DESCRIPTION.md` notes commit so the offline page carries the Description panel, then runs `packdiff` with PR (merge-base) semantics and cleans the clone up. Use when the user invokes packdiff-pr, /packdiff-pr, or asks to turn a GitHub PR or PR URL into a packdiff page.
---

# packdiff-pr — pack a real GitHub pull request into one offline HTML page

Turn a GitHub pull request into a single self-contained `packdiff` review page.
The page must be complete offline — including the PR's description, which lives
on GitHub, not in the repository. That is what the fake notes commit in step 4
is for: packdiff lifts a commit touching nothing but `PR-DESCRIPTION.md` into a
commentable **Description** panel and hides it from the commit list (see
[docs/CLI.md](../../docs/CLI.md), "The notes-commit convention").

Follow the steps in order. Every step that can fail tells you how to stop.

## 1. Preflight — do not skip

- **`gh` is installed**: `command -v gh`. If missing, stop and tell the user to
  install it (`brew install gh` on macOS, <https://cli.github.com> otherwise).
- **`gh` is authenticated**: `gh auth status` must exit 0. If it does not, stop
  and ask the user to run `gh auth login` themselves (in Claude Code, suggest
  typing `! gh auth login` so the interactive login runs in-session).
- **`packdiff` is available**: prefer `command -v packdiff`. If absent and the
  current repo is packdiff itself, `cargo build --release` and use
  `target/release/packdiff`. Otherwise stop and point at the README install
  section (`cargo install packdiff`).

## 2. Resolve the pull request

Accept any of: a full URL (`https://github.com/OWNER/REPO/pull/N`),
`OWNER/REPO#N`, or a bare number (then take `OWNER/REPO` from
`gh repo view --json nameWithOwner` in the current directory; if that fails,
ask the user which repository the PR belongs to). PRs live in the base
repository, so `OWNER/REPO` is the repo to clone — never the fork.

Fetch the metadata once and keep it:

```sh
gh pr view <N-or-URL> --repo OWNER/REPO \
  --json number,title,body,state,isDraft,baseRefName,headRefName,url
```

Any state works — open, draft, closed, or merged; `pull/N/head` stays
fetchable after merge, and merge-base semantics keep a moved-on base branch
from polluting the diff.

## 3. Clone and fetch the PR head

Work in a temporary directory (the session scratchpad if there is one), never
inside the user's working tree:

```sh
gh repo clone OWNER/REPO "$TMP/pr-clone" -- --quiet
git -C "$TMP/pr-clone" fetch --quiet origin "pull/N/head:packdiff-pr-N"
```

A full clone, deliberately: merge-base needs real history (shallow clones break
it), and a blob-filtered partial clone would make packdiff's snapshot pass
lazy-fetch over the network. `pull/N/head` also covers PRs from forks — no
extra remote needed.

## 4. The fake `PR-DESCRIPTION.md` notes commit — do not forget it

The clone does not contain the PR's description; GitHub holds it. Bake it in:

1. `git -C "$TMP/pr-clone" checkout --quiet packdiff-pr-N`
2. Write `PR-DESCRIPTION.md` at the repository root — the PR title as an `# `
   heading, a blank line, then the body **verbatim** (write it from the JSON
   field programmatically; never through shell interpolation, which mangles
   quotes and backticks). If the body is empty, write
   `_No description provided._` under the heading.
3. If the file already exists at the PR head with identical content, skip the
   commit — it is already a notes commit and committing nothing fails.
4. Otherwise commit **only that file** — confinement is what makes it a notes
   commit; anything else riding along keeps it on the page as code:

   ```sh
   git -C "$TMP/pr-clone" add PR-DESCRIPTION.md
   git -C "$TMP/pr-clone" -c user.name="packdiff-pr" -c user.email="packdiff-pr@localhost" \
     commit --quiet -m "PR #N description."
   ```

If the branch already carried a *different* `PR-DESCRIPTION.md`, the page will
show the standing several-versions banner with the GitHub body as `current` —
that is truthful (the in-repo draft diverged from the PR), so keep it and
mention it in the report rather than suppressing either version.

## 5. Generate the page

```sh
packdiff "origin/BASE_REF" "packdiff-pr-N" -C "$TMP/pr-clone" \
  -o "packdiff-OWNER-REPO-pr-N.html" --title "OWNER/REPO#N: TITLE"
```

`BASE_REF` is the PR's `baseRefName`. The default merge-base semantics are
exactly PR semantics — do not pass `--no-merge-base`. Write the output into
the user's current directory unless they named a path. Pass the title as one
argument; PR titles contain quotes and backticks, so build the argument
programmatically, not by string-splicing into a shell line.

Piped stdout puts packdiff in machine mode: capture the one JSON document and
branch on its top-level key (`Packed` on success; on failure the variant's
`message` says what went wrong).

## 6. Verify, report, clean up

- In the `Packed` document, confirm `description` is `"PR-DESCRIPTION.md"` and
  `notes_commits` is non-empty — that is the proof the description panel made
  it into the page. If `description` is `null`, the notes commit went wrong
  (most likely something else got committed with it); fix and regenerate
  rather than shipping a page without its description.
- Report the output path plus the `files` / `additions` / `deletions` /
  `commits` counts and the PR state, and surface any `warnings`.
- Delete the temporary clone (`rm -rf "$TMP/pr-clone"`). The HTML page is
  self-contained; nothing else needs to survive.
- Offer to open the page (`open` / `xdg-open`) — do not open it unasked.
