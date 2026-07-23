from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
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


if __name__ == "__main__":
    unittest.main()
