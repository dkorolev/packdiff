//! Build the packdiff wasm comment engine and hand its artifact path to the
//! CLI via the PACKDIFF_WASM_PATH env, so `cargo build`, `cargo test`, and
//! `cargo install` are self-sufficient.
//!
//! Two modes:
//!
//! - **Workspace** (git checkout): the sibling `../wasm` crate exists — build
//!   it directly, into a SEPARATE --target-dir (target-wasm/) so the nested
//!   cargo cannot deadlock against the outer cargo's lock on target/.
//! - **Packaged** (crates.io tarball: no sibling crate): generate a minimal
//!   cdylib shim project in OUT_DIR that links `packdiff-wasm` from the
//!   registry (version-pinned to this crate's own version) and build that.
//!   The shim's only job is to trigger the cdylib link; the `#[no_mangle]`
//!   `pd_*` exports come from the linked crate.
//!
//! Env overrides (mainly for pre-publish verification):
//! - PACKDIFF_WASM_FORCE_SHIM=1 — use the shim even in a checkout.
//! - PACKDIFF_WASM_SRC=<dir>    — shim depends on that path instead of the registry.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
  // docs.rs builds run with no network and no wasm32 target, so the engine
  // cannot be built there — and rustdoc never executes it. An empty stub
  // keeps `include_bytes!` satisfied and the API docs building.
  if env::var_os("DOCS_RS").is_some() {
    let stub = PathBuf::from(env::var("OUT_DIR").unwrap()).join("engine-stub.wasm");
    std::fs::write(&stub, []).expect("write the docs.rs engine stub");
    println!("cargo:rustc-env=PACKDIFF_WASM_PATH={}", stub.display());
    return;
  }

  let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
  let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());

  let workspace = manifest.parent().map(|p| p.to_path_buf());
  let sibling = workspace.clone().filter(|ws| ws.join("wasm/Cargo.toml").is_file());
  let force_shim = env::var_os("PACKDIFF_WASM_FORCE_SHIM").is_some();

  let wasm = match (&sibling, force_shim) {
    (Some(ws), false) => build_in_workspace(&cargo, ws),
    _ => build_via_shim(&cargo),
  };
  assert!(wasm.is_file(), "expected wasm artifact at {}", wasm.display());
  println!("cargo:rustc-env=PACKDIFF_WASM_PATH={}", wasm.display());

  if let Some(ws) = &sibling {
    println!("cargo:rerun-if-changed={}", ws.join("wasm/src").display());
    println!("cargo:rerun-if-changed={}", ws.join("dto/src").display());
  }
  println!("cargo:rerun-if-env-changed=PACKDIFF_WASM_FORCE_SHIM");
  println!("cargo:rerun-if-env-changed=PACKDIFF_WASM_SRC");
}

fn run(cmd: &mut Command, what: &str) {
  let status = cmd.status().unwrap_or_else(|e| panic!("failed to invoke cargo for {what}: {e}"));
  assert!(
    status.success(),
    "{what} build failed — is the wasm target installed? (rustup target add wasm32-unknown-unknown)"
  );
}

fn build_in_workspace(cargo: &str, workspace: &std::path::Path) -> PathBuf {
  let target_dir = workspace.join("target-wasm");
  run(
    Command::new(cargo)
      .current_dir(workspace)
      .args(["build", "-p", "packdiff-wasm", "--release", "--target", "wasm32-unknown-unknown", "--target-dir"])
      .arg(&target_dir),
    "packdiff-wasm",
  );
  target_dir.join("wasm32-unknown-unknown/release/packdiff_wasm.wasm")
}

fn build_via_shim(cargo: &str) -> PathBuf {
  let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
  let shim = out_dir.join("wasm-shim");
  std::fs::create_dir_all(shim.join("src")).expect("create shim dir");

  let dep = match env::var("PACKDIFF_WASM_SRC") {
    Ok(path) => format!("packdiff-wasm = {{ path = {path:?} }}"),
    Err(_) => format!("packdiff-wasm = \"={}\"", env!("CARGO_PKG_VERSION")),
  };
  std::fs::write(
    shim.join("Cargo.toml"),
    format!(
      r#"[package]
name = "packdiff-wasm-shim"
version = "0.0.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
{dep}

[profile.release]
opt-level = "z"
lto = true
panic = "abort"
strip = true
codegen-units = 1

[workspace]
"#
    ),
  )
  .expect("write shim Cargo.toml");
  std::fs::write(
    shim.join("src/lib.rs"),
    "// Link packdiff-wasm into a cdylib; its #[no_mangle] pd_* symbols are the API.\n\
     pub use packdiff_wasm::*;\n",
  )
  .expect("write shim lib.rs");

  run(
    Command::new(cargo)
      .current_dir(&shim)
      .args(["build", "--release", "--target", "wasm32-unknown-unknown", "--target-dir"])
      .arg(shim.join("target")),
    "packdiff-wasm (shim)",
  );
  shim.join("target/wasm32-unknown-unknown/release/packdiff_wasm_shim.wasm")
}
