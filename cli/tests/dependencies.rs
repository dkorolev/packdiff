//! The workspace has no third-party dependencies, by design: packdiff is a
//! first-party crate that brings nothing under it, so a consumer's
//! dependency tree — and its license audit — gains only packdiff's own
//! crates. This test keeps that a property, not an intention: any crate
//! from a registry, a git repository, or a foreign path fails it.

use std::process::Command;

use packdiff::dto::json;

#[test]
fn the_workspace_depends_on_nothing_third_party() {
  let workspace = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
  let out = Command::new(env!("CARGO"))
    .args(["metadata", "--format-version", "1", "--locked"])
    .current_dir(workspace)
    .output()
    .expect("cargo metadata runs");
  assert!(out.status.success(), "cargo metadata failed: {}", String::from_utf8_lossy(&out.stderr));
  let metadata = json::parse(std::str::from_utf8(&out.stdout).expect("metadata is UTF-8")).expect("metadata is JSON");
  let packages = metadata["packages"].as_array().expect("packages array");
  let mut names: Vec<&str> = packages.iter().map(|p| p["name"].as_str().expect("package name")).collect();
  names.sort_unstable();
  assert_eq!(names, ["packdiff", "packdiff-dto", "packdiff-wasm"], "every package in the graph is ours");
  for p in packages {
    assert!(p["source"].is_null(), "{} comes from {}: a third-party dependency", p["name"], p["source"]);
    assert_eq!(p["license"], "MIT", "{} is not MIT", p["name"]);
  }
}
