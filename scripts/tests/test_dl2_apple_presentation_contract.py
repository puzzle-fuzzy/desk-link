"""Static end-to-end Apple epoch/presentation contracts; no GPU/device acceptance."""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]
CORE = "apps/apple/Sources/DeskLinkAppleCore/"


def read(path):
    return (ROOT / path).read_text(encoding="utf-8")


def bridge_method(name, next_name):
    text = read(CORE + "ControllerBridge.swift")
    return text.split(name, 1)[1].split(next_name, 1)[0]


class AppleInputPresentationContractTests(unittest.TestCase):
    def test_real_rust_video_config_epoch_reaches_swift_configuration(self):
        worker = read("crates/desklink-ffi/src/worker.rs")
        configuration = worker.split("ControllerEvent::VideoConfig(config) =>", 1)[1].split("ControllerEvent::H264AccessUnit", 1)[0]
        self.assertIn("input_epoch: config.input_epoch", configuration)
        self.assertIn("input_epoch: meta.input_epoch", worker)
        ffi = read("crates/desklink-ffi/src/lib.rs").split("pub struct DesklinkEvent {", 1)[1].split("}", 1)[0]
        self.assertIn("pub input_epoch: u64", ffi)
        bridge = read(CORE + "ControllerBridge.swift")
        callback = bridge.split("private func desklinkEventCallback", 1)[1]
        config = callback.split("if value.kind == DESKLINK_EVENT_VIDEO_CONFIG", 1)[1].split("if value.kind == DESKLINK_EVENT_H264_ACCESS_UNIT", 1)[0]
        self.assertIn("inputEpoch: value.input_epoch", config)
        frame = callback.split("if value.kind == DESKLINK_EVENT_H264_ACCESS_UNIT", 1)[1].split("let data = value.data.map", 1)[0]
        self.assertNotIn("value.input_epoch", frame)

    def test_decoder_freezes_epoch_with_configuration_and_emitted_pixels(self):
        scheduler = read(CORE + "H264DecodeScheduler.swift")
        self.assertIn("public let inputEpoch: UInt64", scheduler)
        self.assertIn("inputEpoch: config.inputEpoch", scheduler)
        self.assertIn("width: config.width, height: config.height", scheduler)
        self.assertIn("configuration?.inputEpoch == inputEpoch", scheduler)
        self.assertNotIn("inputEpoch >= current.inputEpoch", scheduler)
        self.assertIn("if inputEpoch == current.inputEpoch, width != current.width || height != current.height", scheduler)
        self.assertIn("inputSurfaceContractViolated = true", scheduler)
        configure = scheduler.split("public func configure(", 1)[1].split("public func receive(", 1)[0]
        self.assertIn("delivery.submit(configuration:", configure)
        self.assertIn("inputEpoch: next.inputEpoch", configure)
        self.assertIn("guard streamID > 0, inputEpoch > 0", configure)
        frame = read(CORE + "PresentedVideoFrame.swift")
        for field in ("let pixelBuffer: CVPixelBuffer", "let token: VideoFramePresentationToken", "let context: CapturedInputContext", "let frameID: UInt64"):
            self.assertIn(field, frame)

    def test_decode_and_configuration_do_not_activate_input_or_future_geometry(self):
        decoded = bridge_method("private func consumeDecodedFrame", "private func consumeDecodeKeyframeRequest")
        self.assertIn("videoFrame = PresentedVideoFrame(pixelBuffer: frame.pixelBuffer, token: token)", decoded)
        self.assertNotIn("presentedInputContext =", decoded)
        self.assertNotIn("videoSize =", decoded)
        applied = bridge_method("private func applyPendingDecodedConfiguration", "private func consumeDecodedFrame")
        self.assertNotIn("videoSize =", applied)
        config = bridge_method("private func consumeDecodedConfiguration", "private func applyPendingDecodedConfiguration")
        self.assertIn("presented.runtimeGeneration != generation || presented.streamID != streamID || presented.inputEpoch != inputEpoch", config)
        self.assertNotIn("presented.configVersion", config)
        confirmation = bridge_method("public func confirmVideoFramePresentation", "private func clearPresentedInputContext")
        self.assertIn("presentationState.confirm(token", confirmation)
        self.assertIn("inputEpoch: token.context.inputEpoch", confirmation)
        self.assertIn("presentedInputContext = token.context", confirmation)
        self.assertIn("videoSize = token.videoSize", confirmation)

    def test_metal_reports_only_actual_presented_drawables_with_immutable_token(self):
        for path in ("apps/macos/Sources/DeskLinkApp/Video/MetalVideoView.swift", "apps/ios/DeskLinkIOS/Views/IOSMetalVideoView.swift"):
            with self.subTest(path=path):
                metal = read(path)
                self.assertIn("let frame: PresentedVideoFrame?", metal)
                self.assertIn("drawable.addPresentedHandler", metal)
                self.assertIn("let presentedTime = drawable.presentedTime", metal)
                self.assertIn("guard presentedTime > 0", metal)
                self.assertIn("presentedTime >= self.lastPresentedTime", metal)
                self.assertIn("self.presentationToken?.context == token.context", metal)
                self.assertLess(metal.index("drawable.addPresentedHandler"), metal.rindex("commandBuffer.present(drawable)"))
                self.assertIn("receipt(token,", metal)
                self.assertIn("presentationToken != frame?.token", metal)
        mac = read("apps/macos/Sources/DeskLinkApp/Views/SessionView.swift")
        ios = read("apps/ios/DeskLinkIOS/Views/IOSSessionView.swift")
        self.assertIn("MetalVideoView(frame: bridge.videoFrame", mac)
        self.assertIn("confirmVideoFramePresentation(token)", mac)
        self.assertIn("frame: controller.videoFrame", ios)
        self.assertIn("confirmVideoFramePresentation(token)", ios)

    def test_input_capture_checks_callback_admission_but_old_releases_keep_their_context(self):
        capture = bridge_method("public func captureInputContext()", "public func acceptsInputContext")
        production = re.sub(r"#if DEBUG.*?#endif", "", capture, flags=re.S)
        self.assertIn("acceptsInputContext(context)", production)
        self.assertIn("decodeScheduler.acceptsInputSurface(generation: context.runtimeGeneration", production)
        self.assertIn("inputEpoch: context.inputEpoch", production)
        self.assertNotIn("context.configVersion", production)
        acceptance = bridge_method("public func acceptsInputContext", "public func confirmVideoFramePresentation")
        self.assertIn("case .connected = state, handleOwner.pointer != nil", acceptance)
        self.assertIn("context.runtimeGeneration == callbackGeneration", acceptance)
        self.assertIn("context.inputEpoch > 0", acceptance)
        self.assertNotIn("decodeScheduler.accepts", acceptance)
        self.assertNotIn("context.inputEpoch ==", acceptance)
        self.assertNotIn("context.configVersion ==", acceptance)

    def test_input_geometry_comes_from_presented_rect_and_size(self):
        mac = read("apps/macos/Sources/DeskLinkApp/Views/SessionView.swift")
        self.assertIn("visibleVideoRect: presentedVideoRect", mac)
        self.assertIn("videoPresentation.rect = rect", mac)
        ios = read("apps/ios/DeskLinkIOS/Views/IOSMetalVideoView.swift")
        callback = ios.split("drawable.addPresentedHandler", 1)[1].split("commandBuffer.present(drawable)", 1)[0]
        self.assertIn("receipt(token, presentedRect)", callback)
        self.assertLess(callback.index("receipt(token, presentedRect)"), callback.index("self.presentedVideoRect = presentedRect"))
        publisher = ios.split("private func publishVisibleVideoRect()", 1)[1]
        self.assertIn("onVisibleVideoRectChanged?(presentedVideoRect)", publisher)
        self.assertNotIn("videoSize", publisher)

    def test_draw_and_receipt_geometry_share_the_same_immutable_pixel_frame(self):
        ios = read("apps/ios/DeskLinkIOS/Views/IOSMetalVideoView.swift")
        self.assertNotIn("videoSize", ios)
        self.assertNotIn("currentVideoRect()", ios)
        draw = ios.split("override func draw(_ rect:", 1)[1].split("private func publishVisibleVideoRect", 1)[0]
        for snapshot in ("let frame = currentFrame", "let pixelBuffer = frame?.pixelBuffer", "let token = frame?.token"):
            self.assertIn(snapshot, draw)
        self.assertIn("source: image.extent.size", draw)
        self.assertIn("visibleRect = CGRect(x: renderedRect.minX / scaleX", draw)
        self.assertIn("let presentedRect = visibleRect", draw)
        self.assertIn("receipt(token, presentedRect)", draw)
        mac = read("apps/macos/Sources/DeskLinkApp/Video/MetalVideoView.swift")
        draw = mac.split("func draw(in view:", 1)[1].split("func mtkView", 1)[0]
        self.assertIn("let frame = self.frame", draw)
        self.assertIn("let token = frame?.token", draw)
        self.assertIn("guard let pixelBuffer = frame?.pixelBuffer", draw)
        self.assertIn("CGRect(x: fitted.minX * scaleX", draw)
        self.assertIn("presentedTime >= self.lastPresentedTime", draw)
        self.assertIn("self.presentationToken?.context == token.context", draw)
        self.assertNotIn("let token = presentationToken", draw)

    def test_reconnect_retires_at_ffi_ingress_and_delayed_main_event_preserves_future_work(self):
        bridge = read(CORE + "ControllerBridge.swift")
        callback = bridge.split("private func desklinkEventCallback", 1)[1]
        self.assertIn("value.kind == DESKLINK_EVENT_STATE, value.state == DESKLINK_RECONNECTING", callback)
        self.assertLess(callback.index("decodeScheduler.retireStream(generation: generation)"), callback.index("DispatchQueue.main.async"))
        scheduler = read(CORE + "H264DecodeScheduler.swift")
        retire = scheduler.split("public func retireStream", 1)[1].split("private func resetLocked", 1)[0]
        self.assertLess(retire.index("guard generation == self.generation else { return false }"), retire.index("resetLocked(generation: generation)"))
        reset = scheduler.split("private func resetLocked", 1)[1].split("public func configure", 1)[0]
        for barrier in ("epoch &+= 1", "configuration = nil", "pendingConfiguration = nil", "discardPendingFrames()", "delivery.clear()"):
            self.assertIn(barrier, reset)
        reconnect = bridge.split("case Int(DESKLINK_RECONNECTING.rawValue):", 1)[1].split("case Int(DESKLINK_DISCONNECTING.rawValue):", 1)[0]
        self.assertNotIn("decodeScheduler.reset", reconnect)
        self.assertNotIn("decodeScheduler.retireStream", reconnect)
        self.assertIn("highestStreamID = 0", reconnect)
        self.assertIn("decodeScheduler.acceptsInputSurface", reconnect)
        self.assertIn("if let pending = pendingDecodedConfiguration", reconnect)
        self.assertIn("if let pending = pendingDecodedFrame", reconnect)
        self.assertNotIn("videoFrame = nil", reconnect)
        self.assertIn("inputEpoch: activeInputEpoch) { return }", reconnect)
        connected = bridge.split("case Int(DESKLINK_CONNECTED.rawValue):", 1)[1].split("case Int(DESKLINK_CLOSED.rawValue):", 1)[0]
        self.assertIn("currentInputContext(generation: callbackGeneration)", connected)
        self.assertIn("current.map({ $0.streamID == streamID })", connected)

    def test_quality_version_preserves_surface_and_same_epoch_resize_is_rejected_in_rust_and_swift(self):
        scheduler = read(CORE + "H264DecodeScheduler.swift")
        surface = scheduler.split("public func acceptsInputSurface", 1)[1].split("public func currentInputContext", 1)[0]
        self.assertIn("configuration?.inputEpoch == inputEpoch", surface)
        self.assertNotIn("version", surface)
        exact = scheduler.split("public func accepts(generation:", 1)[1].split("public func acceptsInputSurface", 1)[0]
        self.assertIn("configuration?.version == version", exact)
        rust = read("crates/desklink-ffi/src/controller.rs")
        helper = rust.split("fn input_geometry_change_is_valid", 1)[1].split("fn next_direct_candidate_id", 1)[0]
        self.assertIn("previous.input_epoch != next.input_epoch", helper)
        self.assertIn("(previous.width, previous.height) == (next.width, next.height)", helper)
        self.assertIn("if !input_geometry_change_is_valid(current, &config)", rust)
        bridge = read(CORE + "ControllerBridge.swift")
        applied = bridge.split("private func applyPendingDecodedConfiguration", 1)[1].split("private func consumeDecodedFrame", 1)[0]
        self.assertIn("if activeInputEpoch != pending.inputEpoch { metrics.lastFrameID = nil }", applied)

    def test_reset_and_explicit_debug_fixture_cannot_be_a_production_presentation(self):
        bridge = read(CORE + "ControllerBridge.swift")
        reset = bridge_method("private func resetDisplayedSessionState()", "private func publishErrorMessage")
        self.assertIn("clearPresentedInputContext()", reset)
        self.assertIn("videoFrame = nil", reset)
        self.assertIn("decodeScheduler.reset(generation: callbackGeneration)", reset)
        clear = bridge_method("private func clearPresentedInputContext()", "public func connect(invite:")
        self.assertIn("presentedInputContextFixtureForTesting = nil", clear)
        self.assertIn("presentationState.reset()", clear)
        fixture = bridge.index("public func acceptPresentedInputContextForTesting")
        debug_region = bridge[bridge.rfind("#if DEBUG", 0, fixture):bridge.index("#endif", fixture)]
        self.assertIn("Explicit metadata fixture", debug_region)
        self.assertIn("guard handleOwner.pointer == nil", debug_region)
        state = read(CORE + "PresentedVideoFrame.swift")
        self.assertIn("token.frameID <= newestIssuedFrameID", state)
        self.assertIn("token.context == current", state)
        self.assertNotIn("token.frameID == newestIssuedFrameID", state)


if __name__ == "__main__":
    unittest.main()
