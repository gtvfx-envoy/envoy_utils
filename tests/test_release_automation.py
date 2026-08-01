"""Tests for Envoy Utils release automation."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

SCRIPT_PATH = Path(__file__).parents[1] / "scripts" / "release_automation.py"
SPEC = importlib.util.spec_from_file_location("release_automation", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
release_automation = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release_automation)


class ReleaseAutomationTests(unittest.TestCase):
    """Exercise deterministic release metadata rewrites."""

    def testValidateVersion(self):
        """SemVer validation accepts stable and prerelease values."""
        self.assertEqual(release_automation.validateVersion("0.2.0"), "0.2.0")
        self.assertEqual(release_automation.validateVersion("1.0.0-rc.1"), "1.0.0-rc.1")
        with self.assertRaises(ValueError):
            release_automation.validateVersion("v0.2.0")

    def testVersionToTag(self):
        """Unprefixed dependency versions convert to Git tags."""
        self.assertEqual(release_automation.versionToTag("0.6.1"), "v0.6.1")
        with self.assertRaises(ValueError):
            release_automation.versionToTag("v0.6.1")

    def testReplaceEnvoyDependency(self):
        """The Cargo tag and exact version move together."""
        temporary_directory = self.enterContext(tempfile.TemporaryDirectory())
        manifest_path = Path(temporary_directory) / "Cargo.toml"
        manifest_path.write_text(
            "[workspace.dependencies]\n"
            'envoy-core = { git = "https://github.com/gtvfx-envoy/envoy", '
            'tag = "v0.5.1", version = "=0.5.1" }\n',
            encoding="utf-8",
        )
        release_automation.replaceEnvoyDependency(manifest_path, "v0.6.0")
        contents = manifest_path.read_text(encoding="utf-8")
        self.assertIn('tag = "v0.6.0"', contents)
        self.assertIn('version = "=0.6.0"', contents)

    def testReplaceWithLocalEnvoy(self):
        """Candidate testing removes the remote exact-version constraint."""
        temporary_directory = self.enterContext(tempfile.TemporaryDirectory())
        manifest_path = Path(temporary_directory) / "Cargo.toml"
        manifest_path.write_text(
            'envoy-core = { git = "https://example.test", tag = "v0.5.1", '
            'version = "=0.5.1" }\n',
            encoding="utf-8",
        )
        release_automation.replaceWithLocalEnvoy(
            manifest_path, Path(temporary_directory)
        )
        contents = manifest_path.read_text(encoding="utf-8")
        self.assertIn('envoy-core = { path = "', contents)
        self.assertNotIn("tag =", contents)


if __name__ == "__main__":
    unittest.main()
