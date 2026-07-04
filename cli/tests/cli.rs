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
  // Hostile content is escaped; the only script tags are our own three
  // (config JSON, wasm base64, app JS).
  assert!(!html.contains("<script>alert(1)</script>"));
  assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
  assert_eq!(html.matches("<script").count(), 3);
  // The wasm module is inlined (base64 of `\0asm`).
  assert!(html.contains("id=\"packdiff-wasm\">AGFzbQ"));
  // Comment anchors use the CamelCase `Side` variant names.
  assert!(html.contains("data-file=\"hello.py\" data-side=\"New\""));
  assert!(html.contains("data-side=\"Old\""));
  assert!(html.contains("torename.txt → renamed.txt"));
  assert!(html.contains("Binary file — contents not shown."));
  assert!(html.contains("feature change one"));
  for id in ["export-json", "export-md", "export-csv", "copy-md", "import-json"] {
    assert!(html.contains(&format!("id=\"{id}\"")), "missing #{id}");
  }

  // The dumped DiffDocument parses back through the dto schema, with
  // single-key line unions.
  let doc: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&dump).unwrap()).unwrap();
  assert_eq!(doc["schema_version"], 1);
  assert_eq!(doc["files"].as_array().unwrap().len(), 5);
  assert_eq!(doc["base"]["name"], "main");
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
