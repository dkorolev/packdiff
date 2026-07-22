//! Integration test: build a scratch git repo, run the real `packdiff` binary,
//! assert the HTML, the machine-mode contract (single-key union documents,
//! auto-JSON when piped), exit codes, and the liberal-vs-canonical rules.
//! Skips (with a hint) if git is absent from PATH.
//!
//! Note: these tests run the binary with PIPED stdout, which per the CLI
//! contract IS machine mode — so they exercise exactly what scripts and
//! agents see.

use std::path::{Path, PathBuf};
use std::process::Command;

fn git_available() -> bool {
  Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn git(repo: &Path, args: &[&str]) {
  let status = Command::new("git")
    .arg("-C")
    .arg(repo)
    .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
    .args(args)
    .status()
    .expect("git runs");
  assert!(status.success(), "git {args:?} failed");
}

fn write(repo: &Path, rel: &str, content: &[u8]) {
  let path = repo.join(rel);
  std::fs::write(path, content).expect("write file");
}

fn make_repo(dir: &Path) {
  std::fs::create_dir_all(dir).unwrap();
  git(dir, &["init", "-q"]);
  git(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
  write(dir, "hello.py", b"def hello():\n    return 'hello'\n");
  write(dir, "todelete.txt", b"obsolete\n");
  write(dir, "torename.txt", b"stable line 1\nstable line 2\n");
  write(dir, "blob.bin", b"\x00\x01\x02BINARY\x00");
  git(dir, &["add", "-A"]);
  git(dir, &["commit", "-qm", "initial"]);

  git(dir, &["checkout", "-qb", "feature"]);
  write(dir, "hello.py", b"def hello():\n    return 'hello'\n\ndef evil():\n    return '<script>alert(1)</script>'\n");
  git(dir, &["rm", "-q", "todelete.txt"]);
  git(dir, &["mv", "torename.txt", "renamed.txt"]);
  git(dir, &["add", "-A"]);
  git(dir, &["commit", "-qm", "feature change one"]);

  write(dir, "newfile.md", b"# New\n\nBrand new file.\n");
  write(dir, "blob.bin", b"\x00\x01\x02CHANGED\x00\xff");
  git(dir, &["add", "-A"]);
  git(dir, &["commit", "-qm", "feature change two"]);

  // Post-branch drift on main: merge-base mode must ignore it.
  git(dir, &["checkout", "-q", "main"]);
  write(dir, "mainline.txt", b"only on main\n");
  git(dir, &["add", "-A"]);
  git(dir, &["commit", "-qm", "mainline drift"]);
}

fn bin() -> &'static str {
  env!("CARGO_BIN_EXE_packdiff")
}

fn tmpdir(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("packdiff-test-{name}-{}", std::process::id()));
  let _ = std::fs::remove_dir_all(&dir);
  std::fs::create_dir_all(&dir).unwrap();
  dir
}

#[test]
fn end_to_end() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH — install git to run this test");
    return;
  }
  let tmp = tmpdir("e2e");
  let repo = tmp.join("sample");
  make_repo(&repo);
  let out = tmp.join("diff.html");
  let dump = tmp.join("doc.json");

  // Piped stdout = machine mode: exactly one single-key `Packed` document.
  let output = Command::new(bin())
    .args([
      "main",
      "feature",
      "-C",
      repo.to_str().unwrap(),
      "-o",
      out.to_str().unwrap(),
      "--dump-json",
      dump.to_str().unwrap(),
    ])
    .output()
    .expect("binary runs");
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

  let doc: serde_json::Value = serde_json::from_slice(&output.stdout).expect("stdout is one JSON document");
  let packed = doc.get("Packed").expect("single-key `Packed` document");
  assert_eq!(packed["commits"], 2, "merge-base mode ignores mainline drift");
  assert_eq!(packed["files"], 5);
  assert_eq!(packed["binary_files"], 1);
  assert_eq!(packed["repo"], "sample");
  assert_eq!(packed["base"]["name"], "main");
  assert!(packed["warnings"].as_array().expect("warnings array present").is_empty());

  let html = std::fs::read_to_string(&out).unwrap();

  // Self-contained: no external src/href.
  for attr in ["src=\"http", "href=\"http", "src=\"//", "href=\"//"] {
    assert!(!html.contains(attr), "external reference: {attr}");
  }
  // Hostile content is escaped; the only script tags are our own four
  // (config JSON, snapshots JSON, wasm base64, app JS).
  assert!(!html.contains("<script>alert(1)</script>"));
  assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
  assert!(html.contains(r#"class="tok-kw">def</span>"#), "Python keywords are highlighted");
  assert!(
    html.contains(r#"class="tok-str">&#39;&lt;script&gt;alert(1)&lt;/script&gt;&#39;</span>"#),
    "hostile source remains escaped inside a syntax span"
  );
  assert_eq!(html.matches("<script").count(), 4);
  // The wasm module is inlined (base64 of `\0asm`).
  assert!(html.contains("id=\"packdiff-wasm\">AGFzbQ"));
  // Comment anchors use the CamelCase `Side` variant names.
  assert!(html.contains("data-file=\"hello.py\" data-side=\"New\""));
  assert!(html.contains("data-side=\"Old\""));
  // Expand-context prerequisites: endpoint paths and line counts stamped on
  // the file panel (hello.py grew from 2 to 5 lines in the fixture).
  assert!(
    html.contains(r#"data-old-path="hello.py" data-new-path="hello.py" data-old-lines="2" data-new-lines="5""#),
    "endpoint paths and line counts are stamped for the expander"
  );
  assert!(html.contains("torename.txt → renamed.txt"));
  assert!(html.contains("Binary file — contents not shown."));
  assert!(html.contains("feature change one"));
  // Exactly one markdown file in the fixture (newfile.md) → exactly one
  // Markdown | Source pill and one server-rendered preview. Markdown is the
  // default: the markdown file's preview starts visible and its diff wrap
  // starts hidden — and it is the only hidden wrap on the page.
  assert_eq!(html.matches(r#"class="seg md-seg""#).count(), 1);
  assert_eq!(html.matches(r#"class="md-preview""#).count(), 1);
  assert!(!html.contains(r#"<div class="md-preview" hidden>"#));
  assert_eq!(html.matches(r#"<div class="diff-wrap" hidden>"#).count(), 1);
  assert!(html.contains(r#"data-mdview="preview" class="active""#));
  assert!(html.contains(">Markdown</button><button type=\"button\" data-mdview=\"diff\""));
  assert!(html.contains("<h1>New</h1>"), "the markdown preview is rendered at build time");
  assert!(html.contains(r#"class="md-run md-run-add""#), "added markdown renders tinted green");
  // Commit range filtering: snapshots embedded, commits selectable, one copy
  // button per commit (two feature commits in the fixture).
  assert!(html.contains(r#"id="packdiff-snapshots""#));
  assert!(html.contains(r#"id="files-range""#));
  assert_eq!(html.matches(r#"class="copy-sha""#).count(), 2);
  assert_eq!(html.matches(r#"class="commit selectable""#).count(), 2);
  for id in [
    "copy-json",
    "copy-md",
    "import-json",
    "expand-all",
    "collapse-all",
    "verdict-comment",
    "verdict-approve",
    "verdict-changes",
  ] {
    assert!(html.contains(&format!("id=\"{id}\"")), "missing #{id}");
  }
  // The human-readable document and its one-row navigation are canonical.
  assert!(html.contains(r#"id="topnav""#));
  assert!(!html.contains(r#"id="sidebar""#));
  assert!(html.contains(r#"class="review-summary-text""#));
  assert!(html.contains(r#"class="gutter-btn""#), "comment gutter buttons present");
  for id in ["summary", "commits", "files", "diff"] {
    assert!(html.contains(&format!("<section id=\"{id}\"")), "missing #{id} section");
  }
  assert!(html.contains(r##"href="#summary""##));
  assert!(html.contains(r##"href="#commits""##));
  assert!(html.contains(r##"href="#files""##));
  assert!(html.contains(r##"href="#diff""##));
  // One canonical file index and one diff panel per file (5 in the fixture).
  assert_eq!(html.matches(r#"class="filelist""#).count(), 1);
  assert_eq!(html.matches("href=\"#file-").count(), 5);
  assert_eq!(html.matches("<details class=\"file\" id=\"file-").count(), 5);
  // Side-by-side: a view toggle in the chrome (disabled until the JS enables it
  // on a wide-enough workspace) and every diff table wrapped for a split sibling.
  assert!(html.contains(r#"id="view-toggle""#));
  assert_eq!(html.matches(r#"class="diff unified""#).count(), html.matches(r#"class="diff-wrap""#).count());

  // The dumped DiffDocument parses back through the dto schema, with
  // single-key line unions.
  let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&dump).unwrap()).unwrap();
  assert_eq!(doc["schema_version"], 3);
  assert_eq!(doc["files"].as_array().unwrap().len(), 5);
  assert_eq!(doc["base"]["name"], "main");
  // Snapshots: one boundary per commit plus the merge base, and the binary
  // blob is stored as null (not snapshotted).
  let boundaries = doc["snapshots"]["boundaries"].as_array().expect("snapshots collected for 2 commits");
  assert_eq!(boundaries.len(), 3);
  assert_eq!(boundaries[0]["sha"], doc["merge_base"]);
  let blobs = doc["snapshots"]["blobs"].as_object().unwrap();
  assert!(blobs.values().any(|v| v.is_null()), "binary blob content is null");
  assert!(blobs.values().any(|v| v.as_str().is_some_and(|s| s.contains("Brand new file."))));
  let hello =
    doc["files"].as_array().unwrap().iter().find(|f| f["new_path"] == "hello.py").expect("hello.py present in dump");
  assert_eq!(hello["status"], "Modified");
  let first_line = &hello["hunks"][0]["lines"][0];
  assert!(
    first_line.get("Ctx").is_some() || first_line.get("Add").is_some() || first_line.get("Del").is_some(),
    "lines are single-key unions: {first_line}"
  );

  let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn two_dot_mode_sees_mainline_drift() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH");
    return;
  }
  let tmp = tmpdir("twodot");
  let repo = tmp.join("sample");
  make_repo(&repo);
  let output = Command::new(bin())
    .args(["main", "feature", "-C", repo.to_str().unwrap(), "-o", "-", "--no-merge-base"])
    .output()
    .unwrap();
  assert!(output.status.success());
  let html = String::from_utf8_lossy(&output.stdout);
  assert!(html.starts_with("<!DOCTYPE html>"));
  assert!(html.ends_with("</html>\n"), "with `-o -` the page is the ONLY stdout content");
  assert!(html.contains("mainline.txt"), "two-dot diff includes reverse changes");
  let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn error_documents_and_exit_codes() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH");
    return;
  }
  let tmp = tmpdir("errors");
  let repo = tmp.join("sample");
  make_repo(&repo);

  // Unknown ref → exit 4 and a single-key `UnknownRef` document with stage.
  let output = Command::new(bin()).args(["main", "no-such-branch", "-C", repo.to_str().unwrap()]).output().unwrap();
  assert_eq!(output.status.code(), Some(4));
  let err: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(err["UnknownRef"]["stage"], "ref");
  assert_eq!(err["UnknownRef"]["exit_code"], 4);
  assert_eq!(err["UnknownRef"]["ref"], "no-such-branch");

  // Not a repo → exit 3, `NotAGitRepository`.
  let empty = tmp.join("not-a-repo");
  std::fs::create_dir_all(&empty).unwrap();
  let output = Command::new(bin()).args(["main", "feature", "-C", empty.to_str().unwrap()]).output().unwrap();
  assert_eq!(output.status.code(), Some(3));
  let err: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(err["NotAGitRepository"]["stage"], "repo");

  // Usage errors → exit 2, `UsageError` documents.
  let output = Command::new(bin()).args(["--bogus-flag"]).output().unwrap();
  assert_eq!(output.status.code(), Some(2));
  let err: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(err["UsageError"]["stage"], "usage");
  let output = Command::new(bin()).args(["a", "b", "c"]).output().unwrap();
  assert_eq!(output.status.code(), Some(2));
  // `--json` and `-o -` both claim stdout.
  let output = Command::new(bin()).args(["main", "feature", "--json", "-o", "-"]).output().unwrap();
  assert_eq!(output.status.code(), Some(2));

  let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn machine_mode_refuses_non_canonical_invocations() {
  // Piped stdout = machine mode: `--no-color` must be refused with the
  // canonical form told back, as a single-key document, exit 2.
  let output = Command::new(bin()).args(["main", "--no-color"]).output().unwrap();
  assert_eq!(output.status.code(), Some(2));
  let err: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(err["NonCanonicalInvocation"]["given"], "--no-color");
  assert_eq!(err["NonCanonicalInvocation"]["canonical"], "--color=never");
  assert_eq!(err["NonCanonicalInvocation"]["stage"], "usage");
}

#[test]
fn help_is_comprehensive_and_free() {
  // No arguments → comprehensive help on stdout, exit 0.
  let output = Command::new(bin()).output().unwrap();
  assert_eq!(output.status.code(), Some(0));
  let text = String::from_utf8_lossy(&output.stdout);
  assert!(text.contains("USAGE:"));
  assert!(text.contains("NOT THIS TOOL'S JOB:"));
  assert!(text.contains("help exitcodes"));
  // The help text is what users actually read, so it must describe the
  // notes-commit convention as it BEHAVES — the lift is by path, and the
  // environment variable is only an on/off switch. These assertions exist
  // because the help once outlived a change to that rule.
  assert!(text.contains("PACKDIFF_SYSTEM_USER_EMAIL"));
  assert!(text.contains("PR-DECISION-<topic>.md"), "both notes kinds are named");
  assert!(text.contains("Authorship does not matter"), "the lift is by path, not by author");
  assert!(
    !text.contains("notes-author email") && !text.contains("commits it authored"),
    "the help must not claim authorship decides what is notes"
  );
  // Likewise, the NOT-THIS-TOOL'S-JOB list must not disown shipped features.
  for shipped in ["side-by-side", "syntax highlighting"] {
    let after = text.split("NOT THIS TOOL'S JOB:").nth(1).unwrap_or("");
    assert!(!after.contains(shipped), "`{shipped}` ships today; it cannot be listed as deferred");
  }

  // `help exitcodes` prints the complete table.
  let output = Command::new(bin()).args(["help", "exitcodes"]).output().unwrap();
  assert_eq!(output.status.code(), Some(0));
  let text = String::from_utf8_lossy(&output.stdout);
  for code in ["0", "2", "3", "4", "5", "130"] {
    assert!(text.contains(code), "exit code {code} missing from the table");
  }
  assert!(text.contains("stage"));

  // `--help` and `-h` also work.
  for flag in ["--help", "-h"] {
    let output = Command::new(bin()).args([flag]).output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{flag}");
  }
}

#[test]
fn range_syntax_and_head_default() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH");
    return;
  }
  let tmp = tmpdir("ranges");
  let repo = tmp.join("sample");
  make_repo(&repo);

  // `main...feature` = merge-base semantics: mainline drift excluded.
  let output = Command::new(bin()).args(["main...feature", "-C", repo.to_str().unwrap(), "-o", "-"]).output().unwrap();
  assert!(output.status.success());
  let html = String::from_utf8_lossy(&output.stdout);
  assert!(html.contains("newfile.md"));
  assert!(!html.contains("mainline.txt"));

  // `main..feature` = literal two-dot: drift included as a reverse change.
  let output = Command::new(bin()).args(["main..feature", "-C", repo.to_str().unwrap(), "-o", "-"]).output().unwrap();
  assert!(output.status.success());
  assert!(String::from_utf8_lossy(&output.stdout).contains("mainline.txt"));

  // Single ref: HEAD defaults to the current checkout.
  git(&repo, &["checkout", "-q", "feature"]);
  let out_file = tmp.join("head-default.html");
  let output = Command::new(bin())
    .args(["main", "-C", repo.to_str().unwrap(), "-o", out_file.to_str().unwrap()])
    .output()
    .unwrap();
  assert!(output.status.success());
  let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(doc["Packed"]["head"]["name"], "HEAD");
  assert_eq!(doc["Packed"]["commits"], 2, "HEAD == feature here");

  let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn head_refs_resolve_with_carets_and_case_insensitively() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH");
    return;
  }
  let tmp = tmpdir("headrefs");
  let repo = tmp.join("sample");
  make_repo(&repo);
  git(&repo, &["checkout", "-q", "feature"]);

  // `HEAD^` as BASE: exactly the last commit of `feature` is in range.
  for base in ["HEAD^", "head^", "Head^"] {
    let output = Command::new(bin()).args([base, "-C", repo.to_str().unwrap(), "-o", "-"]).output().unwrap();
    assert!(output.status.success(), "{base}: {}", String::from_utf8_lossy(&output.stderr));
    let html = String::from_utf8_lossy(&output.stdout);
    assert!(html.contains("newfile.md"), "{base} spans the last feature commit");
    assert!(!html.contains("feature change one"), "{base} excludes the first feature commit");
    // Single-commit ranges carry snapshots (for expand-context) but their
    // one commit is not range-selectable — there is nothing to filter.
    assert!(html.contains(r#"id="packdiff-snapshots""#), "{base}: snapshots power expand-context");
    assert!(!html.contains("commit selectable"), "{base}: a single commit has nothing to range-filter");
  }

  // Deep caret chains work too: `head^^` == the merge base here, so the diff
  // covers both feature commits.
  let output = Command::new(bin()).args(["head^^", "-C", repo.to_str().unwrap(), "-o", "-"]).output().unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let html = String::from_utf8_lossy(&output.stdout);
  assert!(html.contains("feature change one") && html.contains("feature change two"));

  let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn machine_mode_streams_progress_on_stderr() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH");
    return;
  }
  let tmp = tmpdir("progress");
  let repo = tmp.join("sample");
  make_repo(&repo);
  let out = tmp.join("diff.html");
  let output = Command::new(bin())
    .args(["main", "feature", "-C", repo.to_str().unwrap(), "-o", out.to_str().unwrap()])
    .output()
    .unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));

  // stderr: every non-empty line is one single-key `Progress` document.
  // (Stage changes report immediately, so even this sub-second run streams.)
  let stderr = String::from_utf8_lossy(&output.stderr);
  let reports: Vec<serde_json::Value> = stderr
    .lines()
    .filter(|l| !l.trim().is_empty())
    .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("non-JSON stderr line {l:?}: {e}")))
    .collect();
  assert!(!reports.is_empty(), "no progress lines at all");
  let mut prev_percent = 0;
  for r in &reports {
    let obj = r.as_object().expect("progress line is an object");
    assert_eq!(obj.keys().collect::<Vec<_>>(), ["Progress"], "single-key union document");
    let p = &r["Progress"];
    assert!(p["stage"].is_string() && p["elapsed_ms"].is_u64(), "{p}");
    let (done, total) = (p["done"].as_u64().unwrap(), p["total"].as_u64().unwrap());
    assert!(done <= total, "done exceeds total: {p}");
    // `done/total` are per-stage; `percent` is the whole-run number and is
    // guaranteed monotonic — the "never jumps backwards" contract.
    let percent = p["percent"].as_u64().unwrap();
    assert!(percent <= 100, "{p}");
    assert!(percent >= prev_percent, "percent went backwards: {p}");
    prev_percent = percent;
  }
  let stages: Vec<&str> = reports.iter().map(|r| r["Progress"]["stage"].as_str().unwrap()).collect();
  for stage in ["Resolve", "MergeBase", "Diff", "Commits", "Scan", "Snapshots", "Render", "Write", "Done"] {
    assert!(stages.contains(&stage), "stage {stage} never reported: {stages:?}");
  }
  let last = &reports.last().unwrap()["Progress"];
  assert_eq!(last["stage"], "Done", "the stream ends with Done");
  assert_eq!(last["percent"], 100, "Done reports full completion");

  // stdout stays exactly one `Packed` document — progress never leaks there.
  let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert!(doc.get("Packed").is_some());

  // A failing run must NOT report `Done`.
  let output = Command::new(bin()).args(["main", "no-such-branch", "-C", repo.to_str().unwrap()]).output().unwrap();
  assert_eq!(output.status.code(), Some(4));
  let stderr = String::from_utf8_lossy(&output.stderr);
  assert!(!stderr.contains("\"Done\""), "error path leaked a Done report: {stderr}");

  let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn default_output_filename() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH");
    return;
  }
  let tmp = tmpdir("outname");
  let repo = tmp.join("sample");
  make_repo(&repo);

  let output =
    Command::new(bin()).current_dir(&tmp).args(["main", "feature", "-C", repo.to_str().unwrap()]).output().unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let expected = tmp.join("packdiff-main-feature.html");
  assert!(expected.is_file(), "default filename derives from the refs");
  let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(doc["Packed"]["out"], "packdiff-main-feature.html");
  // Piped stdout carries no ANSI escapes, ever.
  assert!(!String::from_utf8_lossy(&output.stdout).contains('\u{1b}'));

  // Slashes in ref names sanitize.
  let output = Command::new(bin())
    .current_dir(&tmp)
    .args(["heads/main", "feature", "-C", repo.to_str().unwrap()])
    .output()
    .unwrap();
  assert!(output.status.success());
  assert!(tmp.join("packdiff-heads-main-feature.html").is_file());

  let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn notes_commits_lift_journaled_decisions_into_their_own_panels() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH");
    return;
  }
  let tmp = tmpdir("decisions");
  let repo = tmp.join("sample");
  make_repo(&repo);
  git(&repo, &["checkout", "-q", "feature"]);
  // One notes commit carrying the description AND a decision together, then a
  // second decision on its own — both shapes are notes.
  write(&repo, "PR-DESCRIPTION.md", b"# Add evil\n\nThis PR adds `evil()`.\n");
  write(&repo, "PR-DECISION-retry-safety.md", b"# Retry safety\n\nOnly unprocessed requests retry.\n");
  git(&repo, &["add", "-A"]);
  git(&repo, &["-c", "user.email=notes-bot@example.com", "commit", "-qm", "pr notes"]);
  write(&repo, "PR-DECISION-schema.md", b"# Schema\n\nAdditive field, so no bump.\n");
  git(&repo, &["add", "-A"]);
  git(&repo, &["-c", "user.email=notes-bot@example.com", "commit", "-qm", "decision: schema"]);
  // A decision-shaped file NOT at the root, committed alongside a root one:
  // the nested path is ordinary documentation, which makes the whole commit
  // MIXED — so neither file lifts. Confinement is what decides.
  std::fs::create_dir_all(repo.join("docs")).unwrap();
  write(&repo, "docs/PR-DECISION-nested.md", b"# Nested\n\nDocumentation, not notes.\n");
  write(&repo, "PR-DECISION-mixed.md", b"# Mixed\n\nRode in with ordinary docs.\n");
  git(&repo, &["add", "-A"]);
  git(&repo, &["commit", "-qm", "docs and a mixed decision"]);

  let out = tmp.join("decisions.html");
  let dump = tmp.join("decisions.json");
  let output = Command::new(bin())
    .env("PACKDIFF_SYSTEM_USER_EMAIL", "notes-bot@example.com")
    .args([
      "main",
      "feature",
      "-C",
      repo.to_str().unwrap(),
      "-o",
      out.to_str().unwrap(),
      "--dump-json",
      dump.to_str().unwrap(),
    ])
    .output()
    .unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let packed = &serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["Packed"];
  assert_eq!(packed["description"], "PR-DESCRIPTION.md");
  assert_eq!(
    packed["decisions"],
    serde_json::json!(["PR-DECISION-retry-safety.md", "PR-DECISION-schema.md"]),
    "root decisions confined to their own commits lift, in path order"
  );
  assert_eq!(packed["notes_commits"].as_array().unwrap().len(), 2);

  let typed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&dump).unwrap()).unwrap();
  let decisions = typed["decisions"].as_array().unwrap();
  assert_eq!(decisions.len(), 2);
  assert!(decisions[0]["text"].as_str().unwrap().starts_with("# Retry safety"));
  assert!(!serde_json::to_string(&typed["snapshots"]).unwrap().contains("PR-DECISION-retry-safety"));

  let html = std::fs::read_to_string(&out).unwrap();
  assert!(html.contains(r##"href="#decisions">Decisions</a>"##), "the nav offers the section");
  assert!(html.contains(r#"data-anchor="PR-DECISION-retry-safety.md""#), "each decision is commentable");
  assert!(html.contains("<h1>Retry safety</h1>"), "decisions render at build time");
  assert!(!html.contains("decision: schema"), "the notes commit stays off the commits table");
  assert!(!html.contains(">PR-DECISION-schema.md</a>"), "lifted decisions leave the Files changed index");
  // The mixed commit's files stay ordinary code under review.
  assert!(html.contains(">PR-DECISION-mixed.md</a>"), "a decision sharing its commit with code is not lifted");
  assert!(html.contains(">docs/PR-DECISION-nested.md</a>"), "a nested decision file is not a notes file");
  assert_eq!(html.matches(r#"class="notes-panel decision""#).count(), 2, "exactly the two lifted decisions");

  let _ = std::fs::remove_dir_all(&tmp);
}

/// Committing `PR-DESCRIPTION.md` more than once is malformed history. The
/// page must say so, keep every version commentable, and still hide all of
/// the description commits from the commit list and the diff.
#[test]
fn several_description_commits_are_flagged_and_all_versions_stay_commentable() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH");
    return;
  }
  let tmp = tmpdir("multidesc");
  let repo = tmp.join("sample");
  make_repo(&repo);
  git(&repo, &["checkout", "-q", "feature"]);
  write(&repo, "PR-DESCRIPTION.md", b"# Draft one\n\nFirst attempt.\n");
  git(&repo, &["add", "-A"]);
  git(&repo, &["commit", "-qm", "Add PR-DESCRIPTION.md"]);
  write(&repo, "PR-DESCRIPTION.md", b"# Draft two\n\nSecond attempt.\n");
  git(&repo, &["add", "-A"]);
  git(&repo, &["commit", "-qm", "Prepare PR description."]);
  write(&repo, "PR-DESCRIPTION.md", b"# Final\n\nWhat the change actually claims.\n");
  git(&repo, &["add", "-A"]);
  git(&repo, &["commit", "-qm", "Rewrite the PR description."]);

  let out = tmp.join("multidesc.html");
  let dump = tmp.join("multidesc.json");
  let output = Command::new(bin())
    .args([
      "main",
      "feature",
      "-C",
      repo.to_str().unwrap(),
      "-o",
      out.to_str().unwrap(),
      "--dump-json",
      dump.to_str().unwrap(),
    ])
    .output()
    .unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let packed = &serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["Packed"];
  assert_eq!(packed["commits"], 2, "all three description commits are hidden");
  assert_eq!(packed["files"], 5, "PR-DESCRIPTION.md never enters the diff");
  assert_eq!(packed["description"], "PR-DESCRIPTION.md");
  assert_eq!(packed["superseded_descriptions"].as_array().unwrap().len(), 2);
  // The malformed history is loud in both channels.
  let warnings = packed["warnings"].as_array().unwrap();
  assert_eq!(warnings.len(), 1, "{warnings:?}");
  assert!(warnings[0].as_str().unwrap().contains("3 separate commits write PR-DESCRIPTION.md"));
  assert!(String::from_utf8_lossy(&output.stderr).contains("warning: malformed commit history"));

  let typed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&dump).unwrap()).unwrap();
  assert!(typed["description"]["text"].as_str().unwrap().starts_with("# Final"), "newest first");
  let older = typed["superseded_descriptions"].as_array().unwrap();
  assert!(older[0]["text"].as_str().unwrap().starts_with("# Draft two"));
  assert!(older[1]["text"].as_str().unwrap().starts_with("# Draft one"));

  let html = std::fs::read_to_string(&out).unwrap();
  assert!(html.contains(r#"id="malformed-history" class="notice notice-warn""#), "the banner is on the page");
  assert!(html.contains("3 separate commits write <code>PR-DESCRIPTION.md</code>"));
  // Every version renders, newest first, each with its own comment anchor.
  assert!(html.contains(r#"<section id="description" class="notes-panel" data-anchor="PR-DESCRIPTION.md">"#));
  assert_eq!(html.matches(r#"class="notes-panel superseded""#).count(), 2);
  for heading in ["<h1>Final</h1>", "<h1>Draft two</h1>", "<h1>Draft one</h1>"] {
    assert!(html.contains(heading), "missing {heading}");
  }
  assert!(html.find("<h1>Final</h1>").unwrap() < html.find("<h1>Draft one</h1>").unwrap(), "newest first");
  // Anchors are per-revision, so a comment on a draft stays on that draft.
  let anchors: std::collections::BTreeSet<&str> = html
    .match_indices(r#"<section class="notes-panel superseded" id="description-"#)
    .map(|(i, _)| {
      let rest = &html[i..];
      let start = rest.find(r#"data-anchor="PR-DESCRIPTION.md@"#).unwrap();
      let value = &rest[start + r#"data-anchor=""#.len()..];
      &value[..value.find('"').unwrap()]
    })
    .collect();
  assert_eq!(anchors.len(), 2, "each superseded version anchors to its own revision: {anchors:?}");
  assert!(html.contains(r#"class="badge badge-current""#));
  assert_eq!(html.matches(r#"class="badge badge-superseded""#).count(), 2);
  assert!(html.contains("Prepare PR description."), "each panel names the commit it came from");
  // Named in the panel headings, never in the commit table or file index.
  assert_eq!(html.matches(r#"class="commit selectable""#).count(), 2, "only the two code commits are listed");
  assert!(!html.contains(">PR-DESCRIPTION.md</a>"), "not in the Files changed index");

  let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn notes_commits_lift_into_the_description_panel() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH");
    return;
  }
  let tmp = tmpdir("notes");
  let repo = tmp.join("sample");
  make_repo(&repo);
  // A notes commit on top of `feature`: PR-DESCRIPTION.md alone. The
  // trailing `-c` outranks the helper's default user.email — authorship no
  // longer decides, but the fixture keeps a distinct identity so the
  // kill-switch case below is exercised against a real second author.
  git(&repo, &["checkout", "-q", "feature"]);
  write(&repo, "PR-DESCRIPTION.md", b"# Add evil\n\nThis PR adds `evil()`.\n\n- point one\n- point two\n");
  git(&repo, &["add", "-A"]);
  git(&repo, &["-c", "user.email=notes-bot@example.com", "commit", "-qm", "pr notes"]);
  // A CODE commit by the SAME bot identity (an orchestrator like scsh authors every
  // integrated commit as one bot): it touches code, so it must stay on the page —
  // only commits CONFINED to the notes file are notes.
  write(&repo, "bot_code.txt", b"the bot wrote code too\n");
  git(&repo, &["add", "-A"]);
  git(&repo, &["-c", "user.email=notes-bot@example.com", "commit", "-qm", "bot code"]);

  // The commit hides, the file lifts.
  let out = tmp.join("with-notes.html");
  let dump = tmp.join("with-notes.json");
  let output = Command::new(bin())
    .env("PACKDIFF_SYSTEM_USER_EMAIL", "notes-bot@example.com")
    .args([
      "main",
      "feature",
      "-C",
      repo.to_str().unwrap(),
      "-o",
      out.to_str().unwrap(),
      "--dump-json",
      dump.to_str().unwrap(),
    ])
    .output()
    .unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  let packed = &doc["Packed"];
  assert_eq!(packed["commits"], 3, "only the notes commit is hidden — the bot's CODE commit counts");
  assert_eq!(packed["files"], 6, "PR-DESCRIPTION.md is lifted out of the diff");
  assert_eq!(packed["description"], "PR-DESCRIPTION.md");
  assert_eq!(packed["notes_commits"].as_array().unwrap().len(), 1);

  let html = std::fs::read_to_string(&out).unwrap();
  assert!(html.contains(r#"<section id="description" class="notes-panel" data-anchor="PR-DESCRIPTION.md">"#));
  assert!(html.contains(r##"href="#description""##));
  assert!(!html.contains(r#"id="decisions""#), "this range journals no decisions");
  assert!(html.contains(r#"data-file="PR-DESCRIPTION.md" data-side="New" data-line="1""#));
  assert!(html.contains("<h1>Add evil</h1>"), "the description renders at build time");
  // Markdown | Source: rendered card by default, source table hidden until toggled.
  assert!(html.contains(r#"class="seg md-seg desc-seg""#));
  assert!(html.contains(r#"data-mdview="raw""#));
  assert!(html.contains(">Markdown</button><button type=\"button\" data-mdview=\"raw\""));
  assert!(html.contains(r#"class="desc-raw" hidden"#));
  assert!(html.contains(r#"class="diff unified desc-source""#));
  assert!(html.contains("# Add evil"), "raw source carries the markdown text");
  assert!(!html.contains("pr notes"), "the notes commit stays off the commits table");
  assert!(html.contains("bot code"), "the bot-authored CODE commit stays ON the commits table");
  assert!(!html.contains(">PR-DESCRIPTION.md</a>"), "not in the Files changed index");
  // No provenance chrome: the panel is just the description. The path
  // appears ONLY inside comment anchors, never as visible text.
  assert_eq!(
    html.matches("PR-DESCRIPTION").count(),
    html.matches(r#"data-file="PR-DESCRIPTION.md""#).count()
      + html.matches(r#"data-anchor="PR-DESCRIPTION.md""#).count(),
    "the path must not appear outside comment anchors"
  );
  assert!(!html.contains("notes author"), "the page never names the convention");

  let typed: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&dump).unwrap()).unwrap();
  assert!(typed["description"]["text"].as_str().unwrap().starts_with("# Add evil"));
  let boundaries = typed["snapshots"]["boundaries"].as_array().unwrap();
  assert_eq!(boundaries.len(), 4, "merge-base plus the three CODE commits only");
  assert!(!serde_json::to_string(&typed["snapshots"]).unwrap().contains("PR-DESCRIPTION"));

  // Authorship is NOT part of the test. With no configured notes email at
  // all — the built-in default, which this repo's bot does not match — the
  // same commit still lifts: it is confined to the description, and a
  // description is metadata about the change whoever signed it.
  let any_author_out = tmp.join("any-author.html");
  let output = Command::new(bin())
    .env_remove("PACKDIFF_SYSTEM_USER_EMAIL")
    .args(["main", "feature", "-C", repo.to_str().unwrap(), "-o", any_author_out.to_str().unwrap()])
    .output()
    .unwrap();
  assert!(output.status.success());
  let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(doc["Packed"]["commits"], 3, "the notes commit lifts on its paths alone");
  assert_eq!(doc["Packed"]["files"], 6);
  assert_eq!(doc["Packed"]["description"], "PR-DESCRIPTION.md");
  let html = std::fs::read_to_string(&any_author_out).unwrap();
  assert!(html.contains(r#"id="description""#));
  assert!(!html.contains("pr notes"));

  // An EMPTY notes email is the kill switch: the convention is off, so the
  // commit and the file are ordinary code again.
  let plain_out = tmp.join("plain.html");
  let output = Command::new(bin())
    .env("PACKDIFF_SYSTEM_USER_EMAIL", "")
    .args(["main", "feature", "-C", repo.to_str().unwrap(), "-o", plain_out.to_str().unwrap()])
    .output()
    .unwrap();
  assert!(output.status.success());
  let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  assert_eq!(doc["Packed"]["commits"], 4);
  assert_eq!(doc["Packed"]["files"], 7);
  assert!(doc["Packed"]["description"].is_null());
  let html = std::fs::read_to_string(&plain_out).unwrap();
  assert!(!html.contains(r#"id="description""#));
  assert!(html.contains("pr notes"));

  let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn notes_commits_lift_around_a_merge_commit_in_two_dot_mode() {
  if !git_available() {
    eprintln!("SKIP: git not found on PATH");
    return;
  }
  let tmp = tmpdir("notes-merge");
  let repo = tmp.join("sample");
  make_repo(&repo);
  // A merge commit inside the range: the batched changed-paths scan skips
  // merges (git log lists no files for them), so classifying this range
  // exercises the per-commit fallback. Merge `main` into `feature`, then a
  // notes commit on top — two-dot mode keeps the merge in the range.
  git(&repo, &["checkout", "-q", "feature"]);
  git(&repo, &["merge", "-q", "--no-ff", "-m", "merge mainline", "main"]);
  write(&repo, "PR-DESCRIPTION.md", b"# With a merge\n\nThe range carries a merge commit.\n");
  git(&repo, &["add", "-A"]);
  git(&repo, &["commit", "-qm", "pr notes"]);

  let out = tmp.join("merge-notes.html");
  let output = Command::new(bin())
    .args(["main..feature", "-C", repo.to_str().unwrap(), "-o", out.to_str().unwrap()])
    .output()
    .unwrap();
  assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
  let doc: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
  let packed = &doc["Packed"];
  assert_eq!(packed["description"], "PR-DESCRIPTION.md", "the notes commit lifts with a merge in the range");
  assert_eq!(packed["notes_commits"].as_array().unwrap().len(), 1);
  assert_eq!(packed["commits"], 3, "two code commits plus the merge stay; only the notes commit hides");
  let html = std::fs::read_to_string(&out).unwrap();
  assert!(html.contains("merge mainline"), "the merge commit stays on the commits table");
  assert!(!html.contains("pr notes"), "the notes commit stays off the commits table");

  let _ = std::fs::remove_dir_all(&tmp);
}
