from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "verify-windows-release.py"


def load_script():
    spec = importlib.util.spec_from_file_location("current_source_versions_release", SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class CurrentSourceVersionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.verify = load_script()

    def expected_repository_versions(self):
        package = json.loads((ROOT / "apps/windows-ui/package.json").read_text(encoding="utf-8"))
        protocol = re.search(
            r"pub const PROTOCOL_VERSION: u16 = (\d+);",
            (ROOT / "crates/desklink-protocol/src/lib.rs").read_text(encoding="utf-8"),
        )
        toolchain = re.search(
            r'(?m)^channel\s*=\s*"([^"]+)"',
            (ROOT / "rust-toolchain.toml").read_text(encoding="utf-8"),
        )
        self.assertIsNotNone(protocol)
        self.assertIsNotNone(toolchain)
        return {
            "windows_version": package["version"],
            "protocol_version": int(protocol.group(1)),
            "bun_version": package["packageManager"].removeprefix("bun@"),
            "rust_toolchain": toolchain.group(1),
        }

    def make_source_fixture(self, root):
        windows_ui = root / "apps/windows-ui"
        sources = {
            "apps/windows/Cargo.toml": '[package]\nname = "runtime"\nversion = "7.8.9"\n',
            "apps/windows-ui/src-tauri/Cargo.toml": '[package]\nname = "ui"\nversion = "7.8.9"\n',
            "tools/windows-installer/Cargo.toml": '[package]\nname = "installer"\nversion = "7.8.9"\n',
            "apps/windows-ui/package.json": json.dumps({"version": "7.8.9", "packageManager": "bun@1.3.14"}),
            "apps/windows-ui/src-tauri/tauri.conf.json": json.dumps({"version": "7.8.9"}),
            "crates/desklink-protocol/src/lib.rs": "pub const PROTOCOL_VERSION: u16 = 321;\n",
            "rust-toolchain.toml": '[toolchain]\nchannel = "1.97.1"\n',
        }
        for relative, text in sources.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return windows_ui

    def test_real_versions_only_cli_prints_manifest_json_without_external_tools(self):
        expected = self.expected_repository_versions()
        with tempfile.TemporaryDirectory() as directory:
            completed = subprocess.run(
                [sys.executable, str(SCRIPT), "--versions-only"],
                cwd=directory,
                env={**os.environ, "PATH": directory},
                capture_output=True,
                text=True,
                encoding="utf-8",
                check=False,
                timeout=30,
            )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertEqual(completed.stderr, "")
        self.assertEqual(json.loads(completed.stdout), expected)

    def test_versions_only_bypasses_windows_guard_native_preparation_and_build(self):
        expected = self.expected_repository_versions()
        output = io.StringIO()
        forbidden = AssertionError("versions-only must only read source manifests")
        with (
            patch.object(sys, "argv", [str(SCRIPT), "--versions-only"]),
            patch.object(self.verify, "os", SimpleNamespace(name="posix")),
            patch.object(self.verify, "prepare_windows_native_build_environment", side_effect=forbidden),
            patch.object(self.verify, "prepare_windows_release_environment", side_effect=forbidden),
            patch.object(self.verify, "run", side_effect=forbidden),
            patch.object(self.verify.subprocess, "run", side_effect=forbidden),
            contextlib.redirect_stdout(output),
        ):
            self.assertEqual(self.verify.main(), 0)
        self.assertEqual(json.loads(output.getvalue()), expected)

    def test_source_reader_follows_changed_manifests_and_protocol_constant(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            windows_ui = self.make_source_fixture(root)
            with patch.object(self.verify, "ROOT", root), patch.object(self.verify, "WINDOWS_UI", windows_ui):
                self.assertEqual(self.verify.current_source_versions(), {
                    "windows_version": "7.8.9",
                    "protocol_version": 321,
                    "bun_version": "1.3.14",
                    "rust_toolchain": "1.97.1",
                })
                (root / "crates/desklink-protocol/src/lib.rs").write_text(
                    "pub const PROTOCOL_VERSION: u16 = 654;\n", encoding="utf-8"
                )
                self.assertEqual(self.verify.current_source_versions()["protocol_version"], 654)

    def test_any_windows_manifest_version_drift_is_rejected(self):
        paths = (
            "apps/windows/Cargo.toml",
            "apps/windows-ui/src-tauri/Cargo.toml",
            "tools/windows-installer/Cargo.toml",
            "apps/windows-ui/package.json",
            "apps/windows-ui/src-tauri/tauri.conf.json",
        )
        for relative in paths:
            with self.subTest(manifest=relative), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                windows_ui = self.make_source_fixture(root)
                path = root / relative
                path.write_text(path.read_text(encoding="utf-8").replace("7.8.9", "7.8.10"), encoding="utf-8")
                with patch.object(self.verify, "ROOT", root), patch.object(self.verify, "WINDOWS_UI", windows_ui):
                    with self.assertRaisesRegex(SystemExit, "Windows release versions do not match"):
                        self.verify.current_source_versions()

    def test_current_documentation_version_table_and_public_test_copy_match_manifests(self):
        expected = self.expected_repository_versions()
        capabilities = (ROOT / "docs/current-capabilities.md").read_text(encoding="utf-8")
        labels = {
            "Windows 应用": expected["windows_version"],
            "业务协议": expected["protocol_version"],
            "Bun": expected["bun_version"],
            "Rust toolchain": expected["rust_toolchain"],
        }
        for label, version in labels.items():
            with self.subTest(label=label):
                self.assertRegex(capabilities, rf"(?m)^\|\s*{re.escape(label)}\s*\|\s*`{re.escape(str(version))}`\s*\|")
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        current = re.findall(r"当前 `([^`]+)` 是公开测试构建", readme)
        self.assertEqual(current, [expected["windows_version"]])


if __name__ == "__main__":
    unittest.main()
