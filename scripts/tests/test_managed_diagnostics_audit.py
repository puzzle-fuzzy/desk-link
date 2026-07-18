from __future__ import annotations

import importlib.util
import sys
import unittest
from datetime import datetime, timezone
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]


def load_script():
    path = ROOT / "scripts" / "audit-managed-diagnostics.py"
    spec = importlib.util.spec_from_file_location("audit_managed_diagnostics", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ManagedDiagnosticsAuditTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.audit = load_script()

    def test_accepts_only_fresh_aggregate_reports(self) -> None:
        now = datetime(2026, 7, 18, 8, 0, tzinfo=timezone.utc)
        value = {
            "schema": 1,
            "status": "attention",
            "requires_attention": True,
            "generated_at": "2026-07-18T07:30:00.000Z",
            "window_hours": 24,
            "summary": {
                "sessions": 3,
                "healthy": 1,
                "warning": 1,
                "error": 1,
                "incomplete": 0,
                "finding_counts": {},
            },
        }
        self.assertEqual(self.audit.validate_summary(value, now), value)
        stale = dict(value, generated_at="2026-07-18T04:00:00.000Z")
        with self.assertRaises(ValueError):
            self.audit.validate_summary(stale, now)

    def test_rejects_non_integer_or_negative_counts(self) -> None:
        now = datetime(2026, 7, 18, 8, 0, tzinfo=timezone.utc)
        value = {
            "schema": 1,
            "status": "healthy",
            "requires_attention": False,
            "generated_at": "2026-07-18T08:00:00.000Z",
            "window_hours": 24,
            "summary": {
                "sessions": -1,
                "healthy": 0,
                "warning": 0,
                "error": 0,
                "incomplete": 0,
            },
        }
        with self.assertRaises(ValueError):
            self.audit.validate_summary(value, now)


if __name__ == "__main__":
    unittest.main()
