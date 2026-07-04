//! Git subprocess layer: the only place packdiff talks to the outside world.
//! Returns raw text; all interpretation happens in `packdiff-dto`.
//!
//! Every invocation is treated as fallible AND liveness-bounded: a watchdog
//! kills any git process that produces no output for [`WATCHDOG_SILENCE`],
//! and once a command runs longer than [`LIVENESS_AFTER`] a status line goes
//! to stderr every ~10 s so callers (humans, scripts, agents) can tell the
//! tool is alive.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use packdiff_dto::diff::Commit;

/// Kill a git process after this much *silence* (no output at all).
const WATCHDOG_SILENCE: Duration = Duration::from_secs(300);
/// Start emitting liveness lines to stderr after this much runtime.
const LIVENESS_AFTER: Duration = Duration::from_secs(10);
/// Interval between liveness lines.
const LIVENESS_EVERY: Duration = Duration::from_secs(10);

/// When set, every git invocation is echoed to stderr (the `--verbose`
/// whitelist: command lines and timing, nothing else).
pub static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Everything that can go wrong in the CLI, with enough structure that exit
/// codes, `stage` classification, and machine-readable variants all derive
/// from the same value. See `help exitcodes`.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
  /// The invocation itself was malformed (exit 2, stage `usage`).
  #[error("{message}")]
  Usage {
    /// Human-readable description of what was wrong with the invocation.
    message: String,
  },
  /// A non-canonical invocation was refused in machine mode (exit 2, stage
  /// `usage`). Machines must use canonical syntax; see the CLI principles.
  #[error("non-canonical invocation {given:?}; use the canonical form {canonical:?}")]
  NonCanonical {
    /// What the caller wrote.
    given: String,
    /// The canonical spelling the caller must use instead.
    canonical: String,
  },
  /// `--repo` does not point inside a git work tree (exit 3, stage `repo`).
  #[error("not a git repository: {repo}")]
  NotAGitRepository {
    /// The offending path, verbatim.
    repo: String,
  },
  /// A ref did not resolve to a commit (exit 4, stage `ref`).
  #[error("unknown ref in {repo:?}: {name}")]
  UnknownRef {
    /// The repository the lookup ran in.
    repo: String,
    /// The ref name that failed to resolve.
    name: String,
  },
  /// git itself failed, hung (watchdog), or emitted garbage (exit 5, stage `git`).
  #[error("{message}")]
  Git {
    /// git's stderr, or a description of the failure.
    message: String,
  },
  /// A local I/O failure — cannot write output and the like (exit 5, stage `io`).
  #[error("{message}")]
  Io {
    /// Description including the path involved.
    message: String,
  },
}

impl CliError {
  /// The process exit code for this error. Documented in `help exitcodes`.
  pub fn code(&self) -> i32 {
    match self {
      CliError::Usage { .. } | CliError::NonCanonical { .. } => 2,
      CliError::NotAGitRepository { .. } => 3,
      CliError::UnknownRef { .. } => 4,
      CliError::Git { .. } | CliError::Io { .. } => 5,
    }
  }

  /// Coarse classification for scripts, mirrored into the JSON error
  /// document; stable even where exit codes overlap.
  pub fn stage(&self) -> &'static str {
    match self {
      CliError::Usage { .. } | CliError::NonCanonical { .. } => "usage",
      CliError::NotAGitRepository { .. } => "repo",
      CliError::UnknownRef { .. } => "ref",
      CliError::Git { .. } => "git",
      CliError::Io { .. } => "io",
    }
  }

  /// The machine-mode document: a single-key union, request-specific variant
  /// names, `stage` inside the payload.
  pub fn to_machine_json(&self) -> serde_json::Value {
    let stage = self.stage();
    let (variant, mut payload) = match self {
      CliError::Usage { message } => ("UsageError", serde_json::json!({ "message": message })),
      CliError::NonCanonical { given, canonical } => (
        "NonCanonicalInvocation",
        serde_json::json!({
          "given": given,
          "canonical": canonical,
          "message": self.to_string(),
        }),
      ),
      CliError::NotAGitRepository { repo } => {
        ("NotAGitRepository", serde_json::json!({ "repo": repo, "message": self.to_string() }))
      }
      CliError::UnknownRef { repo, name } => {
        ("UnknownRef", serde_json::json!({ "repo": repo, "ref": name, "message": self.to_string() }))
      }
      CliError::Git { message } => ("GitError", serde_json::json!({ "message": message })),
      CliError::Io { message } => ("IoError", serde_json::json!({ "message": message })),
    };
    payload
      .as_object_mut()
      .expect("payload is always a JSON object by construction")
      .insert("stage".to_string(), serde_json::json!(stage));
    payload
      .as_object_mut()
      .expect("payload is always a JSON object by construction")
      .insert("exit_code".to_string(), serde_json::json!(self.code()));
    serde_json::json!({ variant: payload })
  }
}

/// Run git and capture stdout, under the liveness watchdog.
pub fn run_git(repo: &str, args: &[&str]) -> Result<String, CliError> {
  let pretty = format!("git -C {repo} {}", args.join(" "));
  if VERBOSE.load(Ordering::Relaxed) {
    eprintln!("packdiff: + {pretty}");
  }
  let started = Instant::now();

  let mut child = Command::new("git")
    .arg("-C")
    .arg(repo)
    .args(args)
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .map_err(|e| CliError::Git { message: format!("failed to run git: {e}") })?;

  let last_output = Arc::new(Mutex::new(Instant::now()));
  // The pipes exist because we just asked for Stdio::piped().
  let stdout_pipe = child.stdout.take().expect("stdout was requested as piped");
  let stderr_pipe = child.stderr.take().expect("stderr was requested as piped");
  let out_handle = spawn_reader(stdout_pipe, Arc::clone(&last_output));
  let err_handle = spawn_reader(stderr_pipe, Arc::clone(&last_output));

  let mut last_liveness = Instant::now();
  let status = loop {
    if let Some(status) = child.try_wait().map_err(|e| CliError::Git { message: format!("wait on git failed: {e}") })? {
      break status;
    }
    let silent_for = last_output.lock().expect("no thread panics while holding this lock").elapsed();
    if silent_for > WATCHDOG_SILENCE {
      let _ = child.kill();
      let _ = child.wait();
      return Err(CliError::Git {
        message: format!(
          "no output from `{pretty}` within {} minutes; process killed",
          WATCHDOG_SILENCE.as_secs() / 60
        ),
      });
    }
    if started.elapsed() > LIVENESS_AFTER && last_liveness.elapsed() > LIVENESS_EVERY {
      last_liveness = Instant::now();
      eprintln!("packdiff: `{pretty}` still running ({}s elapsed)", started.elapsed().as_secs());
    }
    std::thread::sleep(Duration::from_millis(50));
  };

  let stdout = out_handle.join().expect("reader thread does not panic");
  let stderr = err_handle.join().expect("reader thread does not panic");
  if VERBOSE.load(Ordering::Relaxed) {
    eprintln!("packdiff: `{pretty}` finished in {} ms", started.elapsed().as_millis());
  }

  if !status.success() {
    let stderr = String::from_utf8_lossy(&stderr).trim().to_string();
    if stderr.to_lowercase().contains("not a git repository") {
      return Err(CliError::NotAGitRepository { repo: repo.to_string() });
    }
    return Err(CliError::Git { message: if stderr.is_empty() { format!("`{pretty}` failed") } else { stderr } });
  }
  String::from_utf8(stdout).map_err(|e| CliError::Git { message: format!("git output was not UTF-8: {e}") })
}

fn spawn_reader<R: Read + Send + 'static>(
  mut pipe: R, last_output: Arc<Mutex<Instant>>,
) -> std::thread::JoinHandle<Vec<u8>> {
  std::thread::spawn(move || {
    let mut collected = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
      match pipe.read(&mut buf) {
        Ok(0) => break,
        Ok(n) => {
          collected.extend_from_slice(&buf[..n]);
          *last_output.lock().expect("no thread panics while holding this lock") = Instant::now();
        }
        Err(_) => break,
      }
    }
    collected
  })
}

/// A case-insensitive `HEAD` prefix (`head`, `Head^^`, `head~3`) rewritten to
/// the canonical spelling; `None` when the name is not head-like or is
/// already canonical. Only consulted as a FALLBACK after the name failed to
/// resolve as written, so a real ref named `head` always wins.
fn canonical_head(name: &str) -> Option<String> {
  let word_len = name.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(name.len());
  let (word, rest) = name.split_at(word_len);
  (word.eq_ignore_ascii_case("HEAD") && word != "HEAD").then(|| format!("HEAD{rest}"))
}

pub fn resolve_ref(repo: &str, name: &str) -> Result<String, CliError> {
  match rev_parse_commit(repo, name) {
    Ok(sha) => Ok(sha),
    Err(e @ CliError::NotAGitRepository { .. }) => Err(e),
    Err(_) => {
      // Refs are data, not syntax, so this leniency applies in machine mode
      // too; the note is a stderr diagnostic, never part of the output.
      if let Some(canonical) = canonical_head(name) {
        if let Ok(sha) = rev_parse_commit(repo, &canonical) {
          eprintln!("note: `{name}` resolved as `{canonical}`");
          return Ok(sha);
        }
      }
      Err(CliError::UnknownRef { repo: repo.to_string(), name: name.to_string() })
    }
  }
}

fn rev_parse_commit(repo: &str, name: &str) -> Result<String, CliError> {
  let spec = format!("{name}^{{commit}}");
  Ok(run_git(repo, &["rev-parse", "--verify", "--quiet", &spec])?.trim().to_string())
}

pub fn merge_base(repo: &str, a: &str, b: &str) -> Result<String, CliError> {
  Ok(run_git(repo, &["merge-base", a, b])?.trim().to_string())
}

pub fn repo_name(repo: &str) -> Result<String, CliError> {
  let top = run_git(repo, &["rev-parse", "--show-toplevel"])?;
  let name = top.trim().rsplit(['/', '\\']).next().unwrap_or("repo").to_string();
  Ok(if name.is_empty() { "repo".to_string() } else { name })
}

pub fn diff_text(repo: &str, lo: &str, hi: &str, context: u32) -> Result<String, CliError> {
  let ctx = format!("-U{context}");
  run_git(repo, &["diff", "--no-color", "--no-ext-diff", "--find-renames", &ctx, lo, hi])
}

/// Commits in `lo..hi`, oldest first.
pub fn commits(repo: &str, lo: &str, hi: &str) -> Result<Vec<Commit>, CliError> {
  let range = format!("{lo}..{hi}");
  let fmt = "--format=%H%x00%h%x00%an%x00%ae%x00%aI%x00%s";
  let out = run_git(repo, &["log", "--reverse", fmt, &range])?;
  let mut commits = Vec::new();
  for line in out.lines() {
    if line.trim().is_empty() {
      continue;
    }
    let parts: Vec<&str> = line.splitn(6, '\0').collect();
    if parts.len() != 6 {
      continue;
    }
    commits.push(Commit {
      sha: parts[0].to_string(),
      short: parts[1].to_string(),
      author: parts[2].to_string(),
      email: parts[3].to_string(),
      date: parts[4].to_string(),
      subject: parts[5].to_string(),
    });
  }
  Ok(commits)
}

/// RFC 3339 UTC "now" with second precision (the model has no clock; the CLI
/// is where time enters the system). Civil-from-days per Howard Hinnant.
pub fn iso_utc_now() -> String {
  let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
  let days = (secs / 86_400) as i64;
  let rem = secs % 86_400;
  let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
  let z = days + 719_468;
  let era = z.div_euclid(146_097);
  let doe = z.rem_euclid(146_097);
  let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
  let y = yoe + era * 400;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let d = doy - (153 * mp + 2) / 5 + 1;
  let month = if mp < 10 { mp + 3 } else { mp - 9 };
  let year = if month <= 2 { y + 1 } else { y };
  format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn iso_now_shape() {
    let now = iso_utc_now();
    assert_eq!(now.len(), 20, "{now}");
    assert!(now.ends_with('Z'));
    assert_eq!(&now[4..5], "-");
    assert_eq!(&now[10..11], "T");
    let year: u32 = now[..4].parse().unwrap();
    assert!(year >= 2024);
  }

  #[test]
  fn canonical_head_rewrites_case_insensitively() {
    assert_eq!(canonical_head("head").as_deref(), Some("HEAD"));
    assert_eq!(canonical_head("Head^^").as_deref(), Some("HEAD^^"));
    assert_eq!(canonical_head("hEaD~3").as_deref(), Some("HEAD~3"));
    assert_eq!(canonical_head("head@{1}").as_deref(), Some("HEAD@{1}"));
    assert_eq!(canonical_head("HEAD"), None, "already canonical");
    assert_eq!(canonical_head("HEAD^^^^"), None, "already canonical");
    assert_eq!(canonical_head("header"), None, "a real ref name, not HEAD");
    assert_eq!(canonical_head("main"), None);
  }

  #[test]
  fn error_codes_and_stages() {
    let e = CliError::UnknownRef { repo: ".".into(), name: "nope".into() };
    assert_eq!(e.code(), 4);
    assert_eq!(e.stage(), "ref");
    let doc = e.to_machine_json();
    assert!(doc.get("UnknownRef").is_some(), "{doc}");
    assert_eq!(doc["UnknownRef"]["stage"], "ref");
    assert_eq!(doc["UnknownRef"]["exit_code"], 4);
  }

  #[test]
  fn non_canonical_document_names_the_fix() {
    let e = CliError::NonCanonical { given: "--no-color".into(), canonical: "--color=never".into() };
    assert_eq!(e.code(), 2);
    let doc = e.to_machine_json();
    assert_eq!(doc["NonCanonicalInvocation"]["canonical"], "--color=never");
  }
}
