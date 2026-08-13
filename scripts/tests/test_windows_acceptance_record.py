from __future__ import annotations

import hashlib
import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def load_script():
    path = ROOT / "scripts" / "create-windows-acceptance-record.py"
    spec = importlib.util.spec_from_file_location("create_windows_acceptance_record", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class WindowsAcceptanceRecordTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.record = load_script()

    def candidate(self, root: Path) -> tuple[dict[str, object], Path]:
        installer = root / "DeskLinkSetup-0.1.42-x64.exe"
        installer.write_bytes(b"candidate")
        digest = hashlib.sha256(installer.read_bytes()).hexdigest()
        return (
            {
                "schema": 1,
                "passed": True,
                "version": "0.1.42",
                "source_commit": "a" * 40,
                "source_dirty": False,
                "release_scope": self.record.EXPECTED_SCOPE,
                "installer": {
                    "file_name": installer.name,
                    "size_bytes": installer.stat().st_size,
                    "sha256": digest,
                },
            },
            installer,
        )

    def test_accepts_a_complete_candidate_manifest(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest, installer = self.candidate(Path(directory))
            self.assertEqual(
                self.record.validate_candidate_manifest(
                    manifest,
                    version="0.1.42",
                    source_commit="a" * 40,
                    installer_name=installer.name,
                    installer_path=installer,
                ),
                manifest["installer"]["sha256"],
            )

    def test_rejects_scope_or_size_drift(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest, installer = self.candidate(Path(directory))
            manifest["release_scope"] = {
                "target": "windows-10/11-x64",
                "macos_release": True,
                "mobile_release": False,
            }
            with self.assertRaises(ValueError):
                self.record.validate_candidate_manifest(
                    manifest,
                    version="0.1.42",
                    source_commit="a" * 40,
                    installer_name=installer.name,
                    installer_path=installer,
                )
            manifest, installer = self.candidate(Path(directory))
            manifest["installer"]["size_bytes"] += 1
            with self.assertRaises(ValueError):
                self.record.validate_candidate_manifest(
                    manifest,
                    version="0.1.42",
                    source_commit="a" * 40,
                    installer_name=installer.name,
                    installer_path=installer,
                )
