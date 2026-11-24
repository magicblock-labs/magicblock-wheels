"""Check a release branch or tag before packaging. Print GitHub Actions step outputs."""

import json
import re
import subprocess
import sys
from pathlib import Path


def release_version(ecosystem, ref):
    number = r"(?:0|[1-9][0-9]*)"
    match = re.fullmatch(
        rf"refs/(?:heads/release/|tags/){ecosystem}-v({number}\.{number}\.{number})"
        rf"(?:[.-](alpha|beta|rc)\.({number}))?",
        ref,
    )
    if not match:
        raise SystemExit(f"Invalid {ecosystem} release ref: {ref}")
    version, channel, sequence = match.groups()
    if channel:
        return f"{version}-{channel}.{sequence}", channel
    return version, "latest"


def check_manifests(ecosystem, version):
    if ecosystem == "npm":
        package = json.loads(Path("ts/package.json").read_text())
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


if __name__ == "__main__":
    ecosystem, ref = sys.argv[1:]
    version, channel = release_version(ecosystem, ref)
    if ref.startswith("refs/tags/"):
        ancestry = subprocess.run(["git", "merge-base", "--is-ancestor", "HEAD", "origin/master"])
        if ancestry.returncode:
            raise SystemExit(f"Cannot publish {ref}: the tagged commit must be on master")
    check_manifests(ecosystem, version)
    print(f"version={version}")
    print(f"dist_tag={channel}")
