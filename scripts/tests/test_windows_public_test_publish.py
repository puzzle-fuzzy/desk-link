from __future__ import annotations

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
from windows_update_signing import public_bytes, sign_manifest_bytes
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey


def load_script(name: str):
    path = ROOT / "scripts" / name
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class PublicTestPublishTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.publish = load_script("publish-windows-public-test.py")
        cls.build = load_script("build-windows-installer.py")

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="desklink-public-test-policy-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.key = Ed25519PrivateKey.from_private_bytes(bytes([7]) * 32)
        public = self.root / "apps/windows-ui/src-tauri/update-public-key.hex"
        public.parent.mkdir(parents=True)
        public.write_text(public_bytes(self.key).hex() + "\n", encoding="ascii")
        (self.root / ".gitignore").write_text("dist/\ntarget/\n", encoding="ascii")
        self.git("init", "-q")
        self.git("add", ".")
        self.git("-c", "user.name=DeskLink Fixture", "-c", "user.email=fixture@example.invalid", "-c", "commit.gpgsign=false", "-c", "core.hooksPath=disabled-fixture-hooks", "commit", "-qm", "isolated test source")
        self.commit = self.git("rev-parse", "HEAD")
        self.git("tag", "v0.1.93")
        dist = self.root / "dist/windows"
        dist.mkdir(parents=True)
        self.manifest = dist / "windows-installer-manifest.json"
        self.signature = dist / "windows-installer-manifest.json.sig"
        self.verification = dist / "windows-release-verification.json"
        self.application = self.root / "target/x86_64-pc-windows-msvc/release/desklink-windows-ui.exe"
        self.application.parent.mkdir(parents=True)
        self.application.write_bytes(b"synthetic fixture application bytes; never executable")
        app = self.application.read_bytes()
        self.installer = dist / "DeskLinkSetup-0.1.93-x64.exe"
        self.installer.write_bytes(bytes(256) + app + bytes(1_000_000))
        self.value = {
            "schema":1,"version":"0.1.93","target":self.publish.TARGET,
            "release_channel":"publicTest","update_mode":"perUserV1","signed":False,"passed":True,
            "source_commit":self.commit,"source_dirty":False,"release_scope":self.publish.EXPECTED_SCOPE,
            "application":{"file_name":"DeskLink.exe","size_bytes":len(app),"sha256":hashlib.sha256(app).hexdigest(),"source_release_sha256":hashlib.sha256(app).hexdigest()},
            "installer":{"file_name":self.installer.name,"size_bytes":self.installer.stat().st_size,"sha256":hashlib.sha256(self.installer.read_bytes()).hexdigest(),"embedded_payload_offset":256},
        }
        self.save_manifest()
        self.report = {"schema":1,"version":"0.1.93","passed":True,"custom_protocol":True,"source_commit":self.commit,"source_dirty":False,"release_scope":self.publish.EXPECTED_SCOPE,"release":{"size_bytes":len(app),"sha256":hashlib.sha256(app).hexdigest()}}
        self.save_report()

    def git(self, *arguments):
        return subprocess.run(["git", *arguments], cwd=self.root, capture_output=True, text=True, check=True).stdout.strip()

    def save_manifest(self):
        raw = json.dumps(self.value).encode("utf-8")
        self.manifest.write_bytes(raw)
        self.signature.write_bytes(sign_manifest_bytes(raw, self.key))

    def save_report(self):
        self.verification.write_text(json.dumps(self.report), encoding="utf-8")

    def validate(self):
        return self.publish.validate_public_test_payload(self.root, "v0.1.93")

    def test_accepts_explicit_unsigned_test_without_inventing_formal_readiness(self):
        payload = self.validate()
        self.assertEqual(payload.source_commit, self.commit)
        self.assertEqual(len(payload.files), 4)
        self.assertFalse((self.root / "dist/windows/windows-release-readiness.json").exists())
        notes = self.publish.release_notes(payload)
        self.assertIn("公开测试", notes)
        self.assertIn("未 Authenticode", notes)
        self.assertIn("不是正式发布", notes)
        self.assertIn("0.1.92", notes)

    def test_only_allowlisted_public_report_is_uploaded_and_local_paths_remain_private(self):
        self.report["release"]["path"] = str(self.application)
        self.report["local_debug_log"] = "private fixture diagnostic content"
        self.save_report()
        payload = self.validate()
        self.assertEqual(payload.verification.name, "windows-public-test-verification.json")
        self.assertNotIn(self.verification, payload.files)
        raw = payload.verification.read_text(encoding="utf-8")
        self.assertNotIn(str(self.root), raw)
        self.assertNotIn("private fixture diagnostic content", raw)
        public = json.loads(raw)
        self.assertIs(public["ready"], False)
        self.assertIs(public["signed"], False)
        self.assertTrue(public["manifest"]["signature_verified"])
        self.value["local_private_path"] = str(self.root)
        self.save_manifest()
        with self.assertRaisesRegex(ValueError,"unreviewed"): self.validate()

    def test_rejects_clean_or_signed_or_channel_claims_that_do_not_match_policy(self):
        for field, value in [("signed",True),("release_channel","stable"),("source_dirty",True),("source_commit","a"*40),("target","aarch64-pc-windows-msvc"),("update_mode","legacy"),("version","0.1.92")]:
            with self.subTest(field=field):
                original=self.value[field]
                self.value[field]=value; self.save_manifest()
                with self.assertRaises(ValueError): self.validate()
                self.value[field]=original; self.save_manifest()

    def test_rejects_dirty_source_even_if_manifest_claims_clean(self):
        (self.root / "untracked-source.rs").write_text("changed source", encoding="ascii")
        with self.assertRaisesRegex(ValueError,"clean"): self.validate()

    def test_rejects_tag_that_does_not_bind_to_actual_head(self):
        self.git("tag", "-d", "v0.1.93")
        with self.assertRaises(subprocess.CalledProcessError): self.validate()

    def test_rejects_tampered_raw_manifest_and_missing_signature(self):
        self.manifest.write_bytes(self.manifest.read_bytes()+b" ")
        with self.assertRaisesRegex(ValueError,"signature"): self.validate()
        self.save_manifest(); self.signature.unlink()
        with self.assertRaises(OSError): self.validate()

    def test_rejects_actual_installer_or_application_tampering(self):
        content=self.installer.read_bytes(); self.installer.write_bytes(content[:-1]+b"x")
        with self.assertRaisesRegex(ValueError,"actual size/hash"): self.validate()
        self.installer.write_bytes(content)
        self.application.write_bytes(b"tampered application")
        with self.assertRaisesRegex(ValueError,"actual size/hash"): self.validate()

    def test_rejects_rehashed_installer_with_wrong_final_embedded_payload(self):
        data=bytearray(self.installer.read_bytes()); data[256]^=1; self.installer.write_bytes(data)
        self.value["installer"]["sha256"]=hashlib.sha256(data).hexdigest(); self.save_manifest()
        with self.assertRaisesRegex(ValueError,"embed"): self.validate()

    def test_rejects_build_report_source_version_protocol_or_payload_drift(self):
        for field,value in [("source_commit","a"*40),("source_dirty",True),("version","0.1.92"),("custom_protocol",False),("passed",False),("release",{"size_bytes":1,"sha256":"0"*64})]:
            with self.subTest(field=field):
                original=self.report[field]; self.report[field]=value; self.save_report()
                with self.assertRaises(ValueError): self.validate()
                self.report[field]=original; self.save_report()

    def test_public_build_gate_uses_actual_clean_head_not_github_sha_hint(self):
        with patch.dict(os.environ,{"GITHUB_SHA":""}):
            self.assertEqual(self.build.public_test_source(self.root),self.commit)
        with patch.dict(os.environ,{"GITHUB_SHA":"a"*40}):
            with self.assertRaisesRegex(SystemExit,"actual public test HEAD"): self.build.public_test_source(self.root)
        (self.root / "new.rs").write_text("new source",encoding="ascii")
        with patch.dict(os.environ,{"GITHUB_SHA":""}):
            with self.assertRaisesRegex(SystemExit,"clean"): self.build.public_test_source(self.root)

    def test_public_build_policy_does_not_relax_authenticode_requirement(self):
        for requested,required in [(True,False),(False,True),(True,True)]:
            with self.assertRaises(SystemExit): self.build.enforce_public_test_policy(public_test=True,requested=requested,required=required)
        self.build.enforce_public_test_policy(public_test=True,requested=False,required=False)
        with self.assertRaises(SystemExit): self.build.enforce_signing_policy(requested=False,required=True)

    def test_draft_creation_uploads_four_files_but_never_publishes(self):
        payload=self.validate()
        with patch.object(self.publish,"verify_remote_tag"),patch.object(self.publish,"verify_public_repository"),patch.object(self.publish,"verify_draft_assets") as verify,patch.object(self.publish.subprocess,"run") as run:
            self.publish.create_draft(payload,"v0.1.93")
        command=run.call_args.args[0]
        self.assertEqual(command[:3],["gh","release","create"])
        self.assertIn("--draft",command)
        self.assertNotIn("--latest",command)
        self.assertTrue(all(str(path) in command for path in payload.files))
        verify.assert_called_once_with(payload,"v0.1.93")

    def test_publish_requires_verified_draft_before_visibility_change(self):
        payload=self.validate()
        with patch.object(self.publish,"verify_draft_assets",side_effect=ValueError("bad remote bytes")),patch.object(self.publish.subprocess,"run") as run:
            with self.assertRaisesRegex(ValueError,"bad remote"): self.publish.publish_draft(payload,"v0.1.93")
            run.assert_not_called()
        with patch.object(self.publish,"verify_draft_assets"),patch.object(self.publish,"validate_public_test_payload",return_value=payload),patch.object(self.publish.subprocess,"run") as run:
            self.publish.publish_draft(payload,"v0.1.93")
        self.assertIn("--draft=false",run.call_args.args[0])
        self.assertIn("--prerelease=false",run.call_args.args[0])
        self.assertIn("--latest",run.call_args.args[0])

    def test_private_repo_and_wrong_remote_tag_are_rejected_without_visibility_changes(self):
        payload=self.validate()
        with patch.object(self.publish,"gh_json",return_value={"full_name":self.publish.REPOSITORY,"private":True}):
            with self.assertRaisesRegex(ValueError,"already be public"): self.publish.verify_public_repository(self.root)
        with patch.object(self.publish,"gh_json",return_value={"object":{"type":"commit","sha":"f"*40}}):
            with self.assertRaisesRegex(ValueError,"Remote public test tag"): self.publish.verify_remote_tag(payload,"v0.1.93")

    def test_draft_downloaded_bytes_are_compared_before_publication(self):
        payload=self.validate()
        release={"tagName":"v0.1.93","isDraft":True,"isPrerelease":False,"name":"DeskLink 0.1.93 公开测试版（unsigned）","body":self.publish.release_notes(payload),"assets":[{"name":path.name,"size":path.stat().st_size} for path in payload.files]}
        def fake_run(command,**kwargs):
            if command[:3]==["gh","release","view"]:
                return subprocess.CompletedProcess(command,0,stdout=json.dumps(release))
            self.assertEqual(command[:3],["gh","release","download"])
            directory=Path(command[command.index("--dir")+1])
            for source in payload.files: shutil.copyfile(source,directory/source.name)
            if self.tamper_remote: (directory/payload.installer.name).write_bytes(b"bad remote package")
            return subprocess.CompletedProcess(command,0)
        for tamper in [False,True]:
            self.tamper_remote=tamper
            with patch.object(self.publish,"verify_remote_tag"),patch.object(self.publish,"verify_public_repository"),patch.object(self.publish.subprocess,"run",side_effect=fake_run):
                if tamper:
                    with self.assertRaisesRegex(ValueError,"downloaded bytes"): self.publish.verify_draft_assets(payload,"v0.1.93")
                else: self.publish.verify_draft_assets(payload,"v0.1.93")


if __name__ == "__main__":
    unittest.main()
