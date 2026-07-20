//! packdiff as a library: build a typed [`DiffDocument`] from a git
//! repository and render it into the one-file self-contained HTML review
//! page — the same two steps the `packdiff` binary performs.
//!
//! [`pack`] is the one-call path; [`build_document`] and [`render_page`] are
//! its two halves, for callers that also want the typed document (what the
//! CLI's `--dump-json` writes). Progress lands on a caller-supplied
//! [`progress::ProgressObserver`]; `&()` reports nothing. Depend with
//! `default-features = false` to drop the binary-only terminal machinery
//! (`indicatif`).
//!
//! Requirements: `git` on `PATH` at run time, and the
//! `wasm32-unknown-unknown` target at build time — the page's comment engine
//! is compiled into this crate (see the README's install section).
//!
//! The data model lives in its own pure-logic crate and is re-exported here
//! as [`dto`], so callers need no separate version-matched `packdiff-dto`
//! dependency.
//!
//! ```no_run
//! let opts = packdiff::PackOptions::new(".", "main", "HEAD");
//! let out = packdiff::pack(&opts, &())?; // &(): no progress reporting
//! std::fs::write("review.html", &out.html)?;
//! println!("{} files changed", out.document.files.len());
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod git;
pub mod progress;
mod render;

/// The typed data model ([`packdiff_dto`]), re-exported for callers.
pub use packdiff_dto as dto;

pub use git::CliError as Error;

use packdiff_dto::diff::{DiffDocument, NotesFile};
use packdiff_dto::snapshot::{Boundary, RangeSnapshots};
use packdiff_dto::RefInfo;
use progress::{ProgressObserver, Stage};

/// The compiled comment engine, inlined into every generated page.
const WASM: &[u8] = include_bytes!(env!("PACKDIFF_WASM_PATH"));

/// Blobs larger than this are not snapshotted; sub-range diffs render such
/// files as "contents not shown" (the binary-file treatment).
const MAX_SNAPSHOT_BLOB_BYTES: usize = 2 * 1024 * 1024;

/// The default notes-author email (see [`PackOptions::notes_email`]): the
/// identity that commits PR notes such as `PR-DESCRIPTION.md`, per the
/// notes-commit convention. The CLI overrides it from the
/// `PACKDIFF_SYSTEM_USER_EMAIL` environment variable.
pub const DEFAULT_SYSTEM_USER_EMAIL: &str = "dmitry.korolev+elon-presley@gmail.com";

/// The future pull request's description, lifted into its own commentable
/// page panel.
const NOTES_DESCRIPTION_PATH: &str = "PR-DESCRIPTION.md";

/// Journaled decisions are `PR-DECISION-<topic>.md` at the repository root:
/// what was decided while the change was made, and why. Each becomes its own
/// commentable panel, so a reviewer reads the reasoning instead of
/// re-deriving it from the diff.
const NOTES_DECISION_PREFIX: &str = "PR-DECISION-";
const NOTES_DECISION_SUFFIX: &str = ".md";

/// Whether a repository-relative path is a journaled decision. The topic must
/// be non-empty and the file must sit at the root — a `PR-DECISION-*.md`
/// nested under a directory is ordinary documentation under review, not a
/// notes file lifted off the page.
fn is_decision_path(path: &str) -> bool {
  !path.contains('/')
    && path.starts_with(NOTES_DECISION_PREFIX)
    && path.ends_with(NOTES_DECISION_SUFFIX)
    && path.len() > NOTES_DECISION_PREFIX.len() + NOTES_DECISION_SUFFIX.len()
}

/// Whether a path is any notes file: the description or a journaled decision.
fn is_notes_path(path: &str) -> bool {
  path == NOTES_DESCRIPTION_PATH || is_decision_path(path)
}

/// What to pack: the repository, the two refs, and the diff semantics.
/// Construct with [`PackOptions::new`] (the CLI's defaults), then adjust the
/// public fields as needed.
#[non_exhaustive]
pub struct PackOptions {
  /// Path to the git repository: the work tree root or any directory inside it.
  pub repo: String,
  /// Base ref: branch, tag, or SHA.
  pub base: String,
  /// Head ref: branch, tag, or SHA.
  pub head: String,
  /// `true` (the default): PR semantics, diff `merge-base(base, head)..head`,
  /// so drift on the base branch does not pollute the review. `false`: the
  /// literal two-dot diff `base..head`.
  pub merge_base: bool,
  /// Unchanged context lines around each hunk (default 3).
  pub context: u32,
  /// Page title override; `None` derives "repo: base → head".
  pub title: Option<String>,
  /// The notes-author email. Commits authored by it are notes, not code
  /// under review: they are hidden from the commit list, and the
  /// `PR-DESCRIPTION.md` they carry is lifted out of the diff into the
  /// page's commentable Description panel. `None` disables the convention.
  /// Defaults to [`DEFAULT_SYSTEM_USER_EMAIL`].
  pub notes_email: Option<String>,
}

impl PackOptions {
  /// Options for diffing `base` against `head` in `repo`, with the CLI's
  /// defaults: merge-base semantics, 3 context lines, derived title.
  pub fn new(repo: impl Into<String>, base: impl Into<String>, head: impl Into<String>) -> Self {
    Self {
      repo: repo.into(),
      base: base.into(),
      head: head.into(),
      merge_base: true,
      context: 3,
      title: None,
      notes_email: Some(DEFAULT_SYSTEM_USER_EMAIL.to_string()),
    }
  }
}

/// Everything one [`pack`] call produced.
#[non_exhaustive]
pub struct PackOutput {
  /// The typed diff document (what the CLI's `--dump-json` writes).
  pub document: DiffDocument,
  /// The self-contained HTML review page (what the CLI writes to `--out`).
  pub html: String,
}

/// When enabled, every git invocation is echoed to stderr with its timing
/// (the CLI's `--verbose`). Global, off by default.
pub fn set_verbose(enabled: bool) {
  git::VERBOSE.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// File contents at every commit boundary (deduplicated by blob id) — the
/// data behind the page's commit-range filter (two or more commits) and its
/// expand-context control (any non-empty range: the endpoint boundaries
/// carry the full file contents). `None` only for an empty range.
fn collect_snapshots(
  repo: &str, merge_base: &str, commits: &[packdiff_dto::diff::Commit], exclude: &[&str],
  progress: &dyn ProgressObserver,
) -> Result<Option<RangeSnapshots>, Error> {
  if commits.is_empty() {
    return Ok(None);
  }
  let mut boundary_shas = vec![merge_base.to_string()];
  boundary_shas.extend(commits.iter().map(|c| c.sha.clone()));
  // Both stages enter with their FULL item count known — one changed-paths
  // scan per adjacent pair plus one tree listing per boundary, then one
  // fetch per unique blob (countable once the listings exist) — so progress
  // moves linearly through each instead of chasing a growing total.
  progress.stage(Stage::Scan, (boundary_shas.len() - 1 + boundary_shas.len()) as u64);
  let mut tracked: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
  for pair in boundary_shas.windows(2) {
    tracked.extend(git::changed_paths(repo, &pair[0], &pair[1])?);
    progress.step(&format!("changes {}..{}", &pair[0][..7], &pair[1][..7]));
  }
  // A boundary pair spanning a notes commit picks up the lifted notes files;
  // they must not resurface in sub-range diffs.
  let paths: Vec<String> = tracked.into_iter().filter(|p| !exclude.contains(&p.as_str())).collect();
  // One long-lived `cat-file --batch-check` child for every boundary listing;
  // per-boundary progress ticks exactly as before.
  let mut trees = git::TreeReader::new(repo)?;
  let mut boundaries = Vec::with_capacity(boundary_shas.len());
  for sha in &boundary_shas {
    let files = trees.tree_blobs(sha, &paths)?;
    progress.step(&format!("boundary {}", &sha[..7]));
    boundaries.push(Boundary { sha: sha.clone(), files });
  }
  let ids: std::collections::BTreeSet<&String> = boundaries.iter().flat_map(|b| b.files.values()).collect();
  progress.stage(Stage::Snapshots, ids.len() as u64);
  // One long-lived `cat-file --batch` child for the whole pass; per-blob
  // progress ticks exactly as before.
  let mut reader = git::BlobReader::new(repo)?;
  let mut blobs = std::collections::BTreeMap::new();
  for id in ids {
    blobs.insert(id.clone(), reader.blob_text(id, MAX_SNAPSHOT_BLOB_BYTES)?);
    progress.step(&format!("blob {}", &id[..id.len().min(8)]));
  }
  Ok(Some(RangeSnapshots { blobs, boundaries }))
}

/// Partition the range's commits per the notes-commit convention: commits
/// authored by `notes_email` whose changes are CONFINED to notes files carry
/// notes — `PR-DESCRIPTION.md` and `PR-DECISION-<topic>.md` — not code under
/// review. Both halves of the test matter. Authorship alone is not enough:
/// the notes identity may also author code — a run orchestrator like scsh
/// integrates every agent commit under one bot identity — and code commits
/// must stay on the page no matter who authored them. Touching a notes file
/// alone is not enough either: user-authored notes must not be claimed, and
/// a mixed commit (code + notes in one) is code.
///
/// Returns the code commits, the lifted description, and the lifted
/// decisions ordered by path — each file's text as of the LAST notes commit
/// that carries it. With no notes email, no notes commits, or nothing
/// readable to lift, everything stays as it was (hiding commits without
/// lifting anything would lose history).
fn split_notes(
  repo: &str, commits: Vec<packdiff_dto::diff::Commit>, notes_email: Option<&str>,
) -> Result<(Vec<packdiff_dto::diff::Commit>, Option<NotesFile>, Vec<NotesFile>), Error> {
  let Some(email) = notes_email else { return Ok((commits, None, Vec::new())) };
  // Commits arrive oldest-first (`git log --reverse`), so `notes_shas` is too.
  let mut notes_shas = Vec::new();
  // Every notes path the range touches, deduplicated and ordered by path so
  // the panels render deterministically.
  let mut notes_paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
  for c in commits.iter().filter(|c| c.email == email) {
    // `sha^` can fail only for a parentless commit in the range (disjoint
    // histories in two-dot mode); such a commit is not a notes commit.
    let touched = git::changed_paths(repo, &format!("{}^", c.sha), &c.sha).unwrap_or_default();
    if !touched.is_empty() && touched.iter().all(|p| is_notes_path(p)) {
      notes_shas.push(c.sha.clone());
      notes_paths.extend(touched);
    }
  }
  if notes_shas.is_empty() {
    return Ok((commits, None, Vec::new()));
  }
  // A notes file's content is its state at the newest notes commit carrying
  // it; a later commit that deletes it leaves nothing to lift.
  let mut lifted: Vec<NotesFile> = Vec::new();
  for path in &notes_paths {
    let mut text = None;
    for sha in notes_shas.iter().rev() {
      let blobs = git::tree_blobs(repo, sha, std::slice::from_ref(path))?;
      if let Some(id) = blobs.get(path) {
        text = git::blob_text(repo, id, MAX_SNAPSHOT_BLOB_BYTES)?;
        if text.is_some() {
          break;
        }
      }
    }
    if let Some(text) = text {
      lifted.push(NotesFile { path: path.clone(), text, commits: Vec::new() });
    }
  }
  if lifted.is_empty() {
    return Ok((commits, None, Vec::new()));
  }
  let (notes, code): (Vec<_>, Vec<_>) = commits.into_iter().partition(|c| notes_shas.contains(&c.sha));
  // Provenance is the notes commits as a whole: one commit may carry the
  // description and a decision together, so the hidden shas belong to every
  // file lifted out of them.
  let notes_commit_shas: Vec<String> = notes.iter().map(|c| c.sha.clone()).collect();
  for file in &mut lifted {
    file.commits.clone_from(&notes_commit_shas);
  }
  let description = lifted.iter().position(|f| f.path == NOTES_DESCRIPTION_PATH).map(|i| lifted.remove(i));
  Ok((code, description, lifted))
}

/// Extract [`PackOptions`]'s diff from git into the typed document: resolve
/// the refs, diff, list the commits, lift the notes commits' description
/// per the notes-commit convention, and (for multi-commit ranges) snapshot
/// file contents at every commit boundary for the page's range filter.
pub fn build_document(opts: &PackOptions, progress: &dyn ProgressObserver) -> Result<DiffDocument, Error> {
  progress.stage(Stage::Resolve, 2);
  let base_sha = git::resolve_ref(&opts.repo, &opts.base)?;
  progress.step(&opts.base);
  let head_sha = git::resolve_ref(&opts.repo, &opts.head)?;
  progress.step(&opts.head);
  progress.stage(Stage::MergeBase, 1);
  let lo = if opts.merge_base { git::merge_base(&opts.repo, &base_sha, &head_sha)? } else { base_sha.clone() };
  progress.step("");
  progress.stage(Stage::Diff, 1);
  let mut files = packdiff_dto::diff::parse_unified_diff(&git::diff_text(&opts.repo, &lo, &head_sha, opts.context)?);
  progress.step("");
  progress.stage(Stage::Commits, 1);
  let commits = git::commits(&opts.repo, &lo, &head_sha)?;
  progress.step("");
  let (commits, description, decisions) = split_notes(&opts.repo, commits, opts.notes_email.as_deref())?;
  // Lifted notes leave the diff entirely: their files drop out of the file
  // list (and so out of the +/− totals), and out of the snapshot paths below
  // — the page shows each as its own panel instead.
  let exclude: Vec<&str> = description.iter().chain(decisions.iter()).map(|notes| notes.path.as_str()).collect();
  files.retain(|f| {
    let touches = |p: Option<&str>| p.is_some_and(|p| exclude.contains(&p));
    !touches(f.old_path.as_deref()) && !touches(f.new_path.as_deref())
  });
  // collect_snapshots drives the Scan and Snapshots stages itself: each is
  // entered with its full item count already known. An empty range skips
  // both stages entirely (no content to snapshot).
  let snapshots = collect_snapshots(&opts.repo, &lo, &commits, &exclude, progress)?;
  Ok(DiffDocument::new(
    git::repo_name(&opts.repo)?,
    RefInfo { name: opts.base.clone(), sha: base_sha },
    RefInfo { name: opts.head.clone(), sha: head_sha },
    lo,
    git::iso_utc_now(),
    commits,
    files,
    snapshots,
    description,
    decisions,
  ))
}

/// Render a document into the one-file HTML review page, with the comment
/// engine wasm inlined. `title` overrides the derived page title.
pub fn render_page(doc: &DiffDocument, title: Option<&str>) -> String {
  render::render_page(doc, title, WASM)
}

/// The one-call path: [`build_document`], then [`render_page`] under
/// [`PackOptions::title`]. The caller decides where the page goes; nothing
/// is written to disk.
pub fn pack(opts: &PackOptions, progress: &dyn ProgressObserver) -> Result<PackOutput, Error> {
  let document = build_document(opts, progress)?;
  progress.stage(Stage::Render, 1);
  let html = render_page(&document, opts.title.as_deref());
  progress.step("");
  Ok(PackOutput { document, html })
}
