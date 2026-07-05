//! Live progress for one packdiff run. Two backends behind one API, chosen
//! by the CLI's machine flag:
//!
//! - **Human** (terminal): an `indicatif` bar on stderr showing the stage,
//!   work counts, and the estimated time remaining. indicatif hides itself
//!   when stderr is not a terminal, so redirected runs stay clean.
//! - **Machine**: one `{ "Progress": { ... } }` JSON document per line on
//!   stderr — immediately at every stage change and at least once per second
//!   in between — so a harness always knows the stage, the counts, and the
//!   ETA without parsing free text.
//!
//! Progress is liveness output (see the CLI contract): it goes to stderr
//! ONLY, never stdout, and disappears entirely on completion in human mode.
//!
//! Work accounting: the run starts with one unit per fixed stage step and
//! grows the total as snapshot work (boundaries, blobs) is discovered, so
//! `done/total` and the ETA stay honest rather than jumping backwards.

use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};
use serde::{Deserialize, Serialize};

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
  /// Snapshotting file contents at every commit boundary — the dominant
  /// cost; its work items (boundaries, blobs) are discovered incrementally.
  Snapshots,
  /// Assembling the HTML page.
  Render,
  /// Writing the output.
  Write,
  /// The run finished; `done == total`. Always the final report.
  Done,
}

impl Stage {
  /// Short human label for the bar's message area.
  fn label(self) -> &'static str {
    match self {
      Stage::Resolve => "resolving refs",
      Stage::MergeBase => "merge base",
      Stage::Diff => "diffing",
      Stage::Commits => "listing commits",
      Stage::Snapshots => "snapshotting",
      Stage::Render => "rendering",
      Stage::Write => "writing",
      Stage::Done => "done",
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
  /// Work items completed so far across the whole run.
  pub done: u64,
  /// Total work items known so far; grows as snapshot work is discovered,
  /// and never shrinks.
  pub total: u64,
  /// Milliseconds since the run started.
  pub elapsed_ms: u64,
  /// Estimated milliseconds remaining, linearly extrapolated from work done
  /// so far; absent until at least one work item has completed.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub eta_ms: Option<u64>,
}

/// `elapsed × remaining ÷ done`, the linear ETA; `None` before any work is
/// done (no basis for extrapolation).
fn eta_ms(elapsed_ms: u64, done: u64, total: u64) -> Option<u64> {
  if done == 0 {
    return None;
  }
  Some(elapsed_ms.saturating_mul(total.saturating_sub(done)) / done)
}

/// Fixed work units before snapshot discovery: two ref resolutions, merge
/// base, diff, commit list, render, write.
const BASE_UNITS: u64 = 7;

struct State {
  stage: Stage,
  detail: Option<String>,
  done: u64,
  total: u64,
}

impl State {
  fn report(&self, elapsed: Duration) -> ProgressReport {
    let elapsed_ms = elapsed.as_millis() as u64;
    ProgressReport {
      stage: self.stage,
      detail: self.detail.clone(),
      done: self.done,
      total: self.total,
      elapsed_ms,
      eta_ms: eta_ms(elapsed_ms, self.done, self.total),
    }
  }
}

fn emit(state: &State, elapsed: Duration) {
  // Reports are liveness output: stderr only, one document per line.
  eprintln!("{}", serde_json::json!({ "Progress": state.report(elapsed) }));
}

/// Progress for one run. Construct once, thread through the stages, call
/// [`Progress::finish`] on success; dropping it (e.g. on an error path)
/// stops the ticker and clears the bar without emitting a `Done` report.
pub struct Progress {
  started: Instant,
  state: Arc<Mutex<State>>,
  bar: Option<ProgressBar>,
  /// Dropping the sender wakes and ends the ticker thread immediately —
  /// no up-to-a-second exit lag on error paths.
  ticker_stop: Option<Sender<()>>,
  ticker: Option<JoinHandle<()>>,
}

impl Progress {
  pub fn new(machine: bool) -> Self {
    let state = Arc::new(Mutex::new(State { stage: Stage::Resolve, detail: None, done: 0, total: BASE_UNITS }));
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
      let bar = ProgressBar::new(BASE_UNITS);
      bar.set_style(
        ProgressStyle::with_template("{spinner:.green} {msg:24!} {bar:32} {pos}/{len} eta {eta}")
          .expect("static template is valid"),
      );
      bar.enable_steady_tick(Duration::from_millis(100));
      Self { started, state, bar: Some(bar), ticker_stop: None, ticker: None }
    }
  }

  fn locked(&self) -> std::sync::MutexGuard<'_, State> {
    self.state.lock().expect("no thread panics while holding this lock")
  }

  /// Enter a stage. Machine mode reports stage changes immediately (they are
  /// sparse), so even sub-second runs produce one line per stage.
  pub fn stage(&self, stage: Stage) {
    let mut s = self.locked();
    s.stage = stage;
    s.detail = None;
    if let Some(bar) = &self.bar {
      bar.set_message(stage.label().to_string());
    } else {
      emit(&s, self.started.elapsed());
    }
  }

  /// Newly discovered work items (snapshot boundaries, blobs) extend the total.
  pub fn add_work(&self, n: u64) {
    let mut s = self.locked();
    s.total += n;
    if let Some(bar) = &self.bar {
      bar.set_length(s.total);
    }
  }

  /// One work item finished. Machine mode does NOT report each step — the
  /// once-per-second ticker covers cadence without flooding stderr.
  pub fn step(&self, detail: &str) {
    let mut s = self.locked();
    s.done += 1;
    s.detail = if detail.is_empty() { None } else { Some(detail.to_string()) };
    if let Some(bar) = &self.bar {
      bar.inc(1);
      let label = s.stage.label();
      bar.set_message(if detail.is_empty() { label.to_string() } else { format!("{label}: {detail}") });
    }
  }

  /// Successful completion: snap `done` to `total`, emit the final `Done`
  /// report in machine mode, and remove the bar.
  pub fn finish(mut self) {
    {
      let mut s = self.locked();
      s.stage = Stage::Done;
      s.detail = None;
      s.done = s.total;
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
  fn eta_extrapolates_linearly() {
    assert_eq!(eta_ms(0, 0, 10), None, "no work done yet, no basis");
    assert_eq!(eta_ms(1000, 0, 10), None);
    assert_eq!(eta_ms(1000, 1, 10), Some(9000));
    assert_eq!(eta_ms(1000, 5, 10), Some(1000));
    assert_eq!(eta_ms(1000, 10, 10), Some(0));
    assert_eq!(eta_ms(1000, 10, 5), Some(0), "total below done clamps, never underflows");
  }

  #[test]
  fn report_serializes_as_documented() {
    let state = State { stage: Stage::Snapshots, detail: Some("blob 1a2b3c4d".into()), done: 3, total: 12 };
    let value = serde_json::json!({ "Progress": state.report(Duration::from_millis(1500)) });
    assert_eq!(
      value,
      serde_json::json!({ "Progress": {
        "stage": "Snapshots", "detail": "blob 1a2b3c4d",
        "done": 3, "total": 12, "elapsed_ms": 1500, "eta_ms": 4500,
      }})
    );
  }

  #[test]
  fn absent_fields_are_omitted_not_null() {
    let state = State { stage: Stage::Resolve, detail: None, done: 0, total: 7 };
    let text = serde_json::json!({ "Progress": state.report(Duration::ZERO) }).to_string();
    assert!(!text.contains("detail"), "{text}");
    assert!(!text.contains("eta_ms"), "{text}");
  }

  #[test]
  fn report_rejects_unknown_fields() {
    let bad = r#"{ "stage": "Diff", "done": 1, "total": 7, "elapsed_ms": 10, "sneaky": true }"#;
    assert!(serde_json::from_str::<ProgressReport>(bad).is_err());
  }
}
