from __future__ import annotations

import hashlib
import importlib.util
import json
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]


def load_script():
    path = ROOT / "scripts" / "package-windows-candidate.py"
    spec = importlib.util.spec_from_file_location("package_windows_candidate", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class WindowsCandidatePackageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.package = load_script()

    def fixture(self, root: Path) -> tuple[Path, str, str]:
        dist = root / "dist" / "windows"
        dist.mkdir(parents=True)
        version = "0.1.91"
        commit = "a" * 40
        installer = dist / f"DeskLinkSetup-{version}-x64.exe"
        installer.write_bytes(b"candidate installer")
        manifest = {
            "schema": 1,
            "version": version,
            "source_commit": commit,
            "source_dirty": False,
            "release_scope": self.package.EXPECTED_SCOPE,
            "passed": True,
            "installer": {
                "file_name": installer.name,
                "size_bytes": installer.stat().st_size,
                "sha256": hashlib.sha256(installer.read_bytes()).hexdigest(),
            },
        }
        (dist / "windows-installer-manifest.json").write_text(
            json.dumps(manifest), encoding="utf-8"
        )
        for name in (
            "windows-release-verification.json",
            "windows-resilience-report.json",
            "windows-acceptance-record.json",
            "windows-release-readiness.json",
        ):
            report = {
                "source_commit": commit,
                "source_dirty": False,
                "version": version,
                "passed": True,
            }
            if name == "windows-acceptance-record.json":
                report["installer"] = {"sha256": manifest["installer"]["sha256"]}
            (dist / name).write_text(json.dumps(report), encoding="utf-8")
        return dist, version, commit

    def test_rejects_stale_source_report(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            dist, _, _ = self.fixture(Path(directory))
            report = json.loads((dist / "windows-resilience-report.json").read_text())
            report["source_commit"] = "b" * 40
            (dist / "windows-resilience-report.json").write_text(json.dumps(report))
            with self.assertRaises(self.package.CandidatePackageError):
                self.package.validate_candidate_artifacts(Path(directory), dist)

    def test_packages_only_currently_bound_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dist, version, commit = self.fixture(root)
            with patch.object(self.package, "git_output", side_effect=[commit, ""]):
                output = root / "candidate.zip"
                result = self.package.package_candidate(root, output)
            self.assertEqual(result, output.resolve())
            with zipfile.ZipFile(result) as archive:
                names = archive.namelist()
            self.assertIn(f"DeskLinkSetup-{version}-x64.exe", names)
            self.assertIn("windows-release-readiness.json", names)
            self.assertEqual(len(names), 5)


if __name__ == "__main__":
    unittest.main()
