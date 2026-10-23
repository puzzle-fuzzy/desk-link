from __future__ import annotations

import os
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import windows_update_signing as signing
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey


class WindowsUpdateSigningTests(unittest.TestCase):
    def setUp(self):
        self.key = Ed25519PrivateKey.generate()
        self.public = signing.public_bytes(self.key)
        self.raw = b'{"schema":1,"version":"0.1.92","installer":{"sha256":"abc"}}\n'
        self.signature = signing.sign_manifest_bytes(self.raw, self.key)

    def test_exact_bytes_verify_and_no_json_normalization_is_allowed(self):
        signing.verify_manifest_signature(self.raw, self.signature, self.public)
        with self.assertRaises(ValueError):
            signing.verify_manifest_signature(self.raw.rstrip(), self.signature, self.public)

    def test_wrong_key_and_altered_signature_are_rejected(self):
        wrong = signing.public_bytes(Ed25519PrivateKey.generate())
        for signature, public in [(self.signature, wrong), (b"0" * 128, self.public)]:
            with self.subTest(signature=signature[:8], public=public.hex()[:8]):
                with self.assertRaises(ValueError):
                    signing.verify_manifest_signature(self.raw, signature, public)

    def test_signature_for_another_domain_is_rejected(self):
        signature = self.key.sign(self.raw).hex().encode()
        with self.assertRaises(ValueError):
            signing.verify_manifest_signature(self.raw, signature, self.public)

    def test_text_files_allow_lf_or_crlf_but_not_arbitrary_whitespace(self):
        for ending in (b"", b"\n", b"\r\n"):
            signing.verify_manifest_signature(self.raw, self.signature.rstrip(b"\n") + ending, self.public)
        for signature in (b" " + self.signature.rstrip(), self.signature.rstrip() + b" ", self.signature + b"\n"):
            with self.assertRaises(ValueError):
                signing.verify_manifest_signature(self.raw, signature, self.public)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "public.hex"
            path.write_bytes(self.public.hex().encode() + b"\r\n")
            self.assertEqual(signing.read_public_key(path), self.public)

    def test_invalid_encoding_and_unbounded_inputs_are_rejected(self):
        for signature in [b"", b"A" * 128, b"f" * 127, b"f" * 131, b"\xff" * 128]:
            with self.subTest(signature=signature[:8]):
                with self.assertRaises(ValueError):
                    signing.verify_manifest_signature(self.raw, signature, self.public)
        with self.assertRaises(ValueError):
            signing.sign_manifest_bytes(b"x" * (signing.MAX_MANIFEST_BYTES + 1), self.key)

    def test_private_key_path_inside_checkout_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "outside"):
            signing._external_key_path(signing.ROOT / "dist" / "private.key")

    @unittest.skipUnless(os.name == "nt", "current-user Windows DPAPI")
    def test_dpapi_key_roundtrip_no_overwrite_and_wrong_public_key(self):
        with tempfile.TemporaryDirectory(prefix="desklink-update-key-test-") as directory:
            private = Path(directory) / "release.dpapi"
            public = Path(directory) / "public.hex"
            signing.create_release_key(private, public)
            first_private = private.read_bytes()
            loaded = signing.load_release_key(private, public)
            self.assertEqual(signing.public_bytes(loaded), signing.read_public_key(public))
            self.assertNotIn(loaded.private_bytes_raw(), first_private)
            with self.assertRaisesRegex(ValueError, "already exists"):
                signing.create_release_key(private, public)
            self.assertEqual(private.read_bytes(), first_private)
            public.write_text(self.public.hex(), encoding="ascii")
            with self.assertRaisesRegex(ValueError, "does not match"):
                signing.load_release_key(private, public)


if __name__ == "__main__":
    unittest.main()
