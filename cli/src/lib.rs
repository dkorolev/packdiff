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

/// The one notes file recognized today: the future pull request's
/// description, lifted into its own commentable page panel.
const NOTES_DESCRIPTION_PATH: &str = "PR-DESCRIPTION.md";

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
/// data behind the page's commit-range filter. `None` for ranges of fewer
/// than two commits, where there is nothing to filter.
fn collect_snapshots(
  repo: &str, merge_base: &str, commits: &[packdiff_dto::diff::Commit], exclude: Option<&str>,
  progress: &dyn ProgressObserver,
) -> Result<Option<RangeSnapshots>, Error> {
  if commits.len() < 2 {
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
  // A boundary pair spanning a notes commit picks up the lifted notes file;
  // it must not resurface in sub-range diffs.
  let paths: Vec<String> = tracked.into_iter().filter(|p| Some(p.as_str()) != exclude).collect();
  let mut boundaries = Vec::with_capacity(boundary_shas.len());
  for sha in &boundary_shas {
    let files = git::tree_blobs(repo, sha, &paths)?;
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
/// authored by `notes_email` whose changes are CONFINED to the notes file
/// carry notes such as `PR-DESCRIPTION.md`, not code under review. Both
/// halves of the test matter. Authorship alone is not enough: the notes
/// identity may also author code — a run orchestrator like scsh integrates
/// every agent commit under one bot identity — and code commits must stay
/// on the page no matter who authored them. Touching the description alone
/// is not enough either: a user-authored description must not be claimed,
/// and a mixed commit (code + description in one) is code. Returns the code
/// commits plus the lifted [`NotesFile`] — the description text as of the
/// last notes commit. With no notes email, no notes commits, or no readable
/// description text, everything stays as it was (hiding commits without
/// lifting anything would lose history).
fn split_notes(
  repo: &str, commits: Vec<packdiff_dto::diff::Commit>, notes_email: Option<&str>,
) -> Result<(Vec<packdiff_dto::diff::Commit>, Option<NotesFile>), Error> {
  let Some(email) = notes_email else { return Ok((commits, None)) };
  // Commits arrive oldest-first (`git log --reverse`), so `notes_shas` is too.
  let mut notes_shas = Vec::new();
  for c in commits.iter().filter(|c| c.email == email) {
    // `sha^` can fail only for a parentless commit in the range (disjoint
    // histories in two-dot mode); such a commit is not a notes commit.
    let touched = git::changed_paths(repo, &format!("{}^", c.sha), &c.sha).unwrap_or_default();
    if !touched.is_empty() && touched.iter().all(|p| p == NOTES_DESCRIPTION_PATH) {
      notes_shas.push(c.sha.clone());
    }
  }
  if notes_shas.is_empty() {
    return Ok((commits, None));
  }
  let mut text = None;
  for sha in notes_shas.iter().rev() {
    let blobs = git::tree_blobs(repo, sha, &[NOTES_DESCRIPTION_PATH.to_string()])?;
    if let Some(id) = blobs.get(NOTES_DESCRIPTION_PATH) {
      text = git::blob_text(repo, id, MAX_SNAPSHOT_BLOB_BYTES)?;
      if text.is_some() {
        break;
      }
    }
  }
  let Some(text) = text else { return Ok((commits, None)) };
  let (notes, code): (Vec<_>, Vec<_>) = commits.into_iter().partition(|c| notes_shas.contains(&c.sha));
  let description = NotesFile {
    path: NOTES_DESCRIPTION_PATH.to_string(),
    text,
    commits: notes.iter().map(|c| c.sha.clone()).collect(),
  };
  Ok((code, Some(description)))
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
  let (commits, description) = split_notes(&opts.repo, commits, opts.notes_email.as_deref())?;
  // The lifted description leaves the diff entirely: its file drops out of
  // the file list (and so out of the +/− totals), and out of the snapshot
  // paths below — the page shows it as its own panel instead.
  if let Some(d) = &description {
    files.retain(|f| f.old_path.as_deref() != Some(d.path.as_str()) && f.new_path.as_deref() != Some(d.path.as_str()));
  }
  let exclude = description.as_ref().map(|d| d.path.as_str());
  // collect_snapshots drives the Scan and Snapshots stages itself: each is
  // entered with its full item count already known. A range of fewer than
  // two commits skips both stages entirely (nothing to filter).
  let snapshots = collect_snapshots(&opts.repo, &lo, &commits, exclude, progress)?;
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
