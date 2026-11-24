"""Check a release branch or tag before packaging. Print GitHub Actions step outputs."""

import json
import os
import re
import subprocess
import sys
from pathlib import Path


def parse_release_ref(ref):
    number = r"(?:0|[1-9][0-9]*)"
    match = re.fullmatch(
        rf"refs/(?:heads/release/|tags/)(rust|npm)-v"
        rf"({number}\.{number}\.{number}(?:-(alpha|beta|rc)\.{number})?)",
        ref,
    )
    if not match:
        raise SystemExit(
            f"Invalid release ref {ref!r}. Expected a branch like release/rust-v1.2.3 "
            "or a tag like npm-v1.2.3-rc.1"
        )
    ecosystem, version, channel = match.groups()
    return ecosystem, version, channel or "latest"


def check_manifests(ecosystem, version):
    if ecosystem == "npm":
        package = json.loads(Path("ts/package.json").read_text())
        if package["name"] != "@magicblock-labs/wheels":
            raise SystemExit("An npm release must publish @magicblock-labs/wheels")
        packages = [package]
    else:
        metadata = subprocess.check_output(
            ["cargo", "metadata", "--no-deps", "--locked", "--format-version", "1"]
        )
        packages = json.loads(metadata)["packages"]
        wheels = next(p for p in packages if p["name"] == "wheels")
        macros = next(dep for dep in wheels["dependencies"] if dep["name"] == "wheels-macros")
        if macros["req"] != f"={version}":
            raise SystemExit(f"wheels must depend on wheels-macros at exactly ={version}")

    for package in packages:
        if package["version"] != version:
            raise SystemExit(f"{package['name']} has version {package['version']}, expected {version}")
        if package.get("license") in (None, "", "UNLICENSED") and not package.get("license_file"):
            raise SystemExit(f"Set the approved license for {package['name']} in the release PR")


def check_tag(ref, event):
    if not event["created"]:
        raise SystemExit(f"Cannot move {ref}: rerun the original workflow to retry a publish")
    ancestry = subprocess.run(["git", "merge-base", "--is-ancestor", "HEAD", "origin/master"])
    if ancestry.returncode:
        raise SystemExit(f"Cannot publish {ref}: the tagged commit must be on master")


if __name__ == "__main__":
    publisher, ref = sys.argv[1:]
    ecosystem, version, channel = parse_release_ref(ref)
    if ecosystem != publisher:
        raise SystemExit(f"{ref} belongs to the {ecosystem} publisher")
    if ref.startswith("refs/tags/"):
        check_tag(ref, json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text()))
    check_manifests(ecosystem, version)
    print(f"version={version}")
    print(f"dist_tag={channel}")
