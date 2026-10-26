from __future__ import annotations

import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch


ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
import verify


class VerificationTests(unittest.TestCase):
    def run_plan(self, root, tasks, *, errors=None, timeout=5, environment=None):
        path = root / "evidence" / "result.json"
        report = {"source_commit": "a" * 40, "source_dirty": True}
        with contextlib.redirect_stdout(io.StringIO()):
            code = verify.run_tasks(
                tasks,
                root=root,
                bun="missing-bun-test-executable",
                report_path=path,
                report=report,
                environment=environment or os.environ.copy(),
                allow_device=False,
                timeout=timeout,
                errors=errors or {},
            )
        return code, json.loads(path.read_text(encoding="utf-8"))

    def test_real_child_success_preserves_logs_metadata_cwd_and_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "child").mkdir()
            task = verify.Task(
                "success", "fast",
                ("python", "-c", "import os,sys; print(os.getcwd()); print(os.environ['DESKLINK_VERIFY_CANARY']); print('stderr-canary', file=sys.stderr)"),
                "child",
            )
            environment = {**os.environ, "DESKLINK_VERIFY_CANARY": "synthetic-value"}
            code, report = self.run_plan(root, [task], environment=environment)
            self.assertEqual(code, 0)
            self.assertTrue(report["passed"])
            self.assertEqual(report["status"], "passed")
            self.assertEqual(report["source_commit"], "a" * 40)
            self.assertTrue(report["source_dirty"])
            record = report["checks"][0]
            self.assertEqual(record["command"][0], sys.executable)
            self.assertEqual(record["exit_code"], 0)
            self.assertGreaterEqual(record["elapsed_seconds"], 0)
            log = Path(record["log"]).read_text(encoding="utf-8")
            self.assertIn(str(root / "child"), log)
            self.assertIn("synthetic-value", log)
            self.assertIn("stderr-canary", log)

    def test_failure_blocks_dependents_but_runs_independent_tasks(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tasks = [
                verify.Task("fail", "fast", ("python", "-c", "import sys; print('failure-canary'); sys.exit(7)")),
                verify.Task("blocked", "fast", ("python", "-c", "from pathlib import Path; Path('must-not-exist').touch()"), requires=("fail",)),
                verify.Task("independent", "fast", ("python", "-c", "print('continued')")),
            ]
            code, report = self.run_plan(root, tasks)
            self.assertEqual(code, 1)
            self.assertFalse(report["passed"])
            self.assertEqual(report["status"], "failed")
            self.assertEqual([item["status"] for item in report["checks"]], ["failed", "blocked", "passed"])
            self.assertEqual(report["checks"][0]["exit_code"], 7)
            self.assertIn("failure-canary", Path(report["checks"][0]["log"]).read_text())
            self.assertIn("fail", report["checks"][1]["reason"])
            self.assertFalse((root / "must-not-exist").exists())

    def test_missing_platform_and_device_are_incomplete_not_passed(self):
        with tempfile.TemporaryDirectory() as directory:
            tasks = [
                verify.Task("foreign", "native", ("not-an-executable",), host="unavailable-test-platform"),
                verify.Task("device", "native", ("not-an-executable",), device=True),
                verify.Task("needs-device", "native", ("not-an-executable",), requires=("device",)),
            ]
            code, report = self.run_plan(Path(directory), tasks)
            self.assertEqual(code, 2)
            self.assertEqual(report["status"], "incomplete")
            self.assertFalse(report["passed"])
            self.assertEqual([item["status"] for item in report["checks"]], ["skipped", "skipped", "blocked"])
            self.assertIn("current host", report["checks"][0]["reason"])
            self.assertIn("--allow-device", report["checks"][1]["reason"])

    def test_timeout_is_recorded_as_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            task = verify.Task("timeout", "fast", ("python", "-c", "import time; time.sleep(10)"))
            code, report = self.run_plan(Path(directory), [task], timeout=0.1)
            self.assertEqual(code, 1)
            self.assertFalse(report["passed"])
            self.assertEqual(report["checks"][0]["status"], "failed")
            self.assertIn("timed out", report["checks"][0]["reason"])

    def test_timeout_stops_delayed_child_before_it_can_write_marker(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            child_code = "from pathlib import Path; import os,time; Path('child-started').write_text(str(os.getpid())); time.sleep(2); Path('leaked-child-marker').touch()"
            parent_code = "import subprocess,sys,time; subprocess.Popen([sys.executable, '-c', " + repr(child_code) + "]); time.sleep(20)"
            task = verify.Task("process-tree", "fast", ("python", "-c", parent_code))
            code, report = self.run_plan(root, [task], timeout=1)
            self.assertEqual(code, 1)
            self.assertIn("timed out", report["checks"][0]["reason"])
            self.assertTrue((root / "child-started").exists(), "fixture must actually create a child before testing cleanup")
            time.sleep(2.2)
            self.assertFalse((root / "leaked-child-marker").exists(), "timed-out task left a running child process")

    def test_successful_python_discovery_with_no_tests_is_failed_verification(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            # Synthetic older unittest output, independent of host module resolution.
            def execute(command, cwd, environment, output, timeout):
                output.write("Ran 0 tests in 0.000s\n\nOK\n")
                return 0

            task = verify.Task("empty-python", "fast", ("python", "-m", "unittest", "discover"))
            with patch.object(verify, "execute", side_effect=execute) as execute_mock:
                code, report = self.run_plan(root, [task])
            execute_mock.assert_called_once()
            self.assertEqual(code, 1)
            self.assertFalse(report["passed"])
            record = report["checks"][0]
            self.assertEqual(record["exit_code"], 0)
            self.assertEqual(record["tests_reported"], 0)
            self.assertEqual(record["status"], "failed")
            self.assertIn("no executed/passing tests", record["reason"])
            self.assertIn("Ran 0 tests", Path(record["log"]).read_text(encoding="utf-8"))

    def test_rust_exact_filter_with_zero_matches_is_not_a_pass(self):
        task = verify.Task("empty-rust", "integration", ("cargo", "test", "no-such-test", "--", "--exact"))
        summary = "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 2 filtered out; finished in 0.00s\n"
        self.assertEqual(verify.test_count(task, summary), 0)
        with tempfile.TemporaryDirectory() as directory:
            def execute(command, cwd, environment, output, timeout):
                output.write(summary)
                return 0

            with patch.object(verify, "execute", side_effect=execute):
                code, report = self.run_plan(Path(directory), [task])
            self.assertEqual(code, 1)
            self.assertEqual(report["checks"][0]["tests_reported"], 0)
            self.assertEqual(report["checks"][0]["status"], "failed")

    def test_swift_repeated_xctest_suite_summaries_are_not_added_again(self):
        task = verify.Task("swift", "native", ("swift", "test"))
        summary = (
            "Test Suite 'First' passed\nExecuted 2 tests, with 0 failures in 0.1 seconds\n"
            "Test Suite 'Second' passed\nExecuted 5 tests, with 0 failures in 0.1 seconds\n"
            "Test Suite 'All tests' passed\nExecuted 7 tests, with 0 failures in 0.2 seconds\n"
            "Executed 7 tests, with 0 failures in 0.2 seconds\n"
            "Test run with 0 tests passed after 0.001 seconds.\n"
        )
        self.assertEqual(verify.test_count(task, summary), 7)
        self.assertEqual(verify.test_count(task, "Test run with 0 tests passed after 0.001 seconds."), 0)

    def test_taskkill_timeout_still_kills_and_reaps_the_root_process(self):
        process = Mock()
        process.pid = 12345
        process.poll.return_value = None
        with patch.object(verify.sys, "platform", "win32"), patch.object(verify.subprocess, "run", side_effect=subprocess.TimeoutExpired(["taskkill"], 15)):
            with self.assertRaises(subprocess.TimeoutExpired):
                verify.stop_process_tree(process)
        process.kill.assert_called_once_with()
        process.wait.assert_called_once_with(timeout=15)

    def test_cleanup_failure_blocks_even_independent_following_tasks(self):
        tasks = [
            verify.Task("cleanup-failed", "fast", ("python", "-c", "print('first')")),
            verify.Task("must-not-run", "fast", ("python", "-c", "raise SystemExit('must not run')")),
        ]
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(verify, "execute", side_effect=verify.CleanupFailure("synthetic process tree cleanup failure")) as execute:
                code, report = self.run_plan(Path(directory), tasks)
            execute.assert_called_once()
            self.assertEqual(code, 1)
            self.assertEqual([record["status"] for record in report["checks"]], ["failed", "blocked"])
            self.assertIn("synthetic process tree cleanup failure", report["checks"][1]["reason"])

    def test_missing_executable_is_reported_without_losing_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            code, report = self.run_plan(Path(directory), [verify.Task("missing", "fast", ("desklink-verify-deliberately-missing-executable",))])
            self.assertEqual(code, 1)
            self.assertEqual(report["checks"][0]["status"], "failed")
            self.assertIn("reason", report["checks"][0])

    def test_exact_selection_adds_prerequisites_once_and_in_order(self):
        tasks = verify.catalog(Path("isolated target"), 30)
        selected = verify.select_tasks(tasks, [], ["rust-clippy", "frontend-build"])
        self.assertEqual([task.id for task in selected], ["frontend-deps", "frontend-build", "rust-clippy"])
        with self.assertRaisesRegex(ValueError, "unknown tasks"):
            verify.select_tasks(tasks, [], ["typo-must-not-succeed"])
        cyclic = [verify.Task("a", "fast", ("python",), requires=("b",)), verify.Task("b", "fast", ("python",), requires=("a",))]
        with self.assertRaisesRegex(ValueError, "cyclic"):
            verify.select_tasks(cyclic, ["fast"], [])

    def test_fast_plan_covers_all_bun_projects_python_and_rust_without_device_opt_in(self):
        selected = verify.select_tasks(verify.catalog(Path("target"), 30), ["fast"], [])
        names = {task.id for task in selected}
        self.assertTrue({"python-tests", "rust-unit", "rust-format"}.issubset(names))
        for project in verify.PROJECTS:
            self.assertTrue({f"{project}-deps", f"{project}-types", f"{project}-tests"}.issubset(names))
        self.assertFalse(any(task.device or task.host for task in selected))
        for task in selected:
            if "install" in task.command:
                self.assertIn("--frozen-lockfile", task.command)

    def make_manifests(self, root):
        for directory in verify.PROJECTS.values():
            path = root / directory
            path.mkdir(parents=True)
            (path / "package.json").write_text(json.dumps({"packageManager": "bun@1.3.14"}), encoding="utf-8")
        (root / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.97.1"\n', encoding="utf-8")

    def test_bun_mismatch_fails_before_running_install_or_tests(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_manifests(root)
            tasks = [verify.Task("bun-test", "fast", ("bun", "test"))]
            with patch.object(verify, "capture", return_value="1.4.3") as capture:
                versions, errors = verify.tool_versions(tasks, "chosen-bun", root, False)
            capture.assert_called_once_with(["chosen-bun", "--version"], root)
            self.assertEqual(versions["bun"], "1.4.3")
            self.assertIn("1.3.14 required", errors["bun"])
            code, report = self.run_plan(root, tasks, errors=errors)
            self.assertEqual(code, 1)
            self.assertIn("1.3.14 required", report["checks"][0]["reason"])
            self.assertNotIn("log", report["checks"][0])

    def test_pinned_bun_is_accepted_and_inconsistent_manifest_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_manifests(root)
            tasks = [verify.Task("bun-test", "fast", ("bun", "test"))]
            with patch.object(verify, "capture", return_value="1.3.14"):
                self.assertEqual(verify.tool_versions(tasks, "bun", root, False), ({"bun": "1.3.14"}, {}))
                (root / "server/account/package.json").write_text('{"packageManager":"bun@1.4.3"}', encoding="utf-8")
                _, errors = verify.tool_versions(tasks, "bun", root, False)
            self.assertIn("bun", errors)

    def test_malformed_manifest_preflight_writes_structured_failure(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_manifests(root)
            (root / "server/account/package.json").write_text('{"malformed":', encoding="utf-8")
            report_path = root / "evidence" / "preflight.json"
            arguments = ["verify.py", "--task", "account-tests", "--report", str(report_path)]
            with patch.object(verify, "ROOT", root), patch.object(verify.sys, "argv", arguments), patch.object(verify, "metadata", return_value={"source_commit": "b" * 40}), patch.object(verify, "run_tasks") as run_tasks, contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(verify.main(), 1)
            run_tasks.assert_not_called()
            report = json.loads(report_path.read_text(encoding="utf-8"))
            self.assertEqual(report["status"], "failed")
            self.assertFalse(report["passed"])
            self.assertIn("reason", report)
            self.assertEqual(report["source_commit"], "b" * 40)

    def test_explicit_bun_directory_precedes_global_bun_in_child_path(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            chosen_bun = root / "pinned tools" / "bun.exe"
            chosen_bun.parent.mkdir()
            chosen_bun.touch()
            arguments = ["verify.py", "--task", "python-tests", "--bun", str(chosen_bun)]
            observed = {}

            def run_child(tasks, **kwargs):
                observed["bun"] = kwargs["bun"]
                completed = subprocess.run([sys.executable, "-c", "import os; print(os.environ['PATH'].split(os.pathsep)[0])"], env=kwargs["environment"], capture_output=True, text=True, check=True)
                observed["path_first"] = completed.stdout.strip()
                return 0

            with patch.object(verify.sys, "argv", arguments), patch.object(verify, "metadata", return_value={}), patch.object(verify, "tool_versions", return_value=({}, {})), patch.object(verify, "run_tasks", side_effect=run_child):
                self.assertEqual(verify.main(), 0)
            self.assertEqual(Path(observed["bun"]).resolve(), chosen_bun.resolve())
            self.assertEqual(observed["path_first"], str(chosen_bun.resolve().parent))

    def test_rust_override_must_match_repository_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_manifests(root)
            with patch.object(verify, "capture", side_effect=["cargo 1.98.0 (fake)", "rustc 1.98.0 (fake)"]):
                _, errors = verify.tool_versions([verify.Task("rust", "fast", ("cargo", "test"))], "bun", root, False)
            self.assertIn("rust-toolchain.toml", errors["cargo"])

    def test_offline_baseline_missing_integrated_files_is_skipped_without_tool_probes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            task = next(task for task in verify.catalog(root / "target", 30) if task.id == "offline-media-baseline")
            with patch.object(verify.sys, "platform", "win32"), patch.object(verify, "capture") as capture, patch.object(verify, "execute") as execute:
                self.assertEqual(verify.tool_versions([task], "bun", root, False), ({}, {}))
                code, report = self.run_plan(root, [task])
            capture.assert_not_called()
            execute.assert_not_called()
            self.assertEqual(code, 2)
            self.assertFalse(report["passed"])
            record = report["checks"][0]
            self.assertEqual(record["status"], "skipped")
            for name in task.files:
                self.assertIn(name, record["reason"])

    def test_offline_baseline_checks_indirect_cargo_version_before_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.make_manifests(root)
            task = next(task for task in verify.catalog(root / "target", 30) if task.id == "offline-media-baseline")
            for name in task.files:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            with patch.object(verify.sys, "platform", "win32"), patch.object(verify, "capture", side_effect=["cargo 1.98.0 (fake)", "rustc 1.98.0 (fake)"]) as capture, patch.object(verify, "execute") as execute:
                self.assertIsNone(verify.skip_reason(task, "win32", False, root))
                versions, errors = verify.tool_versions([task], "bun", root, False)
                code, report = self.run_plan(root, [task], errors=errors)
            self.assertEqual([call.args[0] for call in capture.call_args_list], [["cargo", "--version"], ["rustc", "--version"]])
            self.assertIn("cargo", versions)
            self.assertIn("cargo", errors)
            execute.assert_not_called()
            self.assertEqual(code, 1)
            self.assertFalse(report["passed"])
            self.assertEqual(report["checks"][0]["status"], "failed")
            self.assertIn("rust-toolchain.toml", report["checks"][0]["reason"])

    def test_windows_cargo_uses_wrapper_and_macos_uses_native_cargo(self):
        task = verify.Task("rust", "fast", ("cargo", "test", "--locked", "--lib"))
        with patch.object(verify.sys, "platform", "win32"):
            self.assertEqual(verify.command_for(task, ROOT, "bun"), [sys.executable, str(ROOT / "scripts/run-windows-cargo.py"), "test", "--locked", "--lib"])
        with patch.object(verify.sys, "platform", "darwin"):
            self.assertEqual(verify.command_for(task, ROOT, "bun"), ["cargo", "test", "--locked", "--lib"])

    def test_swift_linker_uses_selected_target_as_one_argument(self):
        target = Path("isolated target")
        tasks = {task.id: task for task in verify.catalog(target, 30)}
        for name in ("apple-core", "macos-app"):
            self.assertIn("-L" + str(target / "aarch64-apple-darwin/release"), tasks[name].command)
            self.assertEqual(tasks[name].requires, ("apple-ffi-build",))
        with patch.object(verify.platform, "machine", return_value="arm64"):
            self.assertIsNone(verify.skip_reason(tasks["apple-core"], "darwin", False))
        with patch.object(verify.platform, "machine", return_value="x86_64"):
            self.assertIn("Apple Silicon", verify.skip_reason(tasks["apple-core"], "darwin", False))

    def test_main_passes_isolated_target_to_cargo_environment_and_swift_linker(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "isolated target"
            arguments = ["verify.py", "--task", "apple-core", "--target-dir", str(target), "--soak-seconds", "45"]
            with patch.object(verify.sys, "argv", arguments), patch.object(verify, "metadata", return_value={}), patch.object(verify, "tool_versions", return_value=({}, {})), patch.object(verify, "run_tasks", return_value=2) as run_tasks:
                self.assertEqual(verify.main(), 2)
            passed_tasks = run_tasks.call_args.args[0]
            environment = run_tasks.call_args.kwargs["environment"]
            self.assertEqual(environment["CARGO_TARGET_DIR"], str(target.resolve()))
            self.assertEqual(environment["DESKLINK_SOAK_SECONDS"], "45")
            self.assertEqual([task.id for task in passed_tasks], ["apple-ffi-build", "apple-core"])
            self.assertIn("-L" + str(target.resolve() / "aarch64-apple-darwin/release"), passed_tasks[-1].command)

    def test_catalog_does_not_enable_managed_network_or_ignored_device_tests_by_default(self):
        for task in verify.catalog(Path("target"), 30):
            self.assertNotIn("managed_relay_live", task.command)
            self.assertNotIn("cloud_diagnostics_live", task.command)
            self.assertFalse(any("verify-windows-resilience" in arg or "deploy-" in arg for arg in task.command))
            if "--ignored" in task.command:
                self.assertTrue(task.device, task.id)
        baseline = next(task for task in verify.catalog(Path("target"), 30) if task.id == "performance-baseline")
        self.assertIn("DL2-014", verify.skip_reason(baseline, sys.platform, False))

    def test_list_mode_runs_from_other_directory_and_writes_no_report(self):
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "must-not-exist.json"
            completed = subprocess.run([sys.executable, str(ROOT / "scripts/verify.py"), "all", "--list", "--report", str(report)], cwd=directory, capture_output=True, text=True, check=False)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertIn("frontend-tests", completed.stdout)
            self.assertIn("SKIP:", completed.stdout)
            self.assertFalse(report.exists())


if __name__ == "__main__":
    unittest.main()
