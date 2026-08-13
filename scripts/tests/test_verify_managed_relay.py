from __future__ import annotations

import importlib.util
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]


def load_script():
    path = ROOT / "scripts" / "verify-managed-relay.py"
    spec = importlib.util.spec_from_file_location("verify_managed_relay", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class VerifyManagedRelayTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.verify = load_script()

    def test_retries_a_transient_timeout_then_stops_on_success(self) -> None:
        timeout = subprocess.TimeoutExpired(["cargo"], self.verify.PROBE_TIMEOUT_SECONDS)
        success = subprocess.CompletedProcess(
            ["cargo"], 0, stdout="probe passed\n", stderr=""
        )
        with (
            patch.object(self.verify.subprocess, "run", side_effect=[timeout, success]) as run,
            patch.object(self.verify.time, "sleep") as sleep,
            patch.object(self.verify.time, "monotonic", side_effect=[0, 1]),
        ):
            exit_code, output, attempts, elapsed_ms = self.verify.run_probe(["cargo"])

        self.assertEqual(exit_code, 0)
        self.assertEqual(attempts, 2)
        self.assertGreaterEqual(elapsed_ms, 1)
        self.assertIn("attempt 1: timeout=90s", output)
        self.assertIn("attempt 2: exit_code=0", output)
        self.assertEqual(run.call_count, 2)
        sleep.assert_called_once_with(3)

    def test_exhausts_three_attempts_with_bounded_backoff(self) -> None:
        failure = subprocess.CompletedProcess(
            ["cargo"], 1, stdout="network unavailable\n", stderr=""
        )
        with (
            patch.object(self.verify.subprocess, "run", side_effect=[failure] * 3) as run,
            patch.object(self.verify.time, "sleep") as sleep,
            patch.object(self.verify.time, "monotonic", side_effect=[0, 10]),
        ):
            exit_code, output, attempts, _ = self.verify.run_probe(["cargo"])

        self.assertEqual(exit_code, 1)
        self.assertEqual(attempts, 3)
        self.assertEqual(run.call_count, 3)
        self.assertEqual([call.args[0] for call in sleep.call_args_list], [3, 6])
        self.assertEqual(output.count("network unavailable"), 3)


if __name__ == "__main__":
    unittest.main()
