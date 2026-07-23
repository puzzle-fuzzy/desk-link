from __future__ import annotations

import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def load_script():
    path = ROOT / "scripts" / "check-windows-release-ready.py"
    spec = importlib.util.spec_from_file_location("check_windows_release_ready", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class WindowsReleaseReadyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.ready = load_script()

    def create_fixture(self, root: Path, *, signed: bool = True) -> dict[str, object]:
        installer = root / "DeskLinkSetup-0.1.42-x64.exe"
        installer.write_bytes(b"desklink installer")
        commit = "a" * 40
        now = 1_000_000
        manifest = {
            "schema": 1,
            "version": "0.1.42",
            "source_commit": commit,
            "source_dirty": False,
            "release_scope": self.ready.EXPECTED_SCOPE,
            "signed": signed,
            "passed": True,
            "installer": {
                "file_name": installer.name,
                "size_bytes": installer.stat().st_size,
                "sha256": hashlib.sha256(installer.read_bytes()).hexdigest(),
            },
        }
        verification = {
            "version": "0.1.42",
            "source_commit": commit,
            "source_dirty": False,
            "custom_protocol": True,
            "release_scope": self.ready.EXPECTED_SCOPE,
            "passed": True,
        }
        relay = {"passed": True, "completed_at_unix_s": now}
        diagnostics = {"passed": True, "completed_at_unix_s": now}
        return {
            "version": "0.1.42",
            "head": commit,
            "dirty": False,
            "verification": verification,
            "manifest": manifest,
            "installer_path": installer,
            "tag_exists": True,
            "relay_report": relay,
            "diagnostics_report": diagnostics,
            "now": now,
            "manual_checks": {key: True for key in self.ready.MANUAL_CHECK_IDS},
        }

    def test_complete_fixture_is_ready(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            report = self.ready.evaluate_preflight(**self.create_fixture(Path(directory)))
        self.assertTrue(report["ready"])
        self.assertEqual(report["blockers"], [])

    def test_unsigned_candidate_reports_explicit_release_and_manual_blockers(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = self.create_fixture(Path(directory), signed=False)
            fixture["tag_exists"] = False
            fixture["manual_checks"] = {}
            report = self.ready.evaluate_preflight(**fixture)
        blocker_ids = {item["id"] for item in report["blockers"]}
        self.assertFalse(report["ready"])
        self.assertIn("authenticode_signature", blocker_ids)
        self.assertIn("release_tag", blocker_ids)
        self.assertIn("two_windows_acceptance", blocker_ids)

    def test_installer_hash_drift_blocks_readiness(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = self.create_fixture(Path(directory))
            manifest = dict(fixture["manifest"])
            installer = dict(manifest["installer"])
            installer["sha256"] = "b" * 64
            manifest["installer"] = installer
            fixture["manifest"] = manifest
            report = self.ready.evaluate_preflight(**fixture)
        self.assertIn("installer_integrity", {item["id"] for item in report["blockers"]})

    def test_manual_acceptance_record_is_bound_to_candidate_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = self.create_fixture(root)
            manifest = fixture["manifest"]
            installer = manifest["installer"]
            record = {
                "schema": 1,
                "version": "0.1.42",
                "source_commit": "a" * 40,
                "installer": {
                    "file_name": installer["file_name"],
                    "sha256": installer["sha256"],
                },
                "operator": "release-team",
                "recorded_at_utc": "2026-07-23T10:00:00Z",
                "checks": {key: True for key in self.ready.MANUAL_CHECK_IDS},
            }
            path = root / "acceptance.json"
            path.write_text(json.dumps(record), encoding="utf-8")
            checks, metadata = self.ready.load_manual_acceptance(
                path,
                expected_version="0.1.42",
                expected_commit="a" * 40,
                expected_installer_sha256=installer["sha256"],
            )
        self.assertTrue(all(checks.values()))
        self.assertEqual(metadata["operator"], "release-team")

    def test_manual_acceptance_rejects_a_different_installer_hash(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            fixture = self.create_fixture(root)
            manifest = fixture["manifest"]
            record = {
                "schema": 1,
                "version": "0.1.42",
                "source_commit": "a" * 40,
                "installer": {"file_name": "DeskLinkSetup-0.1.42-x64.exe", "sha256": "b" * 64},
                "operator": "release-team",
                "recorded_at_utc": "2026-07-23T10:00:00Z",
                "checks": {key: True for key in self.ready.MANUAL_CHECK_IDS},
            }
            path = root / "acceptance.json"
            path.write_text(json.dumps(record), encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "installer"):
                self.ready.load_manual_acceptance(
                    path,
                    expected_version="0.1.42",
                    expected_commit="a" * 40,
                    expected_installer_sha256=manifest["installer"]["sha256"],
                )


if __name__ == "__main__":
    unittest.main()
