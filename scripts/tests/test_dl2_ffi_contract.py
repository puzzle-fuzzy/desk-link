from pathlib import Path
import importlib.util
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("ffi_header", ROOT / "scripts/sync-ffi-header.py")
header = importlib.util.module_from_spec(spec)
spec.loader.exec_module(header)


class FFIContractTests(unittest.TestCase):
    def test_rust_is_the_source_of_both_headers(self):
        expected = header.generate(header.SOURCE.read_text(encoding="utf-8")).encode()
        for path in header.HEADERS:
            self.assertEqual(path.read_bytes(), expected)

    def test_new_rust_field_and_export_appear_without_manual_header_edit(self):
        source = header.SOURCE.read_text(encoding="utf-8")
        changed = source.replace("pub recoverable: u8,", "pub recoverable: u8,\n    pub test_generation: u64,")
        changed += '\npub unsafe extern "C" fn desklink_test_new(value: u32) -> DesklinkResult { unimplemented!() }'
        generated = header.generate(changed)
        self.assertIn("uint64_t test_generation;", generated)
        self.assertIn("DesklinkResult desklink_test_new(uint32_t value);", generated)

    def test_unknown_abi_type_cannot_be_silently_generated(self):
        with self.assertRaises(ValueError):
            header.ctype("Vec<u8>")

    def test_swift_consumes_each_runtime_capability_event_explicitly(self):
        source = (ROOT / "apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift").read_text(encoding="utf-8")
        for value in (4, 11, 12, 13):
            self.assertIn(f"case {value}:", source)
        self.assertIn("SessionCapabilities(encoded: data)", source)
        self.assertIn("remotePlatform = .unknown", source)

    def test_protocol_version_is_generated_for_swift_and_c(self):
        version = header.PROTOCOL_VERSION
        self.assertIn(f"deskLinkWireProtocolVersion: UInt16 = {version}", header.SWIFT_VERSION.read_text(encoding="utf-8"))
        self.assertIn(f"#define DESKLINK_PROTOCOL_VERSION {version}", header.HEADERS[0].read_text(encoding="utf-8"))
        events = (ROOT / "apps/apple/Sources/DeskLinkAppleCore/DeskLinkEvents.swift").read_text(encoding="utf-8")
        self.assertIn("var cValue: UInt32", events)

    def test_known_host_transfer_cannot_be_classified_as_fatal_error(self):
        host = (ROOT / "apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift").read_text(encoding="utf-8")
        self.assertIn("case DESKLINK_HOST_EVENT_TRANSFER: .transfer", host)
        self.assertIn("default: .unknown", host)

    def test_ffi_direct_probes_never_use_the_exclusive_crypto_api(self):
        controller = (ROOT / "crates/desklink-ffi/src/controller.rs").read_text(encoding="utf-8")
        production = controller.split("#[cfg(test)]", 1)[0]
        self.assertNotIn(".accept_probe_connection(", production)
        self.assertIn(".accept_probe_connection_shared(", production)
        self.assertIn(".connect_shared(&candidate, &binding, &secure, now_unix_s())", production)


if __name__ == "__main__":
    unittest.main()
