"""The changelog's dependency list comes from Cargo.lock, not PR titles.

A dependabot branch is titled with the version it was opened for; by the time
it merges the resolution can be a newer patch. `release_changes.py` compares
the lockfile with the last release so `scripts/release.sh` can refuse a
changelog entry that names a version the release does not ship.
"""

import pathlib
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

import release_changes  # noqa: E402


class DirectDependencyTest(unittest.TestCase):
    def test_path_dependencies_are_not_bumps(self):
        workspace = {
            "crate-core": {"version": "0.1.0", "path": "crates/crate-core"},
            "zip": {"version": "9.0", "features": ["deflate"]},
        }
        manifest = {
            "workspace": {"dependencies": workspace},
            "dependencies": {"crate-core": {"workspace": True}, "bytes": "1"},
        }
        self.assertEqual(release_changes.dependencies(manifest, workspace), {"zip", "bytes"})

    def test_renamed_dependency_uses_the_real_package(self):
        manifest = {"dependencies": {"sha": {"package": "sha2", "version": "0.11"}}}
        self.assertEqual(release_changes.dependencies(manifest, {}), {"sha2"})


class VersionPatternTest(unittest.TestCase):
    def test_version_matches_whole_components_only(self):
        pattern = release_changes.version_pattern("9.8.1")
        self.assertIsNotNone(pattern.search("zip 9.8.1,"))
        self.assertIsNone(pattern.search("zip 9.8.10"))
        self.assertIsNone(pattern.search("zip 19.8.1"))


if __name__ == "__main__":
    unittest.main()
