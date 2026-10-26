"""Portable cursor fixtures and source wiring checks; does not run SwiftUI."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
CORE = ROOT / "apps/apple/Sources/DeskLinkAppleCore"
SWIFT_TESTS = ROOT / "apps/apple/Tests/DeskLinkAppleCoreTests/RemoteCursorPositionTests.swift"


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def fixture(name: str) -> bytes:
    text = re.search(rf"let {name}: \[UInt8\] = \[([^]]+)\]", read(SWIFT_TESTS)).group(1)
    return bytes(int(value.strip()) for value in text.split(","))


def reference_decode(data: bytes) -> tuple:
    """Independent postcard wire oracle for the checked Rust/Swift fixtures."""
    if not 0 < len(data) <= 256:
        raise ValueError("size")
    offset = 0

    def unsigned(bits):
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

    version, stream, sequence, timestamp = unsigned(16), unsigned(64), unsigned(64), unsigned(64)
    x, y = unsigned(32), unsigned(32)
    x, y = (x >> 1) ^ -(x & 1), (y >> 1) ^ -(y & 1)
    visible, shape = unsigned(1), unsigned(64)
    if version != 12 or not stream or not sequence or not 0 <= x <= 1_000_000 or not 0 <= y <= 1_000_000 or offset != len(data):
        raise ValueError("invalid")
    return stream, sequence, timestamp, x, y, bool(visible), shape


class AppleCursorContractTests(unittest.TestCase):
    def test_position_fixtures_cover_visibility_bounds_and_opaque_handle(self):
        self.assertEqual(reference_decode(fixture("visibleFixture")), (7, 11, 42, 250000, 750000, True, 99))
        self.assertEqual(reference_decode(fixture("hiddenFixture")), (7, 12, 43, 0, 1000000, False, 2**64 - 1))
        self.assertEqual(reference_decode(fixture("cornerFixture")), (8, 1, 0, 1000000, 0, True, 0))

    def test_protocol_fixture_gate_uses_the_actual_rust_codec(self):
        rust = read(ROOT / "crates/desklink-protocol/tests/apple_cursor_contract.rs")
        self.assertIn("encode_cursor_update(&update)", rust)
        self.assertIn("decode_cursor_update(&fixture)", rust)
        self.assertIn("RemoteCursorPositionTests.swift", rust)
        protocol = read(ROOT / "crates/desklink-protocol/src/lib.rs")
        swift = read(CORE / "RemoteCursorPosition.swift")
        version = re.search(r"PROTOCOL_VERSION: u16 = (\d+)", protocol).group(1)
        self.assertIn(f"wireProtocolVersion: UInt16 = {version}", swift)

    def test_bad_size_truncated_overflow_and_trailing_data_are_rejected(self):
        valid = fixture("visibleFixture")
        invalid = [valid[:length] for length in range(len(valid))]
        invalid.extend([valid + b"\0", bytes(257), b"\x0b" + valid[1:]])
        invalid.extend([valid[:10] + b"\x02" + valid[11:], fixture("hiddenFixture")[:-1] + b"\x02"])
        for encoded in invalid:
            with self.subTest(encoded=encoded), self.assertRaises(ValueError):
                reference_decode(encoded)
        swift = read(CORE / "RemoteCursorPosition.swift")
        for guard in ("encoded.count <= 256", "reader.isAtEnd", "streamID > 0", "sequence > 0", "unsigned(bits: 1)", "(0...1_000_000).contains(x)"):
            self.assertIn(guard, swift)

    def test_stream_and_sequence_guards_prevent_marker_resurrection(self):
        swift = read(CORE / "RemoteCursorPosition.swift")
        for guard in ("streamID != activeStreamID || overlay == nil", "overlay.streamID == activeStreamID", "update.streamID == activeStreamID", "update.sequence > (latest?.sequence ?? 0)"):
            self.assertIn(guard, swift)
        self.assertIn("latest = nil", swift)
        layer = read(CORE / "RemoteCursorPositionLayer.swift")
        self.assertIn(".onChange(of: activeStreamID)", layer)
        self.assertIn(".allowsHitTesting(false)", layer)
        self.assertIn("cursor.streamID == activeStreamID", layer)

    def test_platforms_use_the_actual_video_geometry_for_position_layer(self):
        mac = read(ROOT / "apps/macos/Sources/DeskLinkApp/Views/SessionView.swift")
        ios = read(ROOT / "apps/ios/DeskLinkIOS/Views/IOSSessionView.swift")
        for text in (mac, ios):
            self.assertIn("RemoteCursorPositionLayer(", text)
            self.assertIn("activeStreamID: cursorStreamID", text)
            self.assertRegex(text, r"if case let \.connected\(streamID\)")
            self.assertRegex(text, r"state == \.frozen \|\| (?:bridge|controller).state == \.recovering")
        self.assertIn("VideoGeometry.aspectFit(", mac)
        self.assertIn("videoRect: visibleVideoRect", ios)
        swift = read(CORE / "RemoteCursorPosition.swift")
        self.assertIn("videoRect.minX + CGFloat(xMillionths) / 1_000_000 * videoRect.width", swift)
        self.assertIn("videoRect.minY + CGFloat(yMillionths) / 1_000_000 * videoRect.height", swift)

    def test_cursor_only_updates_do_not_force_metal_redraw(self):
        mac = read(ROOT / "apps/macos/Sources/DeskLinkApp/Video/MetalVideoView.swift")
        ios = read(ROOT / "apps/ios/DeskLinkIOS/Views/IOSMetalVideoView.swift")
        self.assertIn("guard context.coordinator.pixelBuffer !== pixelBuffer else { return }", mac)
        self.assertIn("guard self.pixelBuffer !== pixelBuffer || self.videoSize != videoSize", ios)
        self.assertIn("self.zoomScale != zoomScale || self.panOffset != panOffset", ios)
        self.assertIn("drawableSizeWillChange", mac)
        self.assertIn("guard bounds != lastLayoutBounds else { return }", ios)
        self.assertIn("lastLayoutBounds = bounds\n        publishVisibleVideoRect()\n        setNeedsDisplay()", ios)


if __name__ == "__main__":
    unittest.main()
