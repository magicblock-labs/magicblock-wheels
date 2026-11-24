import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from check_release import check_tag, parse_release_ref


class ReleaseRefTests(unittest.TestCase):
    def test_registry_versions_and_channels(self):
        for ecosystem in ("rust", "npm"):
            for prefix in ("refs/heads/release/", "refs/tags/"):
                for suffix, version, channel in (
                    ("", "0.4.0", "latest"),
                    ("-alpha.1", "0.4.0-alpha.1", "alpha"),
                    ("-beta.2", "0.4.0-beta.2", "beta"),
                    ("-rc.10", "0.4.0-rc.10", "rc"),
                ):
                    ref = f"{prefix}{ecosystem}-v0.4.0{suffix}"
                    with self.subTest(ref=ref):
                        self.assertEqual(parse_release_ref(ref), (ecosystem, version, channel))

    def test_unrelated_and_malformed_refs(self):
        for ref in (
            "refs/tags/v0.4.0",
            "refs/tags/python-v0.4.0",
            "refs/tags/npm-v0.4.0/rc.1",
            "refs/tags/rust-v0.4.0.alpha.1",
            "refs/heads/release/npm-v0.4.0.rc.1",
            "refs/heads/release/python-v0.4.0",
            "refs/heads/release",
            "refs/heads/rust-v0.4.0",
            "refs/tags/rust-v0.4",
            "refs/tags/rust-v0.4.0-alpha",
            "refs/tags/rust-v0.4.0-rc.01",
        ):
            with self.subTest(ref=ref), self.assertRaises(SystemExit):
                parse_release_ref(ref)

    def test_only_new_tags_on_master_can_publish(self):
        ref = "refs/tags/npm-v0.4.0-beta.1"
        with self.assertRaisesRegex(SystemExit, "Cannot move"):
            check_tag(ref, {"created": False})
        with patch("check_release.subprocess.run") as git:
            git.return_value.returncode = 1
            with self.assertRaisesRegex(SystemExit, "must be on master"):
                check_tag(ref, {"created": True})
            git.return_value.returncode = 0
            check_tag(ref, {"created": True})

    def test_pr_branch_for_a_version_bump(self):
        checker = Path(__file__).with_name("check_release_pr.py").resolve()
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)

            def git(*args):
                subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True)

            git("init")
            git("config", "user.name", "CI")
            git("config", "user.email", "ci@example.org")
            git("config", "commit.gpgSign", "false")
            manifest = repo / "ts/package.json"
            manifest.parent.mkdir()
            package = {"name": "@magicblock-labs/wheels", "version": "0.3.0", "license": "MIT"}
            manifest.write_text(json.dumps(package))
            git("add", ".")
            git("commit", "-qm", "Initial manifest")
            package["version"] = "0.4.0-rc.1"
            manifest.write_text(json.dumps(package))
            git("commit", "-qam", "Bump npm")

            for branch, base, succeeds in (
                ("feature/layout", "HEAD", True),
                ("releases/npm-v0.4.0-rc.1", "HEAD^", False),
                ("release/npm-v0.4.0", "HEAD^", False),
                ("release/npm-v0.4.0-rc.1", "HEAD^", True),
            ):
                with self.subTest(branch=branch):
                    result = subprocess.run(
                        [sys.executable, "-B", checker, branch, base],
                        cwd=repo, capture_output=True, text=True,
                    )
                    self.assertEqual(result.returncode == 0, succeeds, result.stderr)
