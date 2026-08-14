from __future__ import annotations

import re
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


class WindowsWorkflowPolicyTests(unittest.TestCase):
    def test_release_workflows_pin_the_repository_rust_toolchain(self) -> None:
        toolchain = (ROOT / "rust-toolchain.toml").read_text(encoding="utf-8")
        self.assertRegex(toolchain, r'(?m)^channel\s*=\s*"1\.97\.1"\s*$')
        for name in (
            "windows-ci.yml",
            "windows-resilience-nightly.yml",
            "windows-signed-release.yml",
            "security-scan.yml",
            "managed-relay-monitor.yml",
        ):
            workflow = (ROOT / ".github" / "workflows" / name).read_text(
                encoding="utf-8"
            )
            self.assertRegex(
                workflow,
                r"uses: dtolnay/rust-toolchain@stable\r?\n"
                r"[ \t]+with:\r?\n"
                r"(?:[ \t]+[^\r\n]*\r?\n)*"
                r"[ \t]+toolchain: 1\.97\.1",
                msg=f"{name} must pin Rust 1.97.1",
            )

    def test_relay_monitor_fails_partial_ssh_configuration_and_pins_hosts(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "managed-relay-monitor.yml").read_text(
            encoding="utf-8"
        )
        for secret in (
            "DESKLINK_RELAY_SSH_TARGET",
            "DESKLINK_RELAY_SSH_PRIVATE_KEY",
            "DESKLINK_RELAY_SSH_KNOWN_HOSTS",
        ):
            self.assertIn(f"secrets.{secret}", workflow)
        self.assertIn("Validate optional host-audit configuration", workflow)
        self.assertIn("Audit relay host and diagnostics (when configured)", workflow)
        self.assertIn("--known-hosts-file", workflow)
        self.assertIn("dist/linux/managed-relay-host-audit.json", workflow)
        self.assertIn("dist/linux/managed-diagnostics-audit.json", workflow)

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

    def test_signed_workflow_requires_the_release_soak_in_readiness(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "windows-signed-release.yml").read_text(
            encoding="utf-8"
        )
        self.assertRegex(
            workflow,
            r"python scripts/check-windows-release-ready\.py\s+"
            r"--manual-json dist/windows/windows-acceptance-record\.json\s+"
            r"--minimum-soak-seconds 300",
        )

    def test_nightly_workflow_keeps_the_long_soak_guard(self) -> None:
        workflow = (ROOT / ".github" / "workflows" / "windows-resilience-nightly.yml").read_text(
            encoding="utf-8"
        )
        self.assertRegex(
            workflow,
            r"name: Run 300 second Windows resilience checks\s+"
            r"run: >-\s+"
            r"python scripts/verify-windows-resilience\.py\s+"
            r"--soak-seconds 300",
        )
        self.assertIn("name: DeskLink-Windows-resilience-${{ github.run_id }}", workflow)


if __name__ == "__main__":
    unittest.main()
