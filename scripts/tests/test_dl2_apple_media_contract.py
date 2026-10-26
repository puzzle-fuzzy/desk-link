"""Cross-platform checks for Apple media contracts; these do not run VideoToolbox."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


def source(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


class MacHostVideoContractTests(unittest.TestCase):
    def test_capture_dimensions_match_ffi_advertisement(self):
        encoder = source("apps/macos/Sources/DeskLinkApp/Capture/MacH264Encoder.swift")
        host = source("crates/desklink-ffi/src/host_worker.rs")
        capture = source("apps/macos/Sources/DeskLinkApp/Capture/ScreenCaptureSource.swift")
        for swift_name, rust_name in (("maximumWidth", "width"), ("maximumHeight", "height")):
            value = int(re.search(rf"static let {swift_name} = ([\d_]+)", encoder)[1].replace("_", ""))
            declaration = re.search(r"ControlMessage::Capabilities\(DeviceCapabilities \{(.*?)\}\)", host, re.S)[1]
            self.assertRegex(declaration, rf"{rust_name}: {value},")
            self.assertIn(f"{swift_name}: Int = MacHostVideoContract.{swift_name}", capture)
        self.assertIn("MacHostVideoContract.supports(width: width, height: height)", encoder)

    def test_encoder_checks_output_profile_before_event_construction(self):
        encoder = source("apps/macos/Sources/DeskLinkApp/Capture/MacH264Encoder.swift")
        host = source("crates/desklink-ffi/src/host_worker.rs")
        self.assertIn("h264_profiles: vec![H264Profile::Main]", host)
        self.assertIn("kVTProfileLevel_H264_Main_AutoLevel", encoder)
        self.assertNotIn("kVTProfileLevel_H264_High_AutoLevel", encoder)
        self.assertIn("static let profileIDC: UInt8 = 77", encoder)
        profile_check = encoder.index("throw MacH264EncoderError.unsupportedOutputProfile")
        self.assertLess(profile_check, encoder.index("output.append(.configuration("))
        self.assertIn("format.sps.startIndex + 1] == MacHostVideoContract.profileIDC", encoder)


if __name__ == "__main__":
    unittest.main()
