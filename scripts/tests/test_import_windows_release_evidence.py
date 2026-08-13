from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]


def load_script():
    path = ROOT / "scripts" / "import-windows-release-evidence.py"
    spec = importlib.util.spec_from_file_location("import_windows_release_evidence", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class WindowsReleaseEvidenceImportTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.importer = load_script()

    def test_imports_all_evidence_files_from_an_immutable_ref(self) -> None:
        values = {
            "windows-acceptance-record.json": {"checks": {"two_windows_acceptance": False}},
            "windows-resilience-report.json": {"passed": True, "source_dirty": False},
            "managed-relay-verification.json": {"passed": True},
            "managed-diagnostics-audit.json": {"passed": True},
        }

        def read_git_file(_root: Path, _ref: str, path: str) -> bytes:
            return json.dumps(values[Path(path).name]).encode("utf-8")

        with tempfile.TemporaryDirectory() as directory:
            output_root = Path(directory) / "dist"
            with patch.object(self.importer, "read_git_file", side_effect=read_git_file):
                imported = self.importer.import_evidence(
                    Path(directory),
                    ref="a" * 40,
                    tag="v0.1.91",
                    output_root=output_root,
                )

            self.assertEqual(len(imported), 4)
            self.assertEqual(
                json.loads(
                    (output_root / "windows" / "windows-acceptance-record.json").read_text()
                ),
                values["windows-acceptance-record.json"],
            )
            self.assertEqual(
                json.loads(
                    (output_root / "windows" / "windows-resilience-report.json").read_text()
                ),
                values["windows-resilience-report.json"],
            )
            self.assertEqual(
                json.loads(
                    (output_root / "linux" / "managed-diagnostics-audit.json").read_text()
                ),
                values["managed-diagnostics-audit.json"],
            )

    def test_rejects_a_mutable_evidence_ref(self) -> None:
        with self.assertRaisesRegex(ValueError, "immutable"):
            self.importer.import_evidence(
                Path("."),
                ref="release-evidence/v0.1.91",
                tag="v0.1.91",
                output_root=Path("dist"),
            )

    def test_rejects_evidence_for_an_invalid_tag(self) -> None:
        with self.assertRaisesRegex(ValueError, "version tag"):
            self.importer.import_evidence(
                Path("."),
                ref="a" * 40,
                tag="main",
                output_root=Path("dist"),
            )


if __name__ == "__main__":
    unittest.main()
