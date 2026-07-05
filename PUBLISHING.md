# Publishing to crates.io

packdiff is a three-crate workspace, and the crates depend on each other, so they must be published **in dependency order** and their versions kept in lockstep. This page is the maintainer playbook.

## Quick reference

Publish the three crates **in this exact order** (each must be on crates.io before the next, which depends on it, can build):

```console
$ cargo publish -p packdiff-dto     # 1. the data model — no internal deps
$ cargo publish -p packdiff-wasm    # 2. depends on packdiff-dto
$ cargo publish -p packdiff         # 3. depends on packdiff-dto, and pulls packdiff-wasm from the registry
```

The order is mandatory, not a convention: `wasm` and `cli` depend on `dto`, and the `cli` crate's build script fetches `packdiff-wasm` **from the registry** to compile the inlined engine — so `cargo publish -p packdiff` only succeeds once the first two have landed. Everything below explains why and how to get there safely; the step-by-step release checklist is [further down](#releasing-step-by-step).

## The dependency graph

```
packdiff-dto   (dto/)   — pure data model, no internal deps
      ▲
      ├────────────── packdiff-wasm (wasm/)  depends on packdiff-dto
      │
      └────────────── packdiff      (cli/)   depends on packdiff-dto,
                                             AND builds packdiff-wasm from the
                                             registry in its build script
```

Two consequences:

- **Publish order is `dto` → `wasm` → `cli`.** A crate cannot be published until every crate it depends on already exists on crates.io at the version it asks for.
- The `packdiff` (cli) crate is special: its `build.rs` compiles the wasm comment engine by generating a shim crate that depends on `packdiff-wasm = "=<this version>"` **from the registry** (see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#build-pipeline)). So `packdiff-wasm` must be published *before* `cargo publish -p packdiff` runs its verification build. The dry run below sidesteps this with `PACKDIFF_WASM_SRC`.

## Where versions live (the reconciliation)

Every crate shares one version, and it is defined in as few places as possible:

| What | Where | How |
| --- | --- | --- |
| Each crate's own version | `dto`, `wasm`, `cli` `Cargo.toml` | `version.workspace = true` — inherited, never written per-crate |
| The shared version number | `Cargo.toml` → `[workspace.package] version` | The single source of truth |
| The internal `packdiff-dto` dependency pin | `Cargo.toml` → `[workspace.dependencies] packdiff-dto` | `{ path = "dto", version = "X" }`; `cli` and `wasm` reference it as `{ workspace = true }` |
| `packdiff-wasm` pin used by the cli build script | `cli/build.rs` | `packdiff-wasm = "=<CARGO_PKG_VERSION>"` — derived automatically from the cli crate's own version, so it is never written by hand |

So a release touches exactly **two** literals, both in the root `Cargo.toml`: `[workspace.package] version` and the `packdiff-dto` version in `[workspace.dependencies]`. Keep them equal.

Why the internal dep carries both `path` and `version`: inside the workspace cargo resolves it by `path`; when publishing, cargo strips the `path` and the published crate depends on the `version`. A path-only dependency cannot be published.

### Optional: let a tool keep them in lockstep

`cargo release` (or `cargo workspaces version`) bumps the workspace version, rewrites the internal dep pins, commits, tags, and publishes each crate in dependency order — removing the chance of the two literals drifting. If you adopt it, the manual steps below collapse to `cargo release <level>`.

## Releasing, step by step

1. **Land all changes and go green.** `./test.sh` must pass; the working tree should be clean (publishing from a dirty tree needs `--allow-dirty` and is discouraged).

2. **Bump the version.** Edit the two literals in the root `Cargo.toml` (`[workspace.package] version` and `[workspace.dependencies] packdiff-dto` version) to the new `X.Y.Z`. Update the `packdiff <version>` line in the README's install example. Run `cargo build` once so `Cargo.lock` picks up the new version, then commit.

3. **Dry run — verify all three tarballs before anything leaves the machine.** Because `packdiff-wasm` is not on the registry yet, point the cli build script at the local wasm crate:

   ```console
   $ PACKDIFF_WASM_SRC=$PWD/wasm cargo package --workspace
   ```

This packages and verify-builds `dto`, `wasm`, and `cli` against the local sources. It must finish clean.

4. **Publish in order, waiting for each to land.** Recent cargo waits for the index to update before returning, so the next publish sees the dependency:

   ```console
   $ cargo publish -p packdiff-dto
   $ cargo publish -p packdiff-wasm
   $ cargo publish -p packdiff
   ```

The third command's verification build pulls `packdiff-wasm` from the registry (no `PACKDIFF_WASM_SRC`), which is exactly what a user's `cargo install packdiff` will do — so a green publish confirms the install path too.

5. **Tag and push.**

   ```console
   $ git tag v X.Y.Z && git push --tags
   ```

6. **Verify the published install path.**

   ```console
   $ cargo install packdiff --version X.Y.Z
   $ packdiff --version
   ```

## Version policy (per the engineering principles)

- Inside the workspace, before first publish, break freely — no versioning ceremony.
- Once published, a **breaking change bumps the minor version** (`0.1.z` → `0.2.0`), never the patch; patch releases must always be safe to take. The machine-mode `--json` shapes and the exit-code table are public API — a change to what a field or code *means* is breaking.
- The `schema_version` inside the documents is a separate, independent axis; it changes only when the stored document shape changes (see [docs/DATA-MODEL.md](docs/DATA-MODEL.md)).
