#!/usr/bin/env python3
"""The release version ladder, enforced on every PR (see PUBLISHING.md).

Checks, in order:

1. The PR's two version literals in the root Cargo.toml agree
   (`[workspace.package] version` and the `packdiff-dto` dependency pin).
2. All published modules (packdiff-dto, packdiff-wasm, packdiff) serve the
   same version on crates.io — a partial publish blocks everything until it
   is finished (in dependency order: dto, wasm, cli).
3. Versions move forward: crates.io never serves more than the base branch
   holds. The base branch being ahead of crates.io is fine — a pending
   release, reported as a warning.
4. The PR's version is exactly one above the base branch — next patch for
   a compatible release, or next minor / major for a breaking one (per the
   published-crate policy in PUBLISHING.md).

Base case: while nothing is published yet, steps 2 and 3 are skipped with a
warning.

Runs in CI with BASE_REPO/BASE_REF from the pull-request event; runnable
locally too (defaults below). Exits non-zero with a `::error::` line on the
first violated rule.
"""

import json
import os
import re
import sys
import urllib.error
import urllib.request

USER_AGENT = "packdiff-version-gate (https://github.com/dkorolev/packdiff)"

MODULES = ["packdiff-dto", "packdiff-wasm", "packdiff"]


def fail(message):
    print(f"::error::{message}")
    sys.exit(1)


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    return urllib.request.urlopen(request, timeout=30).read().decode()


def parse_version(manifest_text, what):
    match = re.search(r'^version = "(\d+)\.(\d+)\.(\d+)"$', manifest_text, re.M)
    if not match:
        fail(f"cannot find a `version = \"x.y.z\"` line in {what}")
    return tuple(map(int, match.groups()))


def one_above(base, candidate):
    """Exactly one step up the ladder: next patch, next minor, or next major."""
    major, minor, patch = base
    return candidate in ((major, minor, patch + 1), (major, minor + 1, 0), (major + 1, 0, 0))


def pretty(version):
    return ".".join(map(str, version))


def published_version(module):
    """The version crates.io serves for a module, or None if never published."""
    try:
        crate = json.loads(fetch(f"https://crates.io/api/v1/crates/{module}"))
        return tuple(map(int, crate["crate"]["max_version"].split(".")))
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise


def main():
    manifest = open("Cargo.toml").read()
    pr_version = parse_version(manifest, "the PR's root Cargo.toml")

    pin = re.search(r'packdiff-dto = \{ path = "dto", version = "(\d+\.\d+\.\d+)"', manifest)
    if not pin:
        fail("cannot find the `packdiff-dto` dependency pin in the root Cargo.toml")
    if pin.group(1) != pretty(pr_version):
        fail(f"version literals disagree: workspace {pretty(pr_version)} vs `packdiff-dto` pin {pin.group(1)}")

    base_repo = os.environ.get("BASE_REPO", "dkorolev/packdiff")
    base_ref = os.environ.get("BASE_REF", "main")
    base_manifest = fetch(f"https://raw.githubusercontent.com/{base_repo}/{base_ref}/Cargo.toml")
    base_version = parse_version(base_manifest, f"{base_repo}@{base_ref} Cargo.toml")

    versions = {module: published_version(module) for module in MODULES}
    module_report = ", ".join(f"{m} {pretty(v) if v else 'unpublished'}" for m, v in versions.items())

    if all(v is None for v in versions.values()):
        print("::warning::nothing is on crates.io yet; skipping the published-version checks")
    elif len(set(versions.values())) > 1:
        fail(
            f"crates.io modules disagree ({module_report}) — a publish was left half-done; "
            f"finish it in dependency order (dto, then wasm, then cli) before merging"
        )
    else:
        published = next(iter(versions.values()))
        if published > base_version:
            fail(
                f"crates.io serves {pretty(published)} but {base_repo}@{base_ref} holds only "
                f"{pretty(base_version)} — sync {base_ref} before merging a new version"
            )
        if published < base_version:
            print(
                f"::warning::pending release: crates.io serves {pretty(published)} but "
                f"{base_repo}@{base_ref} already holds {pretty(base_version)} — remember to publish"
            )

    if not one_above(base_version, pr_version):
        fail(
            f"PR version {pretty(pr_version)} must be exactly one above "
            f"{base_repo}@{base_ref} ({pretty(base_version)})"
        )

    published_note = module_report if any(versions.values()) else "nothing published yet"
    print(f"version gate ok: PR {pretty(pr_version)}, {base_repo}@{base_ref} {pretty(base_version)}, {published_note}")


if __name__ == "__main__":
    main()
