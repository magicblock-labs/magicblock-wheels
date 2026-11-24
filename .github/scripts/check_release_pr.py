"""Catch version bumps on the wrong PR branch, including typos outside release/."""

import json
import subprocess
import sys
import tomllib
from pathlib import Path

from check_release import check_manifests, parse_release_ref


def package_version(path, contents):
    if path == "Cargo.toml":
        return tomllib.loads(contents)["workspace"].get("package", {}).get("version")
    return json.loads(contents)["version"]


def check_pr(branch, base):
    if branch == "release" or branch.startswith("release/"):
        ecosystem, version, _ = parse_release_ref(f"refs/heads/{branch}")
        check_manifests(ecosystem, version)

    manifests = {"Cargo.toml": "rust", "ts/package.json": "npm"}
    # A new manifest introduces a package. Its first release can be prepared later.
    modified = subprocess.check_output(
        ["git", "diff", "--name-only", "--diff-filter=M", base, "HEAD", "--", *manifests], text=True
    )
    for path in modified.splitlines():
        previous = subprocess.check_output(["git", "show", f"{base}:{path}"], text=True)
        old_version = package_version(path, previous)
        new_version = package_version(path, Path(path).read_text())
        if old_version and old_version != new_version:
            expected = f"release/{manifests[path]}-v{new_version}"
            if branch != expected:
                raise SystemExit(f"{path} changes version to {new_version}. Use branch {expected}")


if __name__ == "__main__":
    check_pr(*sys.argv[1:])
