from __future__ import annotations

import base64
import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]


def load_script(name: str):
    path = ROOT / "scripts" / name
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Could not load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class BuildSigningPolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.build = load_script("build-windows-installer.py")

    def test_required_signing_rejects_missing_identity(self) -> None:
        with self.assertRaises(SystemExit):
            self.build.enforce_signing_policy(requested=False, required=True)

    def test_local_build_can_remain_unsigned(self) -> None:
        self.build.enforce_signing_policy(requested=False, required=False)

    def test_environment_flag_is_strict(self) -> None:
        with patch.dict(os.environ, {"DESKLINK_REQUIRE_SIGNING": "yes"}, clear=False):
            self.assertTrue(self.build.environment_flag("DESKLINK_REQUIRE_SIGNING"))
        with patch.dict(os.environ, {"DESKLINK_REQUIRE_SIGNING": "maybe"}, clear=False):
            with self.assertRaises(SystemExit):
                self.build.environment_flag("DESKLINK_REQUIRE_SIGNING")


class SignToolConfigurationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.signing = load_script("sign-windows-artifact.py")

    def test_artifact_signing_uses_microsoft_timestamp_and_sha256(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dlib = root / "Azure.CodeSigning.Dlib.dll"
            metadata = root / "metadata.json"
            dlib.write_bytes(b"dlib")
            metadata.write_text("{}", encoding="utf-8")
            environment = {
                "DESKLINK_ARTIFACT_SIGNING_DLIB": str(dlib),
                "DESKLINK_ARTIFACT_SIGNING_METADATA": str(metadata),
            }
            with patch.dict(os.environ, environment, clear=True):
                configuration = self.signing.read_signing_configuration()
            command = self.signing.signing_command(
                Path("signtool.exe"), configuration, Path("DeskLink.exe")
            )
            self.assertEqual(configuration.mode, "artifact-signing")
            self.assertIn("http://timestamp.acs.microsoft.com", command)
            self.assertEqual(command.count("SHA256"), 2)
            self.assertIn("/dlib", command)

    def test_certificate_thumbprint_is_normalized(self) -> None:
        thumbprint = "aa " * 19 + "aa"
        with patch.dict(os.environ, {"DESKLINK_SIGN_CERT_SHA1": thumbprint}, clear=True):
            configuration = self.signing.read_signing_configuration()
        self.assertEqual(configuration.thumbprint, "AA" * 20)


class PfxPreparationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.prepare = load_script("prepare-windows-signing-certificate.py")

    def test_decodes_wrapped_base64(self) -> None:
        encoded = base64.b64encode(b"desklink-pfx").decode("ascii")
        wrapped = f"  {encoded[:5]}\n{encoded[5:]}  "
        self.assertEqual(self.prepare.decode_pfx(wrapped), b"desklink-pfx")

    def test_rejects_invalid_base64(self) -> None:
        with self.assertRaises(SystemExit):
            self.prepare.decode_pfx("not-a-pfx!")


if __name__ == "__main__":
    unittest.main()
