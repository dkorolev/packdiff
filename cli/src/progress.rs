//! Live progress for one packdiff run. Two backends behind one API, chosen
//! by the CLI's machine flag:
//!
//! - **Human** (terminal): an `indicatif` bar on stderr showing the stage,
//!   a percentage, and the estimated time remaining. indicatif hides itself
//!   when stderr is not a terminal, so redirected runs stay clean.
//! - **Machine**: one `{ "Progress": { ... } }` JSON document per line on
//!   stderr — immediately at every stage change and at least once per second
//!   in between — so a harness always knows the stage, the counts, the
//!   percentage, and the ETA without parsing free text.
//!
//! Progress is liveness output (see the CLI contract): it goes to stderr
//! ONLY, never stdout, and disappears entirely on completion in human mode.
//!
//! Linearity: each stage owns a fixed span of the whole bar, weighted by its
//! typical share of the wall time (snapshotting dominates), and the position
//! interpolates through the span by items done within the stage. The
//! position is additionally clamped monotonic — discovering more work can
//! slow the bar down, but never moves it backwards.
//!
//! Library callers see only [`ProgressObserver`] (and the [`Stage`] /
//! [`ProgressReport`] vocabulary): [`Progress`] and its `indicatif`
//! dependency are the CLI's implementation, behind the default `cli`
//! feature. `&()` is the silent observer.

#[cfg(feature = "cli")]
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
#[cfg(feature = "cli")]
use std::sync::{Arc, Mutex};
#[cfg(feature = "cli")]
use std::thread::JoinHandle;
#[cfg(any(feature = "cli", test))]
use std::time::Duration;
#[cfg(feature = "cli")]
use std::time::Instant;

#[cfg(feature = "cli")]
use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};

/// Where [`crate::pack`] and [`crate::build_document`] report progress.
/// Stages arrive in execution order, each entered with its full item count
/// known. Both methods default to no-ops, so an implementor opts into only
/// what it needs; `&()` observes nothing.
pub trait ProgressObserver {
  /// A stage was entered; `known_items` work items will follow.
  fn stage(&self, stage: Stage, known_items: u64) {
    let _ = (stage, known_items);
  }
  /// One work item within the current stage finished; `detail` names it
  /// (possibly empty — e.g. for single-item stages).
  fn step(&self, detail: &str) {
    let _ = detail;
  }
}

/// The silent observer: progress is not reported anywhere.
impl ProgressObserver for () {}

/// The phases of one run, in execution order. Serialized as the bare
/// `CamelCase` variant name inside progress reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
  /// Resolving `BASE` and `HEAD` to commit SHAs.
  Resolve,
  /// Computing `merge-base(BASE, HEAD)` (a no-op step in two-dot mode).
  MergeBase,
  /// Running `git diff` and parsing it into the typed document.
  Diff,
  /// Listing the commits in the diffed range.
  Commits,
  /// Scanning the range for snapshot inputs: changed paths per commit pair
  /// and the tree listing at every boundary. Its item count is known at
  /// stage entry, so progress through it is linear.
  Scan,
  /// Fetching the snapshotted file contents (one git call per unique blob)
  /// — the dominant cost. The blob count is known before the first fetch,
  /// so progress through it is linear too.
  Snapshots,
  /// Assembling the HTML page.
  Render,
  /// Writing the output.
  Write,
  /// The run finished; `percent == 100`. Always the final report.
  Done,
}

/// The scale positions and spans are measured in (per-mille of the run).
#[cfg(any(feature = "cli", test))]
const SCALE: u64 = 1000;

#[cfg(any(feature = "cli", test))]
impl Stage {
  /// Short human label for the bar's message area.
  fn label(self) -> &'static str {
    match self {
      Stage::Resolve => "resolving refs",
      Stage::MergeBase => "merge base",
      Stage::Diff => "diffing",
      Stage::Commits => "listing commits",
      Stage::Scan => "scanning boundaries",
      Stage::Snapshots => "snapshotting",
      Stage::Render => "rendering",
      Stage::Write => "writing",
      Stage::Done => "done",
    }
  }

  /// The stage's `[start, end)` span on the 0..=[`SCALE`] bar, weighted by
  /// its typical share of the wall time. Every scan and blob item is one
  /// git call of comparable cost, and blobs typically outnumber scans
  /// roughly 3:1, which sets the `Scan`/`Snapshots` split.
  fn span(self) -> (u64, u64) {
    match self {
      Stage::Resolve => (0, 20),
      Stage::MergeBase => (20, 30),
      Stage::Diff => (30, 70),
      Stage::Commits => (70, 90),
      Stage::Scan => (90, 280),
      Stage::Snapshots => (280, 950),
      Stage::Render => (950, 985),
      Stage::Write => (985, SCALE),
      Stage::Done => (SCALE, SCALE),
    }
  }
}

/// One machine-mode progress report. Emitted to stderr as a single-key
/// `{ "Progress": { ...this } }` document, one per line.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressReport {
  /// The stage currently executing.
  pub stage: Stage,
  /// The current work item, human-oriented (e.g. `blob 1a2b3c4d`); absent
  /// between items.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub detail: Option<String>,
  /// Work items completed within the current stage.
  pub done: u64,
  /// Work items in the current stage; every stage enters with its full
  /// total already known, so this is stable within a stage.
  pub total: u64,
  /// Whole-run completion, `0..=100`: stage spans weighted by typical cost,
  /// interpolated by `done/total` within the stage, and clamped monotonic —
  /// it never decreases across a run.
  pub percent: u64,
  /// Milliseconds since the run started.
  pub elapsed_ms: u64,
  /// Estimated milliseconds remaining, extrapolated linearly from the
  /// weighted completion so far; absent until there is progress to
  /// extrapolate from.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub eta_ms: Option<u64>,
}

/// `elapsed × remaining ÷ done` over the weighted position; `None` at
/// position zero (no basis for extrapolation).
#[cfg(any(feature = "cli", test))]
fn eta_ms(elapsed_ms: u64, position: u64) -> Option<u64> {
  if position == 0 {
    return None;
  }
  Some(elapsed_ms.saturating_mul(SCALE - position.min(SCALE)) / position)
}

#[cfg(any(feature = "cli", test))]
struct State {
  stage: Stage,
  detail: Option<String>,
  /// Items done / known within the current stage only.
  stage_done: u64,
  stage_total: u64,
  /// High-water mark of the weighted position: the monotonic clamp.
  position: u64,
}

#[cfg(any(feature = "cli", test))]
impl State {
  /// Recompute the weighted position from the current stage and its item
  /// counts, ratcheting the monotonic high-water mark.
  fn advance(&mut self) -> u64 {
    let (start, end) = self.stage.span();
    let within =
      if self.stage_total == 0 { 0 } else { (end - start).saturating_mul(self.stage_done) / self.stage_total };
    self.position = self.position.max(start + within.min(end - start));
    self.position
  }

  fn report(&self, elapsed: Duration) -> ProgressReport {
    let elapsed_ms = elapsed.as_millis() as u64;
    ProgressReport {
      stage: self.stage,
      detail: self.detail.clone(),
      done: self.stage_done,
      total: self.stage_total,
      percent: self.position * 100 / SCALE,
      elapsed_ms,
      eta_ms: eta_ms(elapsed_ms, self.position),
    }
  }
}

#[cfg(feature = "cli")]
fn emit(state: &State, elapsed: Duration) {
  // Reports are liveness output: stderr only, one document per line.
  eprintln!("{}", serde_json::json!({ "Progress": state.report(elapsed) }));
}

/// Progress for one run. Construct once, thread through the stages, call
/// [`Progress::finish`] on success; dropping it (e.g. on an error path)
/// stops the ticker and clears the bar without emitting a `Done` report.
#[cfg(feature = "cli")]
pub struct Progress {
  started: Instant,
  state: Arc<Mutex<State>>,
  bar: Option<ProgressBar>,
  /// Dropping the sender wakes and ends the ticker thread immediately —
  /// no up-to-a-second exit lag on error paths.
  ticker_stop: Option<Sender<()>>,
  ticker: Option<JoinHandle<()>>,
}

#[cfg(feature = "cli")]
impl ProgressObserver for Progress {
  fn stage(&self, stage: Stage, known_items: u64) {
    Progress::stage(self, stage, known_items);
  }
  fn step(&self, detail: &str) {
    Progress::step(self, detail);
  }
}

#[cfg(feature = "cli")]
impl Progress {
  pub fn new(machine: bool) -> Self {
    let state =
      Arc::new(Mutex::new(State { stage: Stage::Resolve, detail: None, stage_done: 0, stage_total: 0, position: 0 }));
    let started = Instant::now();
    if machine {
      let (tx, rx) = channel::<()>();
      let ticker_state = Arc::clone(&state);
      let ticker = std::thread::spawn(move || loop {
        match rx.recv_timeout(Duration::from_secs(1)) {
          Err(RecvTimeoutError::Timeout) => {
            emit(&ticker_state.lock().expect("no thread panics while holding this lock"), started.elapsed());
          }
          Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
        }
      });
      Self { started, state, bar: None, ticker_stop: Some(tx), ticker: Some(ticker) }
    } else {
      // Draws to stderr by default; auto-hidden when stderr is not a tty.
      // Fixed length: the weighted position moves through 0..=SCALE, so the
      // bar fills linearly instead of rescaling as work is discovered.
      let bar = ProgressBar::new(SCALE);
      bar.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg:24!} {bar:32} {percent:>3}% eta {eta}")
          .expect("static template is valid"),
      );
      bar.enable_steady_tick(Duration::from_millis(100));
      Self { started, state, bar: Some(bar), ticker_stop: None, ticker: None }
    }
  }

  fn locked(&self) -> std::sync::MutexGuard<'_, State> {
    self.state.lock().expect("no thread panics while holding this lock")
  }

  /// Enter a stage with the number of work items known up front (extend
  /// later with [`Progress::add_work`]). Machine mode reports stage changes
  /// immediately (they are sparse), so even sub-second runs stream one line
  /// per stage.
  pub fn stage(&self, stage: Stage, known_items: u64) {
    let mut s = self.locked();
    s.stage = stage;
    s.detail = None;
    s.stage_done = 0;
    s.stage_total = known_items;
    let position = s.advance();
    if let Some(bar) = &self.bar {
      bar.set_position(position);
      bar.set_message(stage.label().to_string());
    } else {
      emit(&s, self.started.elapsed());
    }
  }

  /// One work item finished. Machine mode does NOT report each step — the
  /// once-per-second ticker covers cadence without flooding stderr.
  pub fn step(&self, detail: &str) {
    let mut s = self.locked();
    s.stage_done += 1;
    s.detail = if detail.is_empty() { None } else { Some(detail.to_string()) };
    let position = s.advance();
    if let Some(bar) = &self.bar {
      bar.set_position(position);
      let label = s.stage.label();
      bar.set_message(if detail.is_empty() { label.to_string() } else { format!("{label}: {detail}") });
    }
  }

  /// Successful completion: snap the position to 100%, emit the final
  /// `Done` report in machine mode, and remove the bar.
  pub fn finish(mut self) {
    {
      let mut s = self.locked();
      s.stage = Stage::Done;
      s.detail = None;
      s.stage_done = s.stage_total;
      s.position = SCALE;
      if self.bar.is_none() {
        emit(&s, self.started.elapsed());
      }
    }
    self.shutdown();
  }

  fn shutdown(&mut self) {
    drop(self.ticker_stop.take());
    if let Some(ticker) = self.ticker.take() {
      let _ = ticker.join();
    }
    if let Some(bar) = self.bar.take() {
      bar.finish_and_clear();
    }
  }
}

#[cfg(feature = "cli")]
impl Drop for Progress {
  fn drop(&mut self) {
    // Error paths: stop the ticker and clear the bar; no `Done` is emitted,
    // so a consumer never sees a completed run that actually failed.
    self.shutdown();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  // The human backend is indicatif rendering (visual, tty-only) and is not
  // unit-tested; the machine wire format below and the end-to-end stderr
  // stream (cli/tests/cli.rs) are.

  #[test]
  fn stage_spans_tile_the_bar() {
    let stages = [
      Stage::Resolve,
      Stage::MergeBase,
      Stage::Diff,
      Stage::Commits,
      Stage::Scan,
      Stage::Snapshots,
      Stage::Render,
      Stage::Write,
    ];
    let mut expected_start = 0;
    for stage in stages {
      let (start, end) = stage.span();
      assert_eq!(start, expected_start, "{stage:?} leaves a gap or overlaps");
      assert!(end > start, "{stage:?} has an empty span");
      expected_start = end;
    }
    assert_eq!(expected_start, SCALE, "the spans cover the whole bar");
  }

  #[test]
  fn position_is_monotonic_even_as_work_grows() {
    let mut s = State { stage: Stage::Snapshots, detail: None, stage_done: 0, stage_total: 4, position: 0 };
    s.stage_done = 3;
    let before = s.advance();
    s.stage_total = 40; // a burst of discovered work: 3/40 << 3/4
    let after = s.advance();
    assert!(after >= before, "position moved backwards: {before} -> {after}");
    s.stage_done = 40;
    assert!(s.advance() > after, "completing the discovered work still advances");
  }

  #[test]
  fn interpolation_stays_inside_the_stage_span() {
    let (start, end) = Stage::Snapshots.span();
    let mut s = State { stage: Stage::Snapshots, detail: None, stage_done: 0, stage_total: 10, position: start };
    assert_eq!(s.advance(), start);
    s.stage_done = 10;
    assert_eq!(s.advance(), end, "a fully done stage reaches exactly its end");
    s.stage_done = 20; // over-stepping is clamped, never spills into the next span
    assert_eq!(s.advance(), end);
  }

  #[test]
  fn eta_extrapolates_from_the_weighted_position() {
    assert_eq!(eta_ms(1000, 0), None, "no progress yet, no basis");
    assert_eq!(eta_ms(1000, 500), Some(1000), "half done in 1s: 1s remains");
    assert_eq!(eta_ms(3000, 750), Some(1000));
    assert_eq!(eta_ms(1000, SCALE), Some(0));
  }

  #[test]
  fn report_serializes_as_documented() {
    let state = State {
      stage: Stage::Snapshots,
      detail: Some("blob 1a2b3c4d".into()),
      stage_done: 3,
      stage_total: 12,
      position: 305,
    };
    let value = serde_json::json!({ "Progress": state.report(Duration::from_millis(1500)) });
    assert_eq!(
      value,
      serde_json::json!({ "Progress": {
        "stage": "Snapshots", "detail": "blob 1a2b3c4d",
        "done": 3, "total": 12, "percent": 30, "elapsed_ms": 1500, "eta_ms": 3418,
      }})
    );
  }

  #[test]
  fn absent_fields_are_omitted_not_null() {
    let state = State { stage: Stage::Resolve, detail: None, stage_done: 0, stage_total: 2, position: 0 };
    let text = serde_json::json!({ "Progress": state.report(Duration::ZERO) }).to_string();
    assert!(!text.contains("detail"), "{text}");
    assert!(!text.contains("eta_ms"), "{text}");
  }

  #[test]
  fn report_rejects_unknown_fields() {
    let bad = r#"{ "stage": "Diff", "done": 1, "total": 7, "percent": 5, "elapsed_ms": 10, "sneaky": true }"#;
    assert!(serde_json::from_str::<ProgressReport>(bad).is_err());
  }
}
