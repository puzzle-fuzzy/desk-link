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
    path = ROOT / "scripts" / "publish-windows-release.py"
    spec = importlib.util.spec_from_file_location("publish_windows_release", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class WindowsReleasePublishTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.publish = load_script()

    def create_release(self, root: Path, *, signed: bool = True) -> Path:
        release = root / "dist" / "windows"
        release.mkdir(parents=True)
        installer = release / "DeskLinkSetup-0.1.42-x64.exe"
        installer.write_bytes(b"signed desklink installer")
        digest = hashlib.sha256(installer.read_bytes()).hexdigest()
        (release / "windows-installer-manifest.json").write_text(
            json.dumps(
                {
                    "schema": 1,
                    "version": "0.1.42",
                    "signed": signed,
                    "passed": True,
                    "installer": {
                        "file_name": installer.name,
                        "size_bytes": installer.stat().st_size,
                        "sha256": digest,
                    },
                }
            ),
            encoding="utf-8",
        )
        (release / "windows-release-verification.json").write_text(
            json.dumps({"version": "0.1.42", "passed": True}),
            encoding="utf-8",
        )
        return installer

    def test_accepts_only_a_matching_signed_verified_installer(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installer = self.create_release(root)
            payload = self.publish.validate_release_payload(root, "v0.1.42")
            self.assertEqual(payload.installer, installer)
            self.assertEqual(payload.version, "0.1.42")
            self.assertIn(payload.sha256, self.publish.release_notes(payload))

    def test_rejects_unsigned_or_mismatched_tags(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.create_release(root, signed=False)
            with self.assertRaisesRegex(ValueError, "Unsigned"):
                self.publish.validate_release_payload(root, "v0.1.42")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.create_release(root)
            with self.assertRaisesRegex(ValueError, "does not match"):
                self.publish.validate_release_payload(root, "v0.1.41")

    def test_rejects_an_installer_changed_after_manifest_generation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            installer = self.create_release(root)
            installer.write_bytes(b"tampered")
            with self.assertRaisesRegex(ValueError, "size differs"):
                self.publish.validate_release_payload(root, "v0.1.42")

    def test_rejects_invalid_repository_names(self) -> None:
        payload = self.publish.ReleasePayload(
            version="0.1.42",
            installer=Path("installer.exe"),
            manifest=Path("manifest.json"),
            verification=Path("verification.json"),
            sha256="a" * 64,
        )
        with self.assertRaises(ValueError):
            self.publish.publish(payload, repository="owner/repo;rm", tag="v0.1.42")


if __name__ == "__main__":
    unittest.main()
