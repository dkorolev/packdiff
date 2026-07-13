# Contributing

The house rules live in [`dkorolev/principles`](https://github.com/dkorolev/principles) — [`ENG-PRINCIPLES.md`](https://github.com/dkorolev/principles/blob/main/ENG-PRINCIPLES.md) for how everything here is built, and [`WEB-UI-PRINCIPLES.md`](https://github.com/dkorolev/principles/blob/main/WEB-UI-PRINCIPLES.md) for the generated review page. Linked, not copied: that repository is the one source of truth. Packdiff-specific product decisions and deliberately deferred ideas live in [`UI-CONCEPTS.md`](UI-CONCEPTS.md); they supplement the house rules rather than restating them.

## The two gates

The full test pass (`./test.sh`: `cargo fmt --check`, debug tests, release tests, the Node wasm-ABI test, and Playwright browser UI tests) runs at exactly two points, and both must be green:

1. **Pre-push hook** — enable once per clone:

   ```console
   $ git config core.hooksPath .githooks
   ```

Committing locally stays free and fast; the gate runs when code is about to leave the machine.

2. **CI** — `.github/workflows/ci.yml` runs the same `./test.sh` on every push to `main` and every pull request, plus two network-dependent jobs that cannot run in the offline gate: the packaged-tarball verification (the crates.io install path) and `cargo semver-checks` on `packdiff-dto`.

## Git conventions

- **Linear history only.** Rebase onto `main`; no merge commits.
- **Commit messages are short, complete sentences** — capital first letter, trailing period, `` `backticks` `` around identifiers and literals.
- **No `Co-Authored-By` trailers.**
- Keep changes small and single-purpose.

## Code conventions

- Formatting is `rustfmt.toml` (2-space, 120 columns); `cargo fmt --all` before pushing — the gate enforces it.
- All data shapes live in the `dto/` crate: strong typing, a doc comment on every field, single-key JSON unions (`{ "VariantName": { … } }`), `snake_case` fields / `CamelCase` variants, and `deny_unknown_fields` everywhere.
- Runtime-fallible operations return `Result` (`thiserror` enums); `unwrap()`/`expect()` only for provable invariants, with a message saying why the invariant holds.
- If a piece of logic is not unit-tested, a comment at that spot must say how it IS tested.
