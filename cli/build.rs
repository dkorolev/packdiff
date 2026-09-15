//! Hand the packdiff wasm comment engine to the crate via the
//! `PACKDIFF_WASM_PATH` rustc-env, so that `cargo build`, `cargo test`, and
//! `cargo install` are self-sufficient.
//!
//! Two modes:
//!
//! - **Workspace** (git checkout): the sibling `../wasm` crate exists — build
//!   it for `wasm32-unknown-unknown`, into a SEPARATE --target-dir
//!   (target-wasm/) so the nested cargo cannot deadlock against the outer
//!   cargo's lock on target/. This is the only mode that needs the wasm target.
//! - **Packaged** (crates.io tarball: no sibling crate): the engine ships
//!   inside the tarball as `engine/packdiff_wasm.wasm`, staged there by
//!   `./stage-engine.sh` right before `cargo package`/`cargo publish`. Nothing
//!   is compiled for wasm, so `cargo install packdiff` — and any crate that
//!   depends on `packdiff` — builds on a plain stable toolchain.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where `./stage-engine.sh` puts the engine, relative to this crate's root.
const STAGED_ENGINE: &str = "engine/packdiff_wasm.wasm";

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
  let sibling = manifest.parent().map(Path::to_path_buf).filter(|ws| ws.join("wasm/Cargo.toml").is_file());

  let wasm = match sibling {
    Some(ws) => {
      let wasm = build_in_workspace(&ws);
      println!("cargo:rerun-if-changed={}", ws.join("wasm/src").display());
      println!("cargo:rerun-if-changed={}", ws.join("dto/src").display());
      wasm
    }
    None => {
      let staged = manifest.join(STAGED_ENGINE);
      assert!(
        staged.is_file(),
        "no compiled engine at {}: this tarball was packaged without running ./stage-engine.sh first",
        staged.display()
      );
      println!("cargo:rerun-if-changed={}", staged.display());
      staged
    }
  };
  assert!(wasm.is_file(), "expected wasm artifact at {}", wasm.display());
  println!("cargo:rustc-env=PACKDIFF_WASM_PATH={}", wasm.display());
}

fn build_in_workspace(workspace: &Path) -> PathBuf {
  let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
  let target_dir = workspace.join("target-wasm");
  let status = Command::new(cargo)
    .current_dir(workspace)
    .args(["build", "-p", "packdiff-wasm", "--release", "--target", "wasm32-unknown-unknown", "--target-dir"])
    .arg(&target_dir)
    .status()
    .unwrap_or_else(|e| panic!("failed to invoke cargo for packdiff-wasm: {e}"));
  assert!(
    status.success(),
    "packdiff-wasm build failed — is the wasm target installed? (rustup target add wasm32-unknown-unknown)"
  );
  target_dir.join("wasm32-unknown-unknown/release/packdiff_wasm.wasm")
}
