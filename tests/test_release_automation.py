"""Tests for Envoy Utils release automation."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from unittest import mock

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

    def testValidateEnvoyReleaseVersion(self):
        """A release tag accepts matching Cargo workspace metadata."""
        with mock.patch.object(
            release_automation,
            "readEnvoyReleaseVersion",
            return_value="0.6.1",
        ) as read_version:
            release_automation.validateEnvoyReleaseVersion("v0.6.1")
        read_version.assert_called_once_with("v0.6.1")

    def testValidateEnvoyReleaseVersionRejectsMismatch(self):
        """A release tag rejects stale Cargo workspace metadata."""
        with mock.patch.object(
            release_automation,
            "readEnvoyReleaseVersion",
            return_value="0.6.0",
        ):
            with self.assertRaisesRegex(
                RuntimeError,
                "Envoy release v0.6.1 is inconsistent.*version 0.6.0.*expected 0.6.1",
            ):
                release_automation.validateEnvoyReleaseVersion("v0.6.1")

    def testReadEnvoyReleaseVersion(self):
        """The Envoy manifest reader returns workspace package metadata."""
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = (
            b'[workspace.package]\nversion = "0.6.1"\n'
        )
        with mock.patch.object(
            release_automation.urllib.request,
            "urlopen",
            return_value=response,
        ) as urlopen:
            workspace_version = release_automation.readEnvoyReleaseVersion("v0.6.1")
        request = urlopen.call_args.args[0]
        self.assertEqual(workspace_version, "0.6.1")
        self.assertEqual(
            request.full_url,
            "https://raw.githubusercontent.com/gtvfx-envoy/envoy/"
            "v0.6.1/rust/Cargo.toml",
        )
        self.assertEqual(urlopen.call_args.kwargs["timeout"], 30)

    def testReadEnvoyReleaseVersionReportsFetchFailure(self):
        """Manifest download failures include the selected release tag."""
        with mock.patch.object(
            release_automation.urllib.request,
            "urlopen",
            side_effect=release_automation.urllib.error.URLError("offline"),
        ):
            with self.assertRaisesRegex(
                RuntimeError, "Unable to read the Envoy manifest for v0.6.1"
            ):
                release_automation.readEnvoyReleaseVersion("v0.6.1")

    def testPrepareReleaseValidatesEnvoyBeforeEditing(self):
        """Release preparation runs the Envoy preflight before local mutations."""
        temporary_directory = self.enterContext(tempfile.TemporaryDirectory())
        repository_root = Path(temporary_directory)
        rust_root = repository_root / "rust"
        rust_root.mkdir()
        manifest_path = rust_root / "Cargo.toml"
        original_contents = (
            "[workspace.package]\n"
            'version = "0.2.0"\n\n'
            "[workspace.dependencies]\n"
            'envoy-core = { git = "https://github.com/gtvfx-envoy/envoy", '
            'tag = "v0.6.0", version = "=0.6.0" }\n'
        )
        manifest_path.write_text(original_contents, encoding="utf-8")
        preflight_error = RuntimeError("inconsistent release")
        with (
            mock.patch.object(
                release_automation,
                "validateEnvoyReleaseVersion",
                side_effect=preflight_error,
            ),
            mock.patch.object(release_automation.subprocess, "run") as run_command,
        ):
            with self.assertRaisesRegex(RuntimeError, "inconsistent release"):
                release_automation.prepareRelease(repository_root, "0.2.1", "0.6.1")
        self.assertEqual(manifest_path.read_text(encoding="utf-8"), original_contents)
        run_command.assert_not_called()

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
