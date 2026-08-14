from __future__ import annotations

import importlib.util
import hashlib
import sys
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch


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

    def test_validates_relay_source_revision(self) -> None:
        revision = "a" * 40
        self.assertEqual(self.audit.validate_source_commit(revision), revision)
        with self.assertRaises(ValueError):
            self.audit.validate_source_commit("a" * 39)

    def test_pins_ssh_host_when_a_known_hosts_file_is_configured(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_directory:
            known_hosts = Path(temporary_directory) / "known_hosts"
            known_hosts.write_text("relay ssh-ed25519 AAAA\n", encoding="utf-8")
            arguments = SimpleNamespace(
                identity_file=None,
                known_hosts_file=known_hosts,
                target="root@example",
            )
            command = self.audit.ssh_command(arguments, "true")
            self.assertIn("StrictHostKeyChecking=yes", command)
            self.assertIn(f"UserKnownHostsFile={known_hosts}", command)

    def test_reads_relay_source_revision_from_json_labels(self) -> None:
        revision = "b" * 40
        self.assertEqual(
            self.audit.source_commit_from_labels(
                '{"org.opencontainers.image.revision":"%s"}' % revision
            ),
            revision,
        )
        with self.assertRaisesRegex(ValueError, "labels are not valid JSON"):
            self.audit.source_commit_from_labels("not-json")
        with self.assertRaisesRegex(ValueError, "label is missing"):
            self.audit.source_commit_from_labels("{}")


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

    def test_requires_a_full_source_revision_on_the_relay_image(self) -> None:
        revision = "a" * 40
        self.assertEqual(self.deploy.validate_source_commit(revision), revision)
        with self.assertRaises(RuntimeError):
            self.deploy.validate_source_commit("a" * 39)
        with self.assertRaises(RuntimeError):
            self.deploy.validate_source_commit("not-a-commit")

    def test_running_container_revision_must_match_loaded_image(self) -> None:
        revision = "a" * 40
        self.assertEqual(
            self.deploy.validate_running_source_commit(revision, revision),
            revision,
        )
        with self.assertRaisesRegex(RuntimeError, "does not match"):
            self.deploy.validate_running_source_commit(revision, "b" * 40)

    def test_health_requires_a_structured_healthy_status(self) -> None:
        class FakeSsh:
            def __init__(self, output: str) -> None:
                self.output = output

            def run(self, arguments: list[str], *, check: bool = True) -> str:
                return self.output

        self.assertTrue(
            self.deploy.healthy(
                FakeSsh('{"Running":true,"Health":{"Status":"healthy"}}'),
                "relay",
            )
        )
        self.assertFalse(
            self.deploy.healthy(FakeSsh('{"Running":true,"Health":null}'), "relay")
        )
        self.assertFalse(
            self.deploy.healthy(FakeSsh('{"Running":true}'), "relay")
        )

    def test_rolls_back_when_deploy_health_check_fails(self) -> None:
        class FakeSsh:
            def __init__(self, target: str, identity_file: Path | None) -> None:
                self.calls: list[tuple[list[str], bool]] = []

            def run(self, arguments: list[str], *, check: bool = True) -> str:
                self.calls.append((arguments, check))
                if arguments[:3] == ["docker", "inspect", "relay"]:
                    format_argument = arguments[-1]
                    if "working_dir" in format_argument:
                        return "/srv/desklink"
                    if "config_files" in format_argument:
                        return "/srv/desklink/compose.yml"
                    if "service" in format_argument:
                        return "relay"
                    if ".Image" in format_argument:
                        return "sha256:old"
                if (
                    arguments[:2] == ["docker", "inspect"]
                    and "org.opencontainers.image.revision" in arguments[-2]
                ):
                    return "a" * 40
                if arguments[:1] == ["sha256sum"]:
                    return f"{archive_sha256}  {arguments[-1]}"
                if arguments[:3] == ["docker", "inspect", "relay"]:
                    return ""
                if arguments[:2] == ["docker", "inspect"]:
                    return ""
                return ""

            def copy(self, source: Path, destination: str) -> None:
                self.calls.append((["copy", str(source), destination], True))

        with tempfile.TemporaryDirectory() as temporary_directory:
            archive = Path(temporary_directory) / "relay.tar"
            archive.write_bytes(b"relay archive")
            archive_sha256 = hashlib.sha256(archive.read_bytes()).hexdigest()
            arguments = SimpleNamespace(
                target="root@example",
                identity_file=None,
                archive=archive,
                container="relay",
                health_timeout=5,
            )
            fake_ssh = FakeSsh("root@example", None)
            with (
                patch.object(self.deploy, "parse_args", return_value=arguments),
                patch.object(self.deploy, "Ssh", return_value=fake_ssh),
                patch.object(self.deploy, "healthy", return_value=False),
                patch.object(self.deploy.time, "time", return_value=0),
                patch.object(self.deploy.time, "monotonic", side_effect=[0, 10]),
            ):
                with self.assertRaisesRegex(RuntimeError, "restored desklink-relay:rollback-"):
                    self.deploy.main()

            commands = [call[0] for call in fake_ssh.calls]
            self.assertIn(["docker", "tag", "sha256:old", "desklink-relay:rollback-0"], commands)
            self.assertIn(["docker", "tag", "desklink-relay:rollback-0", "desklink-relay:0.1.0"], commands)
            self.assertEqual(commands.count(self.deploy.compose_command("/srv/desklink", "/srv/desklink/compose.yml", "relay")), 2)


if __name__ == "__main__":
    unittest.main()
