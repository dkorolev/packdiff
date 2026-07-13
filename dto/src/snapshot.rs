//! Commit-boundary file snapshots and the pure line diff over them — what
//! lets the generated page re-diff any contiguous commit sub-range without
//! touching git: the CLI collects contents once, the page (via WASM) calls
//! [`range_diff`] with a pair of boundary indices.
//!
//! Nothing here shells out; snapshots arrive as data. Renames are not
//! re-detected inside a sub-range — a rename shows as a delete plus an add.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::diff::{FileDiff, FileStatus, Hunk, Line};
use crate::ModelError;

/// File contents pinned at every commit boundary of the diffed range,
/// deduplicated by git blob id. Only paths touched by some commit in the
/// range are tracked.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeSnapshots {
  /// Blob id → content. `None` marks content that was not snapshotted
  /// (binary, not UTF-8, or oversized); sub-range diffs render such files as
  /// "contents not shown".
  pub blobs: BTreeMap<String, Option<String>>,
  /// One entry per boundary, oldest first: index 0 is the diff's start
  /// (the merge base), index `k > 0` is the state after the k-th commit.
  pub boundaries: Vec<Boundary>,
}

/// The tracked files' state at one commit boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boundary {
  /// The commit this boundary is the state at (full SHA).
  pub sha: String,
  /// Path → blob id for every tracked path that exists at this boundary; a
  /// missing path does not exist here.
  pub files: BTreeMap<String, String>,
}

/// The diff between boundaries `from` and `to` (`from < to`, indices into
/// `boundaries`), as the same [`FileDiff`] shape the build-time parser
/// emits. Selecting commit `k` alone is `range_diff(snap, k - 1, k, …)`.
pub fn range_diff(snap: &RangeSnapshots, from: usize, to: usize, context: usize) -> Result<Vec<FileDiff>, ModelError> {
  if from >= to || to >= snap.boundaries.len() {
    return Err(ModelError::InvalidRange(format!(
      "from {from} / to {to} against {} boundaries",
      snap.boundaries.len()
    )));
  }
  let lo = &snap.boundaries[from];
  let hi = &snap.boundaries[to];
  let mut paths: BTreeSet<&String> = lo.files.keys().collect();
  paths.extend(hi.files.keys());

  let content = |id: &String| -> Result<&Option<String>, ModelError> {
    snap.blobs.get(id).ok_or_else(|| ModelError::InvalidRange(format!("blob {id} missing from the snapshot store")))
  };

  let mut files = Vec::new();
  for path in paths {
    let old_id = lo.files.get(path);
    let new_id = hi.files.get(path);
    if old_id == new_id {
      continue; // same blob (or absent on both sides) → unchanged in this sub-range
    }
    let status = match (old_id, new_id) {
      (None, Some(_)) => FileStatus::Added,
      (Some(_), None) => FileStatus::Deleted,
      _ => FileStatus::Modified,
    };
    let old_blob = old_id.map(content).transpose()?;
    let new_blob = new_id.map(content).transpose()?;
    let old_path = old_id.map(|_| path.clone());
    let new_path = new_id.map(|_| path.clone());
    // Unsnapshotted content on a present side → shown like a binary file.
    if matches!(old_blob, Some(None)) || matches!(new_blob, Some(None)) {
      files.push(FileDiff {
        old_path,
        new_path,
        status,
        binary: true,
        hunks: Vec::new(),
        additions: 0,
        deletions: 0,
        notes: Vec::new(),
      });
      continue;
    }
    let old_text = old_blob.and_then(|b| b.as_deref()).unwrap_or("");
    let new_text = new_blob.and_then(|b| b.as_deref()).unwrap_or("");
    let hunks = diff_lines(old_text, new_text, context);
    let additions = hunks.iter().flat_map(|h| &h.lines).filter(|l| matches!(l, Line::Add { .. })).count() as u32;
    let deletions = hunks.iter().flat_map(|h| &h.lines).filter(|l| matches!(l, Line::Del { .. })).count() as u32;
    if hunks.is_empty() {
      continue; // distinct blob ids never carry identical content, but stay defensive
    }
    files.push(FileDiff { old_path, new_path, status, binary: false, hunks, additions, deletions, notes: Vec::new() });
  }
  Ok(files)
}

/// Unchanged lines shared by a file's two endpoint snapshots — what the
/// page's expand-context control reveals inside hunk gaps. Returns up to
/// `count` [`Line::Ctx`] rows starting at `old_start`/`new_start` (1-based,
/// pre- and post-image), clamped at either file's end. The requested region
/// must be text-identical at both endpoints — a gap between hunks always is
/// — so expansion can never invent or hide a change; a mismatch is rejected
/// loudly. `old_path`/`new_path` may differ (renamed files).
pub fn context_slice(
  snap: &RangeSnapshots, old_path: &str, new_path: &str, old_start: u32, new_start: u32, count: u32,
) -> Result<Vec<Line>, ModelError> {
  if old_start == 0 || new_start == 0 {
    return Err(ModelError::InvalidRange("line numbers are 1-based".to_string()));
  }
  let (Some(base), Some(head)) = (snap.boundaries.first(), snap.boundaries.last()) else {
    return Err(ModelError::InvalidRange("snapshot store has no boundaries".to_string()));
  };
  let text_at = |boundary: &Boundary, path: &str, side: &str| -> Result<String, ModelError> {
    let id = boundary
      .files
      .get(path)
      .ok_or_else(|| ModelError::InvalidRange(format!("{path} does not exist at the {side} boundary")))?;
    let blob = snap
      .blobs
      .get(id)
      .ok_or_else(|| ModelError::InvalidRange(format!("blob {id} missing from the snapshot store")))?;
    blob.clone().ok_or_else(|| ModelError::InvalidRange(format!("{path} was not snapshotted (binary or oversized)")))
  };
  let old_text = text_at(base, old_path, "base")?;
  let new_text = text_at(head, new_path, "head")?;
  let old_lines: Vec<&str> = old_text.lines().collect();
  let new_lines: Vec<&str> = new_text.lines().collect();
  let mut out = Vec::new();
  for i in 0..count {
    let (Some(old_no), Some(new_no)) = (old_start.checked_add(i), new_start.checked_add(i)) else { break };
    let (Some(old), Some(new)) = (old_lines.get(old_no as usize - 1), new_lines.get(new_no as usize - 1)) else {
      break; // clamped: one side ran out of file
    };
    if old != new {
      return Err(ModelError::InvalidRange(format!(
        "old line {old_no} and new line {new_no} differ between the endpoints — not an unchanged region"
      )));
    }
    out.push(Line::Ctx { old: old_no, new: new_no, text: (*old).to_string() });
  }
  Ok(out)
}

/// Diff two texts line-wise into hunks with `context` unchanged lines around
/// each change — the same [`Hunk`]/[`Line`] shapes `parse_unified_diff`
/// produces. Identical texts yield no hunks.
pub fn diff_lines(old: &str, new: &str, context: usize) -> Vec<Hunk> {
  let a: Vec<&str> = old.lines().collect();
  let b: Vec<&str> = new.lines().collect();
  let edits = myers_edits(&a, &b);
  hunks_from(&records(&a, &b, &edits), context)
}

/// One step of the edit script: keep a common line, delete from the
/// pre-image, or add from the post-image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edit {
  Keep,
  Del,
  Add,
}

/// Myers' O(ND) shortest edit script over lines (the greedy forward variant
/// with a full trace for backtracking).
fn myers_edits(a: &[&str], b: &[&str]) -> Vec<Edit> {
  let n = a.len() as isize;
  let m = b.len() as isize;
  let max = n + m;
  if max == 0 {
    return Vec::new();
  }
  let offset = max;
  let idx = |k: isize| (k + offset) as usize;
  let mut v = vec![0isize; (2 * max + 1) as usize];
  let mut trace: Vec<Vec<isize>> = Vec::new();
  'search: for d in 0..=max {
    trace.push(v.clone());
    let mut k = -d;
    while k <= d {
      let mut x = if k == -d || (k != d && v[idx(k - 1)] < v[idx(k + 1)]) { v[idx(k + 1)] } else { v[idx(k - 1)] + 1 };
      let mut y = x - k;
      while x < n && y < m && a[x as usize] == b[y as usize] {
        x += 1;
        y += 1;
      }
      v[idx(k)] = x;
      if x >= n && y >= m {
        break 'search;
      }
      k += 2;
    }
  }

  // Backtrack from (n, m); trace[d] is the V state entering round d.
  let mut edits = Vec::new();
  let (mut x, mut y) = (n, m);
  for (d, v) in trace.iter().enumerate().rev() {
    let d = d as isize;
    let k = x - y;
    let prev_k = if k == -d || (k != d && v[idx(k - 1)] < v[idx(k + 1)]) { k + 1 } else { k - 1 };
    let prev_x = v[idx(prev_k)];
    let prev_y = prev_x - prev_k;
    while x > prev_x && y > prev_y {
      edits.push(Edit::Keep);
      x -= 1;
      y -= 1;
    }
    if d > 0 {
      edits.push(if x == prev_x { Edit::Add } else { Edit::Del });
    }
    x = prev_x;
    y = prev_y;
  }
  edits.reverse();
  edits
}

/// A typed diff line plus the 1-based counters as they stood BEFORE the
/// line, which is what hunk headers are computed from.
struct Rec {
  line: Line,
  old_before: u32,
  new_before: u32,
  changed: bool,
}

fn records(a: &[&str], b: &[&str], edits: &[Edit]) -> Vec<Rec> {
  let (mut ai, mut bi) = (0usize, 0usize);
  let (mut old_no, mut new_no) = (1u32, 1u32);
  let mut recs = Vec::with_capacity(edits.len());
  for e in edits {
    match e {
      Edit::Keep => {
        recs.push(Rec {
          line: Line::Ctx { old: old_no, new: new_no, text: a[ai].to_string() },
          old_before: old_no,
          new_before: new_no,
          changed: false,
        });
        ai += 1;
        bi += 1;
        old_no += 1;
        new_no += 1;
      }
      Edit::Del => {
        recs.push(Rec {
          line: Line::Del { old: old_no, text: a[ai].to_string() },
          old_before: old_no,
          new_before: new_no,
          changed: true,
        });
        ai += 1;
        old_no += 1;
      }
      Edit::Add => {
        recs.push(Rec {
          line: Line::Add { new: new_no, text: b[bi].to_string() },
          old_before: old_no,
          new_before: new_no,
          changed: true,
        });
        bi += 1;
        new_no += 1;
      }
    }
  }
  recs
}

/// Group records into hunks: every changed line plus up to `context`
/// neighbors; overlapping windows merge. Headers use the explicit
/// `@@ -start,count +start,count @@` form (count included even when 1); a
/// side with no lines gets git's convention of `start = line before, count 0`.
fn hunks_from(recs: &[Rec], context: usize) -> Vec<Hunk> {
  let mut include = vec![false; recs.len()];
  for (i, r) in recs.iter().enumerate() {
    if r.changed {
      let lo = i.saturating_sub(context);
      let hi = (i + context).min(recs.len() - 1);
      for flag in &mut include[lo..=hi] {
        *flag = true;
      }
    }
  }
  let mut hunks = Vec::new();
  let mut i = 0;
  while i < recs.len() {
    if !include[i] {
      i += 1;
      continue;
    }
    let start = i;
    while i < recs.len() && include[i] {
      i += 1;
    }
    let slice = &recs[start..i];
    let old_count = slice.iter().filter(|r| matches!(r.line, Line::Del { .. } | Line::Ctx { .. })).count();
    let new_count = slice.iter().filter(|r| matches!(r.line, Line::Add { .. } | Line::Ctx { .. })).count();
    let old_start = if old_count > 0 { slice[0].old_before } else { slice[0].old_before.saturating_sub(1) };
    let new_start = if new_count > 0 { slice[0].new_before } else { slice[0].new_before.saturating_sub(1) };
    hunks.push(Hunk {
      header: format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@"),
      lines: slice.iter().map(|r| r.line.clone()).collect(),
    });
  }
  hunks
}

#[cfg(test)]
mod tests {
  use super::*;

  fn snap() -> RangeSnapshots {
    let blob = |s: &str| Some(s.to_string());
    RangeSnapshots {
      blobs: BTreeMap::from([
        ("b-one".into(), blob("alpha\nbeta\ngamma\n")),
        ("b-two".into(), blob("alpha\nBETA\ngamma\n")),
        ("b-new".into(), blob("fresh\n")),
        ("b-bin".into(), None),
      ]),
      boundaries: vec![
        Boundary {
          sha: "s0".into(),
          files: BTreeMap::from([("keep.txt".into(), "b-one".into()), ("gone.txt".into(), "b-one".into())]),
        },
        Boundary {
          sha: "s1".into(),
          files: BTreeMap::from([("keep.txt".into(), "b-two".into()), ("gone.txt".into(), "b-one".into())]),
        },
        Boundary {
          sha: "s2".into(),
          files: BTreeMap::from([
            ("keep.txt".into(), "b-one".into()),
            ("new.txt".into(), "b-new".into()),
            ("blob.bin".into(), "b-bin".into()),
          ]),
        },
      ],
    }
  }

  #[test]
  fn single_commit_diff() {
    let files = range_diff(&snap(), 0, 1, 3).unwrap();
    assert_eq!(files.len(), 1);
    let f = &files[0];
    assert_eq!(f.new_path.as_deref(), Some("keep.txt"));
    assert_eq!(f.status, FileStatus::Modified);
    assert_eq!((f.additions, f.deletions), (1, 1));
    assert_eq!(f.hunks[0].header, "@@ -1,3 +1,3 @@");
    assert_eq!(f.hunks[0].lines[1], Line::Del { old: 2, text: "beta".into() });
    assert_eq!(f.hunks[0].lines[2], Line::Add { new: 2, text: "BETA".into() });
  }

  #[test]
  fn full_range_hides_a_change_that_was_reverted() {
    // keep.txt: one → two → back to one; the 0..2 diff must not mention it.
    let files = range_diff(&snap(), 0, 2, 3).unwrap();
    let paths: Vec<&str> = files.iter().map(|f| f.anchor_path()).collect();
    assert_eq!(paths, vec!["blob.bin", "gone.txt", "new.txt"]);
    // ...while the 1..2 sub-range shows the revert itself.
    let files = range_diff(&snap(), 1, 2, 3).unwrap();
    assert!(files.iter().any(|f| f.anchor_path() == "keep.txt"));
  }

  #[test]
  fn added_deleted_and_binary_statuses() {
    let files = range_diff(&snap(), 0, 2, 3).unwrap();
    let by_path = |p: &str| files.iter().find(|f| f.anchor_path() == p).unwrap();
    let added = by_path("new.txt");
    assert_eq!(added.status, FileStatus::Added);
    assert_eq!(added.old_path, None);
    assert_eq!(added.hunks[0].header, "@@ -0,0 +1,1 @@");
    let deleted = by_path("gone.txt");
    assert_eq!(deleted.status, FileStatus::Deleted);
    assert_eq!(deleted.new_path, None);
    assert_eq!(deleted.deletions, 3);
    let binary = by_path("blob.bin");
    assert!(binary.binary);
    assert!(binary.hunks.is_empty());
  }

  #[test]
  fn invalid_ranges_and_missing_blobs_are_loud() {
    assert!(matches!(range_diff(&snap(), 1, 1, 3), Err(ModelError::InvalidRange(_))));
    assert!(matches!(range_diff(&snap(), 2, 1, 3), Err(ModelError::InvalidRange(_))));
    assert!(matches!(range_diff(&snap(), 0, 9, 3), Err(ModelError::InvalidRange(_))));
    let mut broken = snap();
    broken.blobs.remove("b-two");
    assert!(matches!(range_diff(&broken, 0, 1, 3), Err(ModelError::InvalidRange(_))));
  }

  #[test]
  fn context_slice_returns_clamped_ctx_lines() {
    // keep.txt is b-one at BOTH endpoints (s0 and s2) of the fixture.
    let lines = context_slice(&snap(), "keep.txt", "keep.txt", 1, 1, 10).unwrap();
    assert_eq!(lines.len(), 3, "clamped at end of file");
    assert_eq!(lines[0], Line::Ctx { old: 1, new: 1, text: "alpha".into() });
    assert_eq!(lines[2], Line::Ctx { old: 3, new: 3, text: "gamma".into() });
    let one = context_slice(&snap(), "keep.txt", "keep.txt", 2, 2, 1).unwrap();
    assert_eq!(one, vec![Line::Ctx { old: 2, new: 2, text: "beta".into() }]);
    assert!(context_slice(&snap(), "keep.txt", "keep.txt", 9, 9, 5).unwrap().is_empty(), "past EOF: empty, not error");
  }

  #[test]
  fn context_slice_follows_renames_and_offset_numbering() {
    let s = RangeSnapshots {
      blobs: BTreeMap::from([("b".into(), Some("one\ntwo\nthree\nfour\n".to_string()))]),
      boundaries: vec![
        Boundary { sha: "s0".into(), files: BTreeMap::from([("old name.txt".into(), "b".into())]) },
        Boundary { sha: "s1".into(), files: BTreeMap::from([("new name.txt".into(), "b".into())]) },
      ],
    };
    // Renamed file: the pre-image path resolves at the base boundary, the
    // post-image path at the head boundary.
    let lines = context_slice(&s, "old name.txt", "new name.txt", 3, 3, 2).unwrap();
    assert_eq!(lines[0], Line::Ctx { old: 3, new: 3, text: "three".into() });
  }

  #[test]
  fn context_slice_rejects_changed_regions_and_bad_input() {
    let differing = RangeSnapshots {
      blobs: BTreeMap::from([
        ("b-one".into(), Some("alpha\nbeta\n".to_string())),
        ("b-two".into(), Some("alpha\nBETA\n".to_string())),
        ("b-bin".into(), None),
      ]),
      boundaries: vec![
        Boundary {
          sha: "s0".into(),
          files: BTreeMap::from([("f.txt".into(), "b-one".into()), ("blob.bin".into(), "b-bin".into())]),
        },
        Boundary {
          sha: "s1".into(),
          files: BTreeMap::from([("f.txt".into(), "b-two".into()), ("blob.bin".into(), "b-bin".into())]),
        },
      ],
    };
    // Line 1 is shared, line 2 differs: the slice must fail rather than
    // present a changed line as context.
    assert!(context_slice(&differing, "f.txt", "f.txt", 1, 1, 1).is_ok());
    assert!(matches!(context_slice(&differing, "f.txt", "f.txt", 1, 1, 2), Err(ModelError::InvalidRange(_))));
    // Unsnapshotted, missing, and 0-based requests are loud errors.
    assert!(matches!(context_slice(&differing, "blob.bin", "blob.bin", 1, 1, 1), Err(ModelError::InvalidRange(_))));
    assert!(matches!(context_slice(&differing, "nope", "nope", 1, 1, 1), Err(ModelError::InvalidRange(_))));
    assert!(matches!(context_slice(&differing, "f.txt", "f.txt", 0, 1, 1), Err(ModelError::InvalidRange(_))));
  }

  #[test]
  fn diff_lines_context_and_merging() {
    let old = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n";
    // Two changes far apart → two hunks at context 1; one merged hunk at 3.
    let new = "a\nB\nc\nd\ne\nf\ng\nh\nI\nj\n";
    let hunks = diff_lines(old, new, 1);
    assert_eq!(hunks.len(), 2);
    assert_eq!(hunks[0].header, "@@ -1,3 +1,3 @@");
    assert_eq!(hunks[1].header, "@@ -8,3 +8,3 @@");
    let hunks = diff_lines(old, new, 3);
    assert_eq!(hunks.len(), 1, "windows overlap and merge");
    assert_eq!(hunks[0].header, "@@ -1,10 +1,10 @@");
  }

  #[test]
  fn diff_lines_edge_cases() {
    assert!(diff_lines("", "", 3).is_empty());
    assert!(diff_lines("same\n", "same\n", 3).is_empty());
    let from_empty = diff_lines("", "one\ntwo\n", 3);
    assert_eq!(from_empty[0].header, "@@ -0,0 +1,2 @@");
    let to_empty = diff_lines("one\ntwo\n", "", 3);
    assert_eq!(to_empty[0].header, "@@ -1,2 +0,0 @@");
  }

  #[test]
  fn myers_produces_a_minimal_script() {
    // The classic ABCABBA → CBABAC example: shortest script has 5 edits.
    let a: Vec<&str> = "A B C A B B A".split(' ').collect();
    let b: Vec<&str> = "C B A B A C".split(' ').collect();
    let edits = myers_edits(&a, &b);
    let changes = edits.iter().filter(|e| **e != Edit::Keep).count();
    assert_eq!(changes, 5);
    let keeps = edits.iter().filter(|e| **e == Edit::Keep).count();
    assert_eq!(keeps, 4);
    // The script replays a into b.
    let (mut ai, mut bi) = (0, 0);
    let mut out: Vec<&str> = Vec::new();
    for e in &edits {
      match e {
        Edit::Keep => {
          out.push(a[ai]);
          ai += 1;
          bi += 1;
        }
        Edit::Del => ai += 1,
        Edit::Add => {
          out.push(b[bi]);
          bi += 1;
        }
      }
    }
    assert_eq!(out, b);
    assert_eq!((ai, bi), (a.len(), b.len()));
  }

  #[test]
  fn snapshots_roundtrip_and_reject_unknown_fields() {
    let s = snap();
    let json = serde_json::to_string(&s).unwrap();
    let back: RangeSnapshots = serde_json::from_str(&json).unwrap();
    assert_eq!(back.boundaries.len(), 3);
    let sneaky = json.replacen("{\"blobs\"", "{\"sneaky\":true,\"blobs\"", 1);
    assert!(serde_json::from_str::<RangeSnapshots>(&sneaky).is_err(), "unknown fields are strict-rejected");
  }
}
