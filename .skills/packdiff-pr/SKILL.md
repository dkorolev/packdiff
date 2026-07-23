---
name: packdiff-pr
description: Generates a self-contained packdiff HTML review page from a real GitHub pull request, plus the PR's review discussion as two sidecar files. Confirms `gh` is installed and authenticated first, clones the PR's repository into a temporary directory, fetches the PR head, commits the PR's GitHub description as the fake `PR-DESCRIPTION.md` notes commit so the offline page carries the Description panel, runs `packdiff` with PR (merge-base) semantics, then fetches the PR's review threads, reviews, and conversation and maps them through map-discussion.mjs into review.json (for the page's Import JSON) and discussion.md (for agents and humans), and cleans the clone up. Use when the user invokes packdiff-pr, /packdiff-pr, or asks to turn a GitHub PR or PR URL into a packdiff page.
---

# packdiff-pr — pack a real GitHub pull request into one offline HTML page

Turn a GitHub pull request into a single self-contained `packdiff` review page.
The page must be complete offline — including the PR's description, which lives
on GitHub, not in the repository. That is what the fake notes commit in step 4
is for: packdiff lifts a commit touching nothing but `PR-DESCRIPTION.md` into a
commentable **Description** panel and hides it from the commit list (see
[docs/CLI.md](../../docs/CLI.md), "The notes-commit convention").

The PR's review discussion comes along too (step 6): two sidecar files beside
the page — `review.json`, which Import JSON merges into the page as real
anchored comments, and `discussion.md`, the whole discussion for agents and
humans.

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
  --json number,title,body,state,isDraft,baseRefName,headRefName,url,reviewDecision
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

## 6. Import the review discussion — two sidecar files

The page now exists, but GitHub still holds the other half of the PR: the
review threads, the reviews, and the conversation. Ship them beside the page
as two files, both named after the page:

- `packdiff-OWNER-REPO-pr-N.review.json` — a schema v3 `ReviewDocument`. The
  user imports it via **Actions → Import JSON**; every line-anchored review
  thread lands as real comments at its `file:line` anchor, merged by the CRDT
  (resolved threads arrive resolved; re-importing after new discussion
  upserts instead of duplicating; local edits are never clobbered).
- `packdiff-OWNER-REPO-pr-N.discussion.md` — the whole discussion in one
  markdown file for agents and humans: reviews with their verdicts, the
  conversation tab, and every thread including outdated ones.

Fetch three datasets (paginate — big PRs overflow single pages):

```sh
gh api graphql --paginate --slurp \
  -F owner=OWNER -F repo=REPO -F number=N -f query='
  query($owner: String!, $repo: String!, $number: Int!, $endCursor: String) {
    repository(owner: $owner, name: $repo) {
      pullRequest(number: $number) {
        reviewThreads(first: 50, after: $endCursor) {
          pageInfo { hasNextPage endCursor }
          nodes {
            isResolved isOutdated path line originalLine diffSide
            comments(first: 100) {
              nodes { databaseId url body createdAt lastEditedAt
                      author { login } } } } } } } }' > "$TMP/raw-threads.json"
gh api --paginate "repos/OWNER/REPO/pulls/N/reviews" --slurp > "$TMP/raw-reviews.json"
gh api --paginate "repos/OWNER/REPO/issues/N/comments" --slurp > "$TMP/raw-conversation.json"
```

Assemble the bundle programmatically (a small script or `jq` — never shell
string-splicing; bodies contain quotes and backticks):

```sh
jq -n \
  --slurpfile threads "$TMP/raw-threads.json" \
  --slurpfile reviews "$TMP/raw-reviews.json" \
  --slurpfile conversation "$TMP/raw-conversation.json" \
  --argjson pr "$PR_META" '{
    pr: $pr,
    threads: [$threads[0][].data.repository.pullRequest.reviewThreads.nodes[]],
    reviews: [$reviews[0][][]],
    conversation: [$conversation[0][][]]
  }' > "$TMP/bundle.json"
```

`$PR_META` is `{ number, title, url, state, reviewDecision, repo, base:
{ name, sha }, head: { name, sha } }` — number/title/url/state/reviewDecision
from the step-2 `gh pr view` call (add `reviewDecision` to its `--json` list),
`repo` the repository name, and the SHAs from the clone:
`git -C "$TMP/pr-clone" rev-parse "origin/BASE_REF"` and `… rev-parse
"packdiff-pr-N"`.

Then map — the transform lives beside this skill and prints a machine-mode
summary:

```sh
node "$SKILL_DIR/map-discussion.mjs" "$TMP/bundle.json" \
  "packdiff-OWNER-REPO-pr-N.review.json" "packdiff-OWNER-REPO-pr-N.discussion.md"
```

Mapping rules the script implements (documented in its header): comment ids
are `github-rc-<databaseId>` so re-imports upsert; actors are
`github:<login>`; `isResolved` becomes `resolved_at`; an outdated thread
anchors to its original line and the page shows it in the "outside this diff"
section; **the PR's `reviewDecision` becomes the document verdict** —
`APPROVED` → Approved, `CHANGES_REQUESTED` → Require changes, stamped at the
newest review carrying that state, anything else leaves the verdict unset.
Review summaries and conversation comments have no diff anchor, so they are
in `discussion.md` only.

If all three datasets are empty (a PR with no discussion), skip the sidecar
files and say so in the report rather than shipping empty artifacts.

## 7. Verify, report, clean up

- In the `Packed` document, confirm `description` is `"PR-DESCRIPTION.md"` and
  `notes_commits` is non-empty — that is the proof the description panel made
  it into the page. If `description` is `null`, the notes commit went wrong
  (most likely something else got committed with it); fix and regenerate
  rather than shipping a page without its description.
- Report the output path plus the `files` / `additions` / `deletions` /
  `commits` counts and the PR state, and surface any `warnings`.
- Report the two sidecar files with the `Mapped` counts from
  map-discussion.mjs (threads, comments, reviews, conversation, verdict), and
  tell the user the one click that remains: **Actions → Import JSON** on the
  page, pointing at the `review.json`. If `fallback_anchored` is non-zero,
  say that many threads are outdated on GitHub and will appear in the page's
  "outside this diff" section.
- Delete the temporary clone (`rm -rf "$TMP/pr-clone"`) and the raw/bundle
  JSON intermediates. The page and its two sidecars are the only artifacts
  that survive.
- Offer to open the page (`open` / `xdg-open`) — do not open it unasked.
