"""Cross-language event boundary checks; native AppKit behavior needs macOS."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


class HostEventContractTests(unittest.TestCase):
    def test_transfer_id_is_shared_and_unknown_has_a_nonfatal_counted_path(self):
        rust = (ROOT / "crates/desklink-ffi/src/lib.rs").read_text(encoding="utf-8")
        enum = rust.split("pub enum DesklinkHostEventKind", 1)[1].split("}", 1)[0]
        self.assertRegex(enum, r"Transfer\s*=\s*8")
        for relative in ("crates/desklink-ffi/include/desklink.h", "apps/apple/Sources/DeskLinkC/include/desklink.h"):
            self.assertIn("DESKLINK_HOST_EVENT_TRANSFER = 8", (ROOT / relative).read_text(encoding="utf-8"))
        host = (ROOT / "apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift").read_text(encoding="utf-8")
        unknown = host.split("case .unknown:", 1)[1].split("}", 1)[0]
        self.assertIn("ignoredHostEventCount &+= 1", unknown)
        self.assertNotIn("publishError", unknown)
        self.assertIn("case DESKLINK_HOST_EVENT_TRANSFER: .transfer", host)
        self.assertIn("default: .unknown", host)

    def test_only_explicit_error_kind_interprets_payload_as_error_text(self):
        host = (ROOT / "apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift").read_text(encoding="utf-8")
        callback = host.split("private func desklinkHostEventCallback", 1)[1].split("private extension", 1)[0]
        mapper = host.split("private func hostCallbackEvent(", 1)[1].split("private extension", 1)[0]
        self.assertRegex(mapper, r"if case \.error = kind\s*\{\s*message = value\.data")
        self.assertRegex(mapper, r"else\s*\{[^}]*message = nil")
        self.assertLess(callback.index("acceptsCallback(generation: generation)"), callback.index("bridge.consume(mapped)"))


if __name__ == "__main__":
    unittest.main()
