from __future__ import annotations

import importlib.util
import sys
import unittest
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def load_script():
    path = ROOT / "scripts" / "audit-managed-relay.py"
    spec = importlib.util.spec_from_file_location("audit_managed_relay", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def load_deploy_script():
    path = ROOT / "scripts" / "deploy-managed-relay.py"
    spec = importlib.util.spec_from_file_location("deploy_managed_relay", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ManagedRelayAuditTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.audit = load_script()

    def test_parses_aggregate_capacity_without_session_data(self) -> None:
        sample = (
            "unrelated\nrelay_capacity active_sessions=12 attached_participants=20 "
            "accepted_connections=22 max_sessions=256 max_connections=512\n"
        )
        self.assertEqual(
            self.audit.parse_capacity(sample),
            {
                "active_sessions": 12,
                "attached_participants": 20,
                "accepted_connections": 22,
                "max_sessions": 256,
                "max_connections": 512,
            },
        )

    def test_parses_certificate_window_and_disk_usage(self) -> None:
        now = datetime(2026, 7, 1, tzinfo=timezone.utc)
        self.assertEqual(
            self.audit.parse_certificate_expiry(
                "notAfter=Jul 31 00:00:00 2026 GMT", now
            ),
            30,
        )
        self.assertEqual(
            self.audit.parse_disk_percent(
                "Filesystem 1024-blocks Used Available Capacity Mounted on\n"
                "/dev/vda1 100 48 52 48% /\n"
            ),
            48,
        )


class ManagedRelayDeployTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.deploy = load_deploy_script()

    def test_rejects_shell_metacharacters_in_remote_labels(self) -> None:
        self.assertEqual(
            self.deploy.validate_remote_value("service", "relay"), "relay"
        )
        with self.assertRaises(RuntimeError):
            self.deploy.validate_remote_value("service", "relay; reboot")
        with self.assertRaises(RuntimeError):
            self.deploy.validate_remote_value("path", "relative/compose.yml", absolute=True)


if __name__ == "__main__":
    unittest.main()
