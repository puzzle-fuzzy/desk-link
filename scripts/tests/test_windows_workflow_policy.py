from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class WindowsWorkflowPolicyTests(unittest.TestCase):
    def test_signed_candidates_require_the_release_soak_duration(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "windows-signed-release.yml").read_text(
            encoding="utf-8"
        )
        self.assertRegex(
            workflow,
            r"name: Run native Windows resilience checks\s+run: python scripts/verify-windows-resilience\.py --soak-seconds 300",
        )

    def test_regular_ci_keeps_the_short_feedback_loop(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "windows-ci.yml").read_text(
            encoding="utf-8"
        )
        self.assertRegex(
            workflow,
            r"name: Run native Windows resilience checks\s+run: python scripts/verify-windows-resilience\.py --soak-seconds 10",
        )

    def test_signed_workflow_does_not_silently_use_the_ci_duration(self) -> None:
        signed = (ROOT / ".github" / "workflows" / "windows-signed-release.yml").read_text(
            encoding="utf-8"
        )
        matches = re.findall(r"verify-windows-resilience\.py --soak-seconds (\d+)", signed)
        self.assertEqual(matches, ["300"])


if __name__ == "__main__":
    unittest.main()
