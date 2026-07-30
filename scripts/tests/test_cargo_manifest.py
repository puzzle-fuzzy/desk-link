from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from cargo_manifest import package_version


class CargoManifestTests(unittest.TestCase):
    def test_reads_package_version_without_parsing_unrelated_sections(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            manifest = Path(directory) / "Cargo.toml"
            manifest.write_text(
                '[package]\nname = "desklink-test"\nversion = "0.1.91"\n'
                '\n[dependencies]\nversion = "not-a-package-version"\n',
                encoding="utf-8",
            )

            self.assertEqual(package_version(manifest), "0.1.91")


if __name__ == "__main__":
    unittest.main()
