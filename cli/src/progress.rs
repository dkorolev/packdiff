//! Live progress for one packdiff run. Two backends behind one API, chosen
//! by the CLI's machine flag:
//!
//! - **Human** (terminal): a bar redrawn in place on stderr showing the
//!   stage, a percentage, and the estimated time remaining. It is drawn only
//!   when stderr is a terminal, so redirected runs stay clean.
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
//! [`ProgressReport`] vocabulary): [`Progress`] is the CLI's
//! implementation, behind the default `cli` feature. `&()` is the silent
//! observer.

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

use packdiff_dto::json::{self, Fields, FromJson, Map, ToJson, Value};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone)]
pub struct ProgressReport {
  /// The stage currently executing.
  pub stage: Stage,
  /// The current work item, human-oriented (e.g. `blob 1a2b3c4d`); absent
  /// between items.
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
  pub eta_ms: Option<u64>,
}

impl ToJson for Stage {
  fn to_json(&self) -> Value {
    Value::from(format!("{self:?}"))
  }
}

impl FromJson for Stage {
  fn from_json(value: &Value) -> json::Result<Self> {
    Ok(match json::variant_name(value, "Stage")? {
      "Resolve" => Stage::Resolve,
      "MergeBase" => Stage::MergeBase,
      "Diff" => Stage::Diff,
      "Commits" => Stage::Commits,
      "Scan" => Stage::Scan,
      "Snapshots" => Stage::Snapshots,
      "Render" => Stage::Render,
      "Write" => Stage::Write,
      "Done" => Stage::Done,
      other => return Err(json::Error::unknown_variant(other, "Stage")),
    })
  }
}

impl ToJson for ProgressReport {
  fn to_json(&self) -> Value {
    let mut o = Map::new();
    o.insert("stage", self.stage.to_json());
    if let Some(detail) = &self.detail {
      o.insert("detail", detail);
    }
    o.insert("done", self.done);
    o.insert("total", self.total);
    o.insert("percent", self.percent);
    o.insert("elapsed_ms", self.elapsed_ms);
    if let Some(eta_ms) = self.eta_ms {
      o.insert("eta_ms", eta_ms);
    }
    Value::Object(o)
  }
}

impl FromJson for ProgressReport {
  fn from_json(value: &Value) -> json::Result<Self> {
    let mut f = Fields::of(value, "ProgressReport")?;
    let report = ProgressReport {
      stage: f.required("stage")?,
      detail: f.optional("detail")?,
      done: f.required("done")?,
      total: f.required("total")?,
      percent: f.required("percent")?,
      elapsed_ms: f.required("elapsed_ms")?,
      eta_ms: f.optional("eta_ms")?,
    };
    f.finish()?;
    Ok(report)
  }
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
    let within = (end - start).saturating_mul(self.stage_done).checked_div(self.stage_total).unwrap_or(0);
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
  eprintln!("{}", Value::object([("Progress", state.report(elapsed).to_json())]));
}

/// Width of the bar's message area; longer labels are cut with an ellipsis.
#[cfg(any(feature = "cli", test))]
const LABEL_WIDTH: usize = 24;
/// Width of the bar itself, in cells.
#[cfg(any(feature = "cli", test))]
const BAR_WIDTH: u64 = 32;
/// The spinner's frames, advanced once per redraw.
#[cfg(any(feature = "cli", test))]
const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// The human bar's one line, without the carriage return that redraws it
/// in place: spinner, stage (and item) label, the bar, the percentage, and
/// the ETA — the same shape as before, drawn in-house.
#[cfg(any(feature = "cli", test))]
fn bar_line(state: &State, elapsed: Duration, frame: usize) -> String {
  let report = state.report(elapsed);
  let label = match &state.detail {
    Some(detail) => format!("{}: {detail}", state.stage.label()),
    None => state.stage.label().to_string(),
  };
  let label: String = if label.chars().count() > LABEL_WIDTH {
    label.chars().take(LABEL_WIDTH - 1).chain(std::iter::once('…')).collect()
  } else {
    label
  };
  let filled = (state.position.min(SCALE) * BAR_WIDTH / SCALE) as usize;
  let bar: String =
    std::iter::repeat('█').take(filled).chain(std::iter::repeat('·').take(BAR_WIDTH as usize - filled)).collect();
  let eta = match report.eta_ms {
    Some(ms) if ms >= 60_000 => format!("{}m {}s", ms / 60_000, ms % 60_000 / 1000),
    Some(ms) => format!("{}s", ms / 1000),
    None => "-".to_string(),
  };
  format!("{} {label:<LABEL_WIDTH$} {bar} {:>3}% eta {eta}", SPINNER[frame % SPINNER.len()], report.percent)
}

#[cfg(feature = "cli")]
fn draw(state: &State, elapsed: Duration, frame: usize) {
  use std::io::Write;
  // Clear the line, then redraw from its start: the bar always occupies the
  // one line it started on.
  let mut stderr = std::io::stderr().lock();
  let _ = write!(stderr, "\r\x1b[2K{}", bar_line(state, elapsed, frame));
  let _ = stderr.flush();
}

/// Which backend a run reports through.
#[cfg(feature = "cli")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
  /// One `Progress` document per line on stderr.
  Machine,
  /// A bar redrawn in place on stderr, which is a terminal.
  Bar,
  /// stderr is redirected: nothing is drawn.
  Silent,
}

/// Progress for one run. Construct once, thread through the stages, call
/// [`Progress::finish`] on success; dropping it (e.g. on an error path)
/// stops the ticker and clears the bar without emitting a `Done` report.
#[cfg(feature = "cli")]
pub struct Progress {
  started: Instant,
  state: Arc<Mutex<State>>,
  backend: Backend,
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
    use std::io::IsTerminal;
    let state =
      Arc::new(Mutex::new(State { stage: Stage::Resolve, detail: None, stage_done: 0, stage_total: 0, position: 0 }));
    let started = Instant::now();
    let backend = if machine {
      Backend::Machine
    } else if std::io::stderr().is_terminal() {
      Backend::Bar
    } else {
      Backend::Silent
    };
    // The ticker is the cadence in both live backends: machine mode reports
    // at least once per second, the bar redraws (and spins) ten times per
    // second. Stage changes are reported eagerly on top of that.
    let (tx, rx) = channel::<()>();
    let ticker_state = Arc::clone(&state);
    let ticker = match backend {
      Backend::Machine => Some(std::thread::spawn(move || {
        while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_secs(1)) {
          emit(&ticker_state.lock().expect("no thread panics while holding this lock"), started.elapsed());
        }
      })),
      Backend::Bar => Some(std::thread::spawn(move || {
        let mut frame = 0;
        while let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_millis(100)) {
          draw(&ticker_state.lock().expect("no thread panics while holding this lock"), started.elapsed(), frame);
          frame += 1;
        }
      })),
      Backend::Silent => None,
    };
    Self { started, state, backend, ticker_stop: Some(tx), ticker }
  }

  fn locked(&self) -> std::sync::MutexGuard<'_, State> {
    self.state.lock().expect("no thread panics while holding this lock")
  }

  /// Enter a stage with the number of work items known up front. Machine
  /// mode reports stage changes immediately (they are sparse), so even
  /// sub-second runs stream one line per stage.
  pub fn stage(&self, stage: Stage, known_items: u64) {
    let mut s = self.locked();
    s.stage = stage;
    s.detail = None;
    s.stage_done = 0;
    s.stage_total = known_items;
    s.advance();
    if self.backend == Backend::Machine {
      emit(&s, self.started.elapsed());
    }
  }

  /// One work item finished. Machine mode does NOT report each step — the
  /// once-per-second ticker covers cadence without flooding stderr — and
  /// the bar picks the new position up on its next redraw.
  pub fn step(&self, detail: &str) {
    let mut s = self.locked();
    s.stage_done += 1;
    s.detail = if detail.is_empty() { None } else { Some(detail.to_string()) };
    s.advance();
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
      if self.backend == Backend::Machine {
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
    if self.backend == Backend::Bar {
      use std::io::Write;
      // The ticker has stopped, so this is the last write to the line.
      let mut stderr = std::io::stderr().lock();
      let _ = write!(stderr, "\r\x1b[2K");
      let _ = stderr.flush();
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

  // The human backend's terminal handling (tty detection, in-place redraw)
  // is visual and not unit-tested; its line is, as are the machine wire
  // format below and the end-to-end stderr stream (cli/tests/cli.rs).

  #[test]
  fn bar_line_shows_stage_item_progress_and_eta() {
    let state = State {
      stage: Stage::Snapshots,
      detail: Some("blob 1a2b3c4d".into()),
      stage_done: 3,
      stage_total: 12,
      position: 305,
    };
    let line = bar_line(&state, Duration::from_millis(1500), 0);
    assert_eq!(line, "⠋ snapshotting: blob 1a2b… █████████·······················  30% eta 3s");
    assert!(!line.contains('\n') && !line.contains('\r'), "one line, redrawn in place");
    // A long item label is cut to the message area, so the bar never wraps.
    let mut long = State { detail: Some("x".repeat(80)), ..state };
    let cut = bar_line(&long, Duration::from_millis(1500), 3);
    assert_eq!(cut.chars().count(), line.chars().count());
    assert!(cut.starts_with("⠸ snapshotting: xxxxxxxxx…"), "{cut}");
    // Minutes show as `Nm Ss`; no basis for an ETA shows as `-`; done is full.
    long.position = 10;
    assert!(bar_line(&long, Duration::from_secs(10), 0).ends_with("eta 16m 30s"));
    long.position = 0;
    assert!(bar_line(&long, Duration::ZERO, 0).ends_with("eta -"));
    long.position = SCALE;
    long.detail = None;
    assert!(bar_line(&long, Duration::from_secs(1), 0).contains("████████████████████████████████ 100% eta 0s"));
  }

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
    let value = Value::object([("Progress", state.report(Duration::from_millis(1500)).to_json())]);
    let expected = concat!(
      r#"{ "Progress": { "stage": "Snapshots", "detail": "blob 1a2b3c4d", "#,
      r#""done": 3, "total": 12, "percent": 30, "elapsed_ms": 1500, "eta_ms": 3418 } }"#
    );
    assert_eq!(value, json::parse(expected).unwrap());
  }

  #[test]
  fn absent_fields_are_omitted_not_null() {
    let state = State { stage: Stage::Resolve, detail: None, stage_done: 0, stage_total: 2, position: 0 };
    let text = Value::object([("Progress", state.report(Duration::ZERO).to_json())]).to_string();
    assert!(!text.contains("detail"), "{text}");
    assert!(!text.contains("eta_ms"), "{text}");
  }

  #[test]
  fn report_rejects_unknown_fields() {
    let bad = r#"{ "stage": "Diff", "done": 1, "total": 7, "percent": 5, "elapsed_ms": 10, "sneaky": true }"#;
    assert!(json::from_str::<ProgressReport>(bad).is_err());
  }
}
