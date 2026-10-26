"""Portable protocol fixtures and source contracts; does not run SwiftUI/peers."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
CORE = ROOT / "apps/apple/Sources/DeskLinkAppleCore"
SWIFT_TESTS = ROOT / "apps/apple/Tests/DeskLinkAppleCoreTests/SessionControlStateTests.swift"


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def fixture(name: str) -> bytes:
    match = re.search(rf"let {name}: \[UInt8\] = \[([^]]+)\]", read(SWIFT_TESTS))
    return bytes(int(value.strip()) for value in match.group(1).split(","))


def reference_decode(data: bytes) -> tuple:
    """Wire oracle for fixtures whose encoding is checked by the Rust codec."""
    if not 0 < len(data) <= 256:
        raise ValueError("size")
    offset = 0

    def unsigned(bits: int) -> int:
        nonlocal offset
        value = 0
        for index in range((bits + 6) // 7):
            if offset >= len(data):
                raise ValueError("truncated")
            byte = data[offset]
            offset += 1
            value |= (byte & 127) << (index * 7)
            if value >= 1 << bits:
                raise ValueError("overflow")
            if byte < 128:
                if index and byte == 0:
                    raise ValueError("noncanonical")
                return value
        raise ValueError("overflow")

    kind = unsigned(32)
    if kind == 4:
        count = unsigned(32)
        if not 1 <= count <= 16:
            raise ValueError("count")
        displays, identifiers = [], set()
        for _ in range(count):
            identifier, width, height, primary = unsigned(32), unsigned(16), unsigned(16), unsigned(1)
            if not width or not height or identifier in identifiers:
                raise ValueError("display")
            identifiers.add(identifier)
            displays.append((identifier, width, height, bool(primary)))
        active = unsigned(32)
        if active not in identifiers:
            raise ValueError("active")
        result = "displays", displays, active
    elif kind == 7:
        result = "audio", bool(unsigned(1)), bool(unsigned(1))
    elif kind == 10:
        preference, preset = unsigned(32), unsigned(32)
        if preference > 3 or preset > 2:
            raise ValueError("quality")
        result = "quality", preference, preset
    elif kind == 14:
        reason = unsigned(32) if unsigned(1) else None
        if reason is not None and reason > 2:
            raise ValueError("reason")
        result = "input", reason
    else:
        raise ValueError("unknown")
    if offset != len(data):
        raise ValueError("trailing")
    return result


class MacSessionContractTests(unittest.TestCase):
    def test_exact_fixtures_cover_displays_quality_audio_and_input_recovery(self):
        self.assertEqual(reference_decode(fixture("displaysFixture")), ("displays", [(7, 1920, 1080, True), (300, 2560, 1440, False)], 300))
        self.assertEqual(reference_decode(fixture("automaticQualityFixture")), ("quality", 0, 1))
        self.assertEqual(reference_decode(fixture("sharpQualityFixture")), ("quality", 3, 2))
        self.assertEqual(reference_decode(fixture("audioFixture")), ("audio", True, False))
        self.assertEqual(reference_decode(fixture("inputBlockedFixture")), ("input", 0))
        self.assertEqual(reference_decode(fixture("inputRecoveredFixture")), ("input", None))

    def test_truncation_trailing_noncanonical_overflow_and_bad_values(self):
        for name in ["displaysFixture", "automaticQualityFixture", "sharpQualityFixture", "audioFixture", "inputBlockedFixture", "inputRecoveredFixture"]:
            valid = fixture(name)
            invalid = [valid[:length] for length in range(len(valid))] + [valid + b"\0"]
            for encoded in invalid:
                with self.subTest(name=name, encoded=encoded), self.assertRaises(ValueError):
                    reference_decode(encoded)
        for encoded in [bytes(257), b"\x04\0\0", b"\x04\x11", bytes([4, 1, 1, 0, 1, 1, 1]), bytes([4, 2, 1, 1, 1, 0, 1, 1, 1, 0, 1]), bytes([4, 1, 1, 1, 1, 0, 2]), bytes([4, 1, 255, 255, 255, 255, 31]), bytes([4, 1, 0, 128, 128, 4]), bytes([132, 0, 1, 1, 1, 1, 0, 1]), bytes([7, 2, 0]), bytes([10, 4, 1]), bytes([10, 0, 3]), bytes([14, 2]), bytes([14, 1, 3])]:
            with self.subTest(encoded=encoded), self.assertRaises(ValueError):
                reference_decode(encoded)

    def test_swift_reader_is_bounded_canonical_and_validates_display_identity(self):
        swift = read(CORE / "SessionControlState.swift")
        for guard in ["encoded.count <= 256", "(1...16).contains(count)", "width > 0", "height > 0", "identifiers.insert(UInt32(rawID)).inserted", "identifiers.contains(UInt32(active))", "reader.isAtEnd", "index == 0 || payload != 0", "payload < (UInt64(1) << remaining)"]:
            self.assertIn(guard, swift)

    def test_fixtures_are_verified_with_actual_rust_control_codec(self):
        rust = read(ROOT / "crates/desklink-protocol/tests/apple_session_control_contract.rs")
        for token in ["encode_control(&message)", "decode_control(&bytes)", "SessionControlStateTests.swift", "ControlMessage::InputState", "ControlMessage::DisplayList", "ControlMessage::VideoQualityState"]:
            self.assertIn(token, rust)

    def test_production_menus_use_bounded_ffi_and_remote_acknowledgement(self):
        view = read(ROOT / "apps/macos/Sources/DeskLinkApp/Views/SessionView.swift")
        for token in ["bridge.capabilities != nil", "bridge.sendSessionCommand(3, value: preference.rawValue)", "bridge.sendSessionCommand(1, value: identifier)", "bridge.qualityStateRevision", "bridge.displayStateRevision", "operations.qualityConfirmed(bridge.confirmedQuality)", "operations.displayConfirmed(bridge.activeDisplayID)", "Task.sleep(for: .seconds(5))", "bridge.inputRestriction", "bridge.lastError"]:
            self.assertIn(token, view)
        self.assertNotIn("bridge.confirmedQuality =", view)
        self.assertNotIn("bridge.activeDisplayID =", view)

    def test_queued_status_never_claims_remote_success(self):
        status = read(ROOT / "apps/macos/Sources/DeskLinkApp/Views/SessionOperationStatus.swift")
        queued = status.split("mutating func qualityQueued", 1)[1].split("mutating func qualityConfirmed", 1)[0]
        self.assertIn("等待远端确认", queued)
        self.assertNotIn("已确认", queued)
        self.assertIn("quality.preference == requested", status)
        self.assertIn("requested == identifier", status)
        self.assertIn("qualityTimedOut()", status)
        self.assertIn("displayTimedOut()", status)


if __name__ == "__main__":
    unittest.main()
