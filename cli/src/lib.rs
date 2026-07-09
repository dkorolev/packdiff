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

use packdiff_dto::diff::DiffDocument;
use packdiff_dto::snapshot::{Boundary, RangeSnapshots};
use packdiff_dto::RefInfo;
use progress::{ProgressObserver, Stage};

/// The compiled comment engine, inlined into every generated page.
const WASM: &[u8] = include_bytes!(env!("PACKDIFF_WASM_PATH"));

/// Blobs larger than this are not snapshotted; sub-range diffs render such
/// files as "contents not shown" (the binary-file treatment).
const MAX_SNAPSHOT_BLOB_BYTES: usize = 2 * 1024 * 1024;

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
}

impl PackOptions {
  /// Options for diffing `base` against `head` in `repo`, with the CLI's
  /// defaults: merge-base semantics, 3 context lines, derived title.
  pub fn new(repo: impl Into<String>, base: impl Into<String>, head: impl Into<String>) -> Self {
    Self { repo: repo.into(), base: base.into(), head: head.into(), merge_base: true, context: 3, title: None }
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
  repo: &str, merge_base: &str, commits: &[packdiff_dto::diff::Commit], progress: &dyn ProgressObserver,
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
  let paths: Vec<String> = tracked.into_iter().collect();
  let mut boundaries = Vec::with_capacity(boundary_shas.len());
  for sha in &boundary_shas {
    let files = git::tree_blobs(repo, sha, &paths)?;
    progress.step(&format!("boundary {}", &sha[..7]));
    boundaries.push(Boundary { sha: sha.clone(), files });
  }
  let ids: std::collections::BTreeSet<&String> = boundaries.iter().flat_map(|b| b.files.values()).collect();
  progress.stage(Stage::Snapshots, ids.len() as u64);
  let mut blobs = std::collections::BTreeMap::new();
  for id in ids {
    blobs.insert(id.clone(), git::blob_text(repo, id, MAX_SNAPSHOT_BLOB_BYTES)?);
    progress.step(&format!("blob {}", &id[..id.len().min(8)]));
  }
  Ok(Some(RangeSnapshots { blobs, boundaries }))
}

/// Extract [`PackOptions`]'s diff from git into the typed document: resolve
/// the refs, diff, list the commits, and (for multi-commit ranges) snapshot
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
  let files = packdiff_dto::diff::parse_unified_diff(&git::diff_text(&opts.repo, &lo, &head_sha, opts.context)?);
  progress.step("");
  progress.stage(Stage::Commits, 1);
  let commits = git::commits(&opts.repo, &lo, &head_sha)?;
  progress.step("");
  // collect_snapshots drives the Scan and Snapshots stages itself: each is
  // entered with its full item count already known. A range of fewer than
  // two commits skips both stages entirely (nothing to filter).
  let snapshots = collect_snapshots(&opts.repo, &lo, &commits, progress)?;
  Ok(DiffDocument::new(
    git::repo_name(&opts.repo)?,
    RefInfo { name: opts.base.clone(), sha: base_sha },
    RefInfo { name: opts.head.clone(), sha: head_sha },
    lo,
    git::iso_utc_now(),
    commits,
    files,
    snapshots,
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
