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
        self.assertIn("CMVideoFormatDescriptionGetDimensions(formatDescription)", encoder)
        self.assertIn("encodedWidth == Int32(currentWidth), encodedHeight == Int32(currentHeight)", encoder)

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


class AppleDecodeIngressContractTests(unittest.TestCase):
    def test_input_has_count_byte_and_age_budgets(self):
        scheduler = source("apps/apple/Sources/DeskLinkAppleCore/H264DecodeScheduler.swift")
        self.assertIn("frames: Int = 6", scheduler)
        self.assertIn("maximumEncodedInputBytes = 8 * 1_024 * 1_024", scheduler)
        self.assertIn("ageNanoseconds: UInt64 = 250_000_000", scheduler)
        self.assertIn("frames.count >= limits.frames", scheduler)
        self.assertIn("queuedBytes > limits.bytes - accessUnit.count", scheduler)
        self.assertIn("DispatchTime.now().uptimeNanoseconds &- next.arrivedAt > limits.ageNanoseconds", scheduler)

    def test_overflow_discards_dependency_chain_and_waits_for_idr(self):
        scheduler = source("apps/apple/Sources/DeskLinkAppleCore/H264DecodeScheduler.swift")
        receive = scheduler.split("public func receive(", 1)[1].split("public func accepts(", 1)[0]
        self.assertIn("discardPendingFrames()", receive)
        self.assertIn("awaitingKeyframe = true", receive)
        self.assertIn("if awaitingKeyframe, !isKeyframe", receive)
        self.assertIn("requestKeyframe(current)", receive)
        self.assertIn("H264AnnexB.containsIDR(in: accessUnit)", receive)
        self.assertLess(receive.index("if awaitingKeyframe, !isKeyframe"), receive.index("frames.append("))

    def test_native_decode_has_serial_owner_and_one_inflight_submission(self):
        decoder = source("apps/apple/Sources/DeskLinkAppleCore/H264Decoder.swift")
        scheduler = source("apps/apple/Sources/DeskLinkAppleCore/H264DecodeScheduler.swift")
        self.assertIn("final class H264DecodeSession: @unchecked Sendable", decoder)
        native = decoder.split("final class H264DecodeSession", 1)[1].split("private func h264OutputCallback", 1)[0]
        self.assertNotIn("DispatchQueue.main", native)
        self.assertIn("VTDecompressionSessionWaitForAsynchronousFrames(session)", native)
        self.assertIn("private let outputHandlerLock = NSLock()", native)
        self.assertIn("let handler = onFrame", native)
        self.assertIn("outputHandlerLock.lock()", native)
        self.assertIn("private let decoder = H264DecodeSession()", scheduler)
        self.assertIn("guard !draining else { return }", scheduler)
        self.assertIn("queue.async { [weak self] in self?.drain() }", scheduler)
        self.assertIn("let shouldSchedule = !scheduled", scheduler)
        self.assertIn("guard shouldSchedule else { return }", scheduler)

    def test_production_ffi_ingress_is_bounded_before_main_queue_and_copy(self):
        bridge = source("apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift")
        callback = bridge.split("private func desklinkEventCallback", 1)[1]
        self.assertLess(callback.index("callbackContext.decodeScheduler.configure("), callback.index("DispatchQueue.main.async"))
        self.assertLess(callback.index("callbackContext.decodeScheduler.receive("), callback.index("DispatchQueue.main.async"))
        configuration = callback.split("if value.kind == DESKLINK_EVENT_VIDEO_CONFIG", 1)[1].split("if value.kind == DESKLINK_EVENT_H264_ACCESS_UNIT", 1)[0]
        frame = callback.split("if value.kind == DESKLINK_EVENT_H264_ACCESS_UNIT", 1)[1].split("let data = value.data.map", 1)[0]
        self.assertLess(configuration.index("maximumSequenceHeaderBytes"), configuration.index("Data(bytes:"))
        self.assertLess(frame.index("maximumEncodedInputBytes"), frame.index("Data(bytes:"))
        self.assertIn("accessUnit = Data()", frame)
        self.assertNotIn("decoder.receive(", bridge)
        self.assertNotIn("decoder.configure(", bridge)

    def test_ui_delivery_rechecks_generation_stream_and_configuration(self):
        bridge = source("apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift")
        consumer = bridge.split("private func consumeDecodedFrame", 1)[1].split("private func requestDecodeKeyframe", 1)[0]
        self.assertIn("acceptsCallback(generation: frame.generation)", consumer)
        self.assertIn("activeStreamID == frame.streamID", consumer)
        self.assertIn("activeConfigVersion == frame.version", consumer)
        self.assertIn("decodeScheduler.accepts(generation:", consumer)
        self.assertNotIn("latestAccessUnit =", consumer)
        connected = bridge.split("case Int(DESKLINK_CONNECTED.rawValue):", 1)[1].split("case Int(DESKLINK_CLOSED.rawValue):", 1)[0]
        self.assertNotIn("decodeScheduler.reset(", connected)

    def test_configuration_and_recovery_requests_wait_for_connected_without_fifo_assumptions(self):
        bridge = source("apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift")
        config = bridge.split("private func consumeDecodedConfiguration", 1)[1].split("private func applyPendingDecodedConfiguration", 1)[0]
        self.assertIn("pendingDecodedConfiguration = DecodedConfiguration(", config)
        self.assertNotIn("activeStreamID == streamID", config)
        connected = bridge.split("case Int(DESKLINK_CONNECTED.rawValue):", 1)[1].split("case Int(DESKLINK_CLOSED.rawValue):", 1)[0]
        self.assertIn("applyPendingDecodedConfiguration()", connected)
        self.assertIn("pendingDecodedFrame = frame", bridge)
        self.assertIn("pendingDecodeKeyframeRequest = (generation, streamID)", bridge)
        self.assertIn("flushPendingDecodeKeyframeRequest()", bridge)
        reset = bridge.split("private func resetDisplayedSessionState()", 1)[1].split("private func publishErrorMessage", 1)[0]
        for field in ("pendingDecodedConfiguration", "pendingDecodedFrame", "pendingDecodeKeyframeRequest"):
            self.assertIn(field + " = nil", reset)

    def test_annexb_conversion_uses_source_ranges(self):
        annexb = source("apps/apple/Sources/DeskLinkAppleCore/H264AnnexB.swift")
        conversion = annexb.split("public static func avccAccessUnit", 1)[1].split("public static func nalUnits", 1)[0]
        self.assertIn("nalRanges(in: annexB)", conversion)
        self.assertIn("output.append(contentsOf: bytes[range])", conversion)
        self.assertNotIn("nalUnits(in:", conversion)
        self.assertNotIn("[UInt8](annexB)", annexb)


if __name__ == "__main__":
    unittest.main()
