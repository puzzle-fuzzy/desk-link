from __future__ import annotations

import contextlib
import io
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import windows_native_build_env as native


class WindowsNativeBuildEnvironmentTests(unittest.TestCase):
    def include_directory(self, root):
        include = root / "C include headers"
        include.mkdir()
        (include / "stddef.h").touch()
        (include / "stdint.h").touch()
        return include

    def test_ready_environment_requires_compiler_and_both_standard_headers(self):
        with tempfile.TemporaryDirectory() as directory:
            include = self.include_directory(Path(directory))
            environment = {"PATH": "synthetic compiler path", "INCLUDE": f'"{include}"'}
            with patch.object(native.shutil, "which", return_value="synthetic-cl"):
                self.assertTrue(native.msvc_environment_is_ready(environment))
                self.assertFalse(native.msvc_environment_is_ready({"PATH": "same", "INCLUDE": ""}))
                (include / "stdint.h").unlink()
                self.assertFalse(native.msvc_environment_is_ready(environment))
            with patch.object(native.shutil, "which", return_value=None):
                self.assertFalse(native.msvc_environment_is_ready(environment))

    def test_existing_working_developer_environment_is_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            include = self.include_directory(Path(directory))
            with patch.dict(os.environ, {"PATH": "existing compiler", "INCLUDE": str(include), "USER_CANARY": "synthetic-preserved"}, clear=True), patch.object(native.shutil, "which", return_value="existing-cl"), patch.object(native, "visual_studio_installations") as locate, patch.object(native.subprocess, "run") as run:
                before = dict(os.environ)
                native.prepare_msvc_environment()
                self.assertEqual(dict(os.environ), before)
            locate.assert_not_called()
            run.assert_not_called()

    def test_utf16_capture_handles_spaces_unicode_and_equals_without_logging_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            batch = Path(directory) / "Visual Studio with spaces" / "VsDevCmd.bat"
            content = "Path=C:\\compiler tools\\bin\r\nINCLUDE=C:\\headers 测试\r\nSYNTHETIC_SECRET=value=with=equals\r\n=C:=C:\\ignored\r\n"
            result = Mock(returncode=0, stdout=content.encode("utf-16-le"), stderr=b"synthetic-secret-error")
            captured = io.StringIO()
            with patch.object(native.subprocess, "run", return_value=result) as run, contextlib.redirect_stdout(captured), contextlib.redirect_stderr(captured):
                environment = native.capture_msvc_environment(batch)
            self.assertEqual(environment["PATH"], "C:\\compiler tools\\bin")
            self.assertEqual(environment["INCLUDE"], "C:\\headers 测试")
            self.assertEqual(environment["SYNTHETIC_SECRET"], "value=with=equals")
            self.assertNotIn("", environment)
            self.assertEqual(captured.getvalue(), "")
            command = run.call_args.args[0]
            self.assertIsInstance(command, str)
            self.assertIn(' /d /u /s /c "call "', command)
            self.assertIn(f'call "{batch.resolve()}"', command)
            self.assertTrue(command.endswith('>nul && set"'))
            self.assertIn("-arch=x64 -host_arch=x64", command)
            self.assertTrue(run.call_args.kwargs["executable"].endswith(str(Path("System32/cmd.exe"))))
            self.assertNotIn("shell", run.call_args.kwargs)

    def test_batch_failure_does_not_expose_captured_environment_or_stderr(self):
        result = Mock(returncode=9, stdout=b"SYNTHETIC_SECRET=do-not-print", stderr=b"SYNTHETIC_SECRET=do-not-print")
        with patch.object(native.subprocess, "run", return_value=result):
            with self.assertRaises(SystemExit) as raised:
                native.capture_msvc_environment(Path("VsDevCmd.bat"))
        self.assertIn("exit 9", str(raised.exception))
        self.assertNotIn("SYNTHETIC_SECRET", str(raised.exception))

    def test_bad_utf16_and_process_timeout_have_safe_diagnostics(self):
        for result in [Mock(returncode=0, stdout=b"odd", stderr=b"private-output"), subprocess.TimeoutExpired("private-command", 60)]:
            with self.subTest(result=type(result).__name__):
                run = patch.object(native.subprocess, "run", side_effect=result) if isinstance(result, Exception) else patch.object(native.subprocess, "run", return_value=result)
                with run, self.assertRaises(SystemExit) as raised:
                    native.capture_msvc_environment(Path("VsDevCmd.bat"))
                self.assertEqual(str(raised.exception), "Could not capture the Visual Studio developer environment.")

    def test_batch_path_with_expansion_or_shell_metacharacters_is_rejected(self):
        for segment in ("VS%PATH%", "VS!EXPAND!", "VS&command", 'VS"quote'):
            with self.subTest(segment=segment), patch.object(native.subprocess, "run") as run:
                with self.assertRaises(SystemExit):
                    native.capture_msvc_environment(Path(segment) / "VsDevCmd.bat")
                run.assert_not_called()

    def test_vcvarsall_uses_its_x64_argument(self):
        with patch.object(native.subprocess, "run", return_value=Mock(returncode=0, stdout="PATH=synthetic\r\n".encode("utf-16-le"))) as run:
            native.capture_msvc_environment(Path("VS tools") / "vcvarsall.bat")
        self.assertIn('vcvarsall.bat" x64 >nul && set', run.call_args.args[0])

    @unittest.skipUnless(os.name == "nt", "real cmd quoting and UTF-16 output")
    def test_real_cmd_captures_a_synthetic_batch_at_a_path_with_spaces(self):
        with tempfile.TemporaryDirectory() as directory:
            batch = Path(directory) / "VS with spaces" / "VsDevCmd.bat"
            batch.parent.mkdir()
            batch.write_text('@echo off\nset "DESKLINK_SYNTHETIC_NATIVE_ENV=quoted path value"\nexit /b 0\n', encoding="ascii")
            environment = native.capture_msvc_environment(batch)
            self.assertEqual(environment["DESKLINK_SYNTHETIC_NATIVE_ENV"], "quoted path value")

    def test_setup_validates_before_merging_and_keeps_unrelated_variables(self):
        with tempfile.TemporaryDirectory() as directory:
            installation = Path(directory) / "VS with spaces"
            batch = installation / "Common7/Tools/VsDevCmd.bat"
            batch.parent.mkdir(parents=True)
            batch.touch()
            loaded = {"PATH": "developer tools", "INCLUDE": "developer include", "LIB": "developer libraries"}
            with patch.dict(os.environ, {"PATH": "original", "UNRELATED": "synthetic-preserved"}, clear=True), patch.object(native, "msvc_environment_is_ready", side_effect=[False, True]), patch.object(native, "visual_studio_installations", return_value=[installation]), patch.object(native, "capture_msvc_environment", return_value=loaded) as capture:
                native.prepare_msvc_environment()
                self.assertEqual(os.environ["PATH"], "developer tools")
                self.assertEqual(os.environ["LIB"], "developer libraries")
                self.assertEqual(os.environ["UNRELATED"], "synthetic-preserved")
            capture.assert_called_once_with(batch)

    def test_incomplete_setup_does_not_mutate_callers_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            installation = Path(directory)
            batch = installation / "Common7/Tools/VsDevCmd.bat"
            batch.parent.mkdir(parents=True)
            batch.touch()
            with patch.dict(os.environ, {"PATH": "original", "UNRELATED": "synthetic-preserved"}, clear=True), patch.object(native, "msvc_environment_is_ready", return_value=False), patch.object(native, "visual_studio_installations", return_value=[installation]), patch.object(native, "capture_msvc_environment", return_value={"PATH": "incomplete"}):
                before = dict(os.environ)
                with self.assertRaises(SystemExit):
                    native.prepare_msvc_environment()
                self.assertEqual(dict(os.environ), before)

    def test_vcvarsall_is_used_when_dev_cmd_is_absent(self):
        with tempfile.TemporaryDirectory() as directory:
            installation = Path(directory)
            batch = installation / "VC/Auxiliary/Build/vcvarsall.bat"
            batch.parent.mkdir(parents=True)
            batch.touch()
            with patch.object(native, "msvc_environment_is_ready", side_effect=[False, True]), patch.object(native, "visual_studio_installations", return_value=[installation]), patch.object(native, "capture_msvc_environment", return_value={}) as capture:
                native.prepare_msvc_environment()
            capture.assert_called_once_with(batch)

    def test_vswhere_can_find_a_custom_installation_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tool = root / "Microsoft Visual Studio/Installer/vswhere.exe"
            tool.parent.mkdir(parents=True)
            tool.touch()
            custom = root / "Custom installation with spaces"
            custom.mkdir()
            result = Mock(returncode=0, stdout=(str(custom) + "\r\n").encode("utf-8"))
            with patch.dict(os.environ, {"ProgramFiles": str(root), "ProgramFiles(x86)": str(root)}, clear=True), patch.object(native.subprocess, "run", return_value=result) as run:
                self.assertIn(custom.resolve(), native.visual_studio_installations())
            self.assertIn("-utf8", run.call_args.args[0])
            self.assertIn("Microsoft.VisualStudio.Component.VC.Tools.x86.x64", run.call_args.args[0])

    def test_standard_installations_are_available_without_vswhere(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            edition = root / "Microsoft Visual Studio/2022/Community"
            edition.mkdir(parents=True)
            with patch.dict(os.environ, {"ProgramFiles": str(root), "ProgramFiles(x86)": str(root)}, clear=True):
                self.assertIn(edition.resolve(), native.visual_studio_installations())

    def test_non_windows_setup_is_a_no_op(self):
        with patch.object(native.os, "name", "posix"), patch.object(native, "prepare_msvc_environment") as prepare, patch.object(native.shutil, "which") as which:
            native.prepare_windows_native_build_environment()
        prepare.assert_not_called()
        which.assert_not_called()

    @unittest.skipUnless(os.name == "nt", "Windows entry-point path handling")
    def test_existing_ninja_does_not_skip_msvc_setup(self):
        with patch.object(native, "prepare_msvc_environment") as prepare, patch.object(native.shutil, "which", return_value="ninja.exe"):
            native.prepare_windows_native_build_environment()
        prepare.assert_called_once_with()

    @unittest.skipUnless(os.name == "nt", "Windows entry-point path handling")
    def test_bundled_ninja_search_is_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ninja = root / "Microsoft Visual Studio/2022/Community/Common7/IDE/CommonExtensions/Microsoft/CMake/Ninja/ninja.exe"
            ninja.parent.mkdir(parents=True)
            ninja.touch()
            with patch.dict(os.environ, {"ProgramFiles": str(root), "ProgramFiles(x86)": str(root), "PATH": "existing compiler"}, clear=True), patch.object(native, "prepare_msvc_environment"), patch.object(native.shutil, "which", return_value=None):
                native.prepare_windows_native_build_environment()
                self.assertEqual(os.environ["PATH"].split(os.pathsep)[0], str(ninja.parent))
                self.assertTrue(os.environ["PATH"].endswith("existing compiler"))

    @unittest.skipUnless(os.name == "nt", "Windows CRT policy")
    def test_release_crt_policy_is_preserved(self):
        with patch.dict(os.environ, {"RUSTFLAGS": "-C opt-level=2"}, clear=True):
            native.prepare_windows_release_environment()
            native.prepare_windows_release_environment()
            self.assertEqual(os.environ["RUSTFLAGS"], "-C opt-level=2 -C target-feature=+crt-static")
        with patch.dict(os.environ, {"RUSTFLAGS": "-C target-feature=-crt-static"}, clear=True):
            with self.assertRaises(SystemExit):
                native.prepare_windows_release_environment()


if __name__ == "__main__":
    unittest.main()
