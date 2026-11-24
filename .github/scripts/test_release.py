import unittest

from check_release import release_version


class ReleaseRefTests(unittest.TestCase):
    def test_registry_versions_and_channels(self):
        for ecosystem in ("rust", "npm"):
            for prefix in ("refs/heads/release/", "refs/tags/"):
                for suffix, version, channel in (
                    ("", "0.4.0", "latest"),
                    (".alpha.1", "0.4.0-alpha.1", "alpha"),
                    ("-beta.2", "0.4.0-beta.2", "beta"),
                    (".rc.10", "0.4.0-rc.10", "rc"),
                ):
                    ref = f"{prefix}{ecosystem}-v0.4.0{suffix}"
                    with self.subTest(ref=ref):
                        self.assertEqual(release_version(ecosystem, ref), (version, channel))

    def test_unrelated_and_malformed_refs(self):
        for ref in (
            "refs/tags/npm-v0.4.0",
            "refs/heads/rust-v0.4.0",
            "refs/tags/rust-v0.4",
            "refs/tags/rust-v0.4.0.alpha",
            "refs/tags/rust-v0.4.0.rc.01",
        ):
            with self.subTest(ref=ref), self.assertRaises(SystemExit):
                release_version("rust", ref)
