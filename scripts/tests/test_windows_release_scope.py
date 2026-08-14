from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from unittest.mock import patch
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def load_script():
    path = ROOT / "scripts" / "verify-windows-release.py"
    spec = importlib.util.spec_from_file_location("verify_windows_release", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class WindowsReleaseScopeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.verify = load_script()

    def test_current_readme_is_windows_only(self) -> None:
        scope = self.verify.verify_release_scope()
        self.assertEqual(scope["target"], "windows-10/11-x64")
        self.assertFalse(scope["macos_release"])
        self.assertFalse(scope["mobile_release"])

    def test_windows_release_uses_the_pinned_bun_version(self) -> None:
        self.assertEqual(self.verify.expected_bun_version(), "1.3.14")
        completed = self.verify.subprocess.CompletedProcess(
            ["bun", "--version"], 0, stdout="1.3.14\n", stderr=""
        )
        with patch.object(self.verify.subprocess, "run", return_value=completed):
            self.assertEqual(self.verify.verify_bun_version(), "1.3.14")

    def test_windows_release_rejects_a_different_bun_version(self) -> None:
        completed = self.verify.subprocess.CompletedProcess(
            ["bun", "--version"], 0, stdout="1.4.0\n", stderr=""
        )
        with patch.object(self.verify.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(SystemExit, "Bun 1.3.14 is required"):
                self.verify.verify_bun_version()

    def test_rejects_old_cross_platform_release_claims(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            readme = Path(directory) / "README.md"
            readme.write_text(
                "Windows 10/11 x64\n"
                "当前正式发布目标是 Windows 10/11 x64\n"
                "跨平台研究代码\n"
                "## macOS 构建与使用\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(SystemExit, "non-Windows"):
                self.verify.verify_release_scope(readme)

    def test_production_rust_has_no_unbounded_channels(self) -> None:
        result = self.verify.verify_production_backpressure(
            (ROOT / "apps" / "windows" / "src", ROOT / "apps" / "windows-ui" / "src-tauri" / "src")
        )
        self.assertEqual(result["unbounded_channels"], 0)

    def test_rejects_an_unbounded_production_channel(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "runtime.rs"
            source.write_text(
                "let (_sender, _receiver) = std::sync::mpsc::channel();\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(SystemExit, "unbounded channel"):
                self.verify.verify_production_backpressure((Path(directory),))

    def test_rejects_an_aliased_unbounded_mpsc_channel(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "tray.rs"
            source.write_text(
                "use std::sync::mpsc;\nlet (_sender, _receiver) = mpsc::channel();\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(SystemExit, "unbounded channel"):
                self.verify.verify_production_backpressure((Path(directory),))


if __name__ == "__main__":
    unittest.main()
