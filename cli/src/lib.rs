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
/// identity that conventionally commits PR notes such as
/// `PR-DESCRIPTION.md`. Authorship no longer decides what is notes — the
/// changed paths do — so this value only keeps the option non-empty, i.e.
/// the convention enabled. The CLI overrides it from the
/// `PACKDIFF_SYSTEM_USER_EMAIL` environment variable; setting it empty is
/// how a caller turns the lift off.
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
  /// Kill switch for the notes-commit convention, kept in email form for
  /// backwards compatibility. `Some(_)` (the default,
  /// [`DEFAULT_SYSTEM_USER_EMAIL`]) enables the lift: a commit confined to
  /// notes files is hidden from the commit list and its `PR-DESCRIPTION.md`
  /// / `PR-DECISION-<topic>.md` become commentable page panels, whoever
  /// authored it. `None` disables the convention entirely.
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

/// What [`split_notes`] separated out of the range.
#[derive(Default)]
struct LiftedNotes {
  /// The commits that remain code under review, oldest first.
  code: Vec<packdiff_dto::diff::Commit>,
  /// The newest version of the PR description, if the range carries one.
  description: Option<NotesFile>,
  /// Older versions of the description, newest first — see
  /// [`packdiff_dto::diff::DiffDocument::superseded_descriptions`].
  superseded_descriptions: Vec<NotesFile>,
  /// The journaled decisions, ordered by path.
  decisions: Vec<NotesFile>,
}

/// A notes file's text at one commit, or `None` when the commit does not
/// carry it (deleted) or it is too large to lift.
fn notes_text_at(repo: &str, sha: &str, path: &str) -> Result<Option<String>, Error> {
  let owned = path.to_string();
  let blobs = git::tree_blobs(repo, sha, std::slice::from_ref(&owned))?;
  match blobs.get(path) {
    Some(id) => git::blob_text(repo, id, MAX_SNAPSHOT_BLOB_BYTES),
    None => Ok(None),
  }
}

/// Partition the range's commits per the notes-commit convention: a commit
/// whose changes are CONFINED to notes files — `PR-DESCRIPTION.md` and
/// `PR-DECISION-<topic>.md` at the repository root — carries notes, not code
/// under review. The test is the paths alone. Who authored the commit is
/// irrelevant: a description is metadata about the change no matter whose
/// name is on it, and a real notes commit is easy to recognize because it
/// touches nothing else. A commit mixing code with notes is still code, so a
/// notes file only ever leaves the diff when some commit was exclusively
/// about it. `notes_email` survives purely as a kill switch: `None` disables
/// the convention and everything stays code.
///
/// Each decision is lifted at its state in the LAST notes commit that
/// touched it — journaling is incremental, so only the final text matters.
/// The description is different: committing it more than once is malformed,
/// and the reviewer may well have meant to comment on an earlier draft, so
/// EVERY version is lifted, newest first.
///
/// When the range has notes commits but nothing readable to lift, nothing is
/// hidden — dropping commits without lifting anything would lose history.
fn split_notes(
  repo: &str, lo: &str, hi: &str, commits: Vec<packdiff_dto::diff::Commit>, notes_email: Option<&str>,
) -> Result<LiftedNotes, Error> {
  let unchanged = |commits| LiftedNotes { code: commits, ..LiftedNotes::default() };
  if notes_email.is_none() || commits.is_empty() {
    return Ok(unchanged(commits));
  }
  // Commits arrive oldest-first (`git log --reverse`), so `notes` is too.
  // The paths are the only way to know a commit is notes, and one batched
  // `git log` scan collects them for every single-parent commit at once.
  let by_commit = git::changed_paths_by_commit(repo, lo, hi)?;
  let mut notes: Vec<(&packdiff_dto::diff::Commit, Vec<String>)> = Vec::new();
  // Every notes path the range touches, deduplicated and ordered by path so
  // the panels render deterministically.
  let mut notes_paths: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
  for c in &commits {
    // Merge and parentless commits are absent from the batched scan and take
    // the per-commit form instead: a merge diffs against its first parent
    // exactly as it always did, and `sha^` failing for a parentless commit
    // (disjoint histories in two-dot mode) still means not-a-notes-commit.
    let touched = match by_commit.get(&c.sha) {
      Some(paths) => paths.clone(),
      None => git::changed_paths(repo, &format!("{}^", c.sha), &c.sha).unwrap_or_default(),
    };
    if !touched.is_empty() && touched.iter().all(|p| is_notes_path(p)) {
      notes_paths.extend(touched.iter().cloned());
      notes.push((c, touched));
    }
  }
  if notes.is_empty() {
    return Ok(unchanged(commits));
  }
  // Provenance is the notes commits as a whole: one commit may carry the
  // description and a decision together, so the hidden shas belong to every
  // file lifted out of them.
  let notes_commit_shas: Vec<String> = notes.iter().map(|(c, _)| c.sha.clone()).collect();
  let lift = |path: &str, text: String, revision| NotesFile {
    path: path.to_string(),
    text,
    commits: notes_commit_shas.clone(),
    revision,
  };

  let mut decisions: Vec<NotesFile> = Vec::new();
  for path in notes_paths.iter().filter(|p| is_decision_path(p)) {
    // A later commit that deletes the file leaves nothing to lift, so walk
    // back until some notes commit still carries readable text.
    for (c, _) in notes.iter().rev() {
      if let Some(text) = notes_text_at(repo, &c.sha, path)? {
        decisions.push(lift(path, text, None));
        break;
      }
    }
  }
  // Every commit that was exclusively about the description contributes one
  // version, newest first; the first is current, the rest are superseded.
  let mut versions: Vec<NotesFile> = Vec::new();
  for (c, _) in notes.iter().rev().filter(|(_, t)| t.iter().any(|p| p == NOTES_DESCRIPTION_PATH)) {
    if let Some(text) = notes_text_at(repo, &c.sha, NOTES_DESCRIPTION_PATH)? {
      let revision = packdiff_dto::diff::NotesRevision { short: c.short.clone(), subject: c.subject.clone() };
      versions.push(lift(NOTES_DESCRIPTION_PATH, text, Some(revision)));
    }
  }
  // A single version is unambiguous, so it needs no revision label.
  if versions.len() == 1 {
    versions[0].revision = None;
  }
  if versions.is_empty() && decisions.is_empty() {
    return Ok(unchanged(commits));
  }
  let mut versions = versions.into_iter();
  let description = versions.next();
  let superseded_descriptions: Vec<NotesFile> = versions.collect();
  let notes_shas: std::collections::BTreeSet<&String> = notes_commit_shas.iter().collect();
  let code = commits.into_iter().filter(|c| !notes_shas.contains(&c.sha)).collect();
  Ok(LiftedNotes { code, description, superseded_descriptions, decisions })
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
  let notes = split_notes(&opts.repo, &lo, &head_sha, commits, opts.notes_email.as_deref())?;
  let LiftedNotes { code: commits, description, superseded_descriptions, decisions } = notes;
  // Lifted notes leave the diff entirely: their files drop out of the file
  // list (and so out of the +/− totals), and out of the snapshot paths below
  // — the page shows each as its own panel instead. Superseded description
  // versions share the description's path, so they add nothing here.
  let exclude: Vec<&str> = description.iter().chain(decisions.iter()).map(|notes| notes.path.as_str()).collect();
  files.retain(|f| {
    let touches = |p: Option<&str>| p.is_some_and(|p| exclude.contains(&p));
    !touches(f.old_path.as_deref()) && !touches(f.new_path.as_deref())
  });
  // collect_snapshots drives the Scan and Snapshots stages itself: each is
  // entered with its full item count already known. An empty range skips
  // both stages entirely (no content to snapshot).
  let snapshots = collect_snapshots(&opts.repo, &lo, &commits, &exclude, progress)?;
  let mut doc = DiffDocument::new(
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
  );
  doc.superseded_descriptions = superseded_descriptions;
  Ok(doc)
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
