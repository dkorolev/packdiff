#!/usr/bin/env python3
"""The release version ladder, enforced on every PR (see PUBLISHING.md).

Checks, in order:

1. The PR's two version literals in the root Cargo.toml agree
   (`[workspace.package] version` and the `packdiff-dto` dependency pin).
2. The version published on crates.io equals the version on the base branch
   (what is on `main` is what users can install).
3. The PR's version is exactly one above the published one — next patch for
   a compatible release, or next minor / major for a breaking one (per the
   published-crate policy in PUBLISHING.md).

Base case: while nothing is published yet, step 2 is skipped with a warning
and step 3 gates against the base branch's version instead.

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

    try:
        crate = json.loads(fetch("https://crates.io/api/v1/crates/packdiff"))
        published = tuple(map(int, crate["crate"]["max_version"].split(".")))
    except urllib.error.HTTPError as error:
        if error.code == 404:
            published = None
        else:
            raise

    if published is None:
        print(f"::warning::packdiff is not on crates.io yet; gating against {base_repo}@{base_ref} instead")
        if not one_above(base_version, pr_version):
            fail(
                f"PR version {pretty(pr_version)} must be exactly one above "
                f"{base_repo}@{base_ref} ({pretty(base_version)}) until a first version is published"
            )
    else:
        if published != base_version:
            fail(
                f"crates.io serves {pretty(published)} but {base_repo}@{base_ref} holds "
                f"{pretty(base_version)} — publish main (or sync it) before merging a new version"
            )
        if not one_above(published, pr_version):
            fail(f"PR version {pretty(pr_version)} must be exactly one above the published {pretty(published)}")

    published_note = f"published {pretty(published)}" if published else "nothing published yet"
    print(f"version gate ok: PR {pretty(pr_version)}, {published_note}, {base_repo}@{base_ref} {pretty(base_version)}")


if __name__ == "__main__":
    main()
