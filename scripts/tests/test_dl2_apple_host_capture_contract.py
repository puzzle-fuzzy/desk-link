"""Portable source contracts for Host capture ownership, not Swift/GPU proof.

The behavioral task-order tests are in HostBridgeTests and require macOS.
These checks protect the cross-thread admission and scoped C API boundaries
that previously let retired capture events enter a successor Noise session.
"""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


def source(relative):
    return (ROOT / relative).read_text(encoding="utf-8")


def body(text, signature):
    """Extract a Swift declaration's balanced body without line-number ties."""
    start = text.index(signature)
    parentheses = 0
    while text[start] != "{" or parentheses:
        parentheses += (text[start] == "(") - (text[start] == ")")
        start += 1
    depth = 1
    cursor = start + 1
    while depth:
        char = text[cursor]
        depth += (char == "{") - (char == "}")
        cursor += 1
    return text[start + 1:cursor - 1]


class HostCaptureContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.bridge = source("apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift")
        cls.lifecycle = source("apps/macos/Sources/DeskLinkApp/Bridge/HostCaptureLifecycle.swift")
        cls.swift_tests = source("apps/macos/Tests/DeskLinkAppTests/HostBridgeTests.swift")

    def test_native_callback_retires_before_ui_dispatch_and_ui_checks_receipt(self):
        callback = body(self.bridge, "private func desklinkHostEventCallback(")
        self.assertLess(callback.index("captureAdmission.observe("), callback.index("DispatchQueue.main.async"))
        self.assertLess(callback.index("captureAdmission.matches(receipt)"), callback.index("bridge.consume(mapped)"))
        self.assertIn("mediaGeneration: value.media_generation", callback)
        context = body(self.bridge, "private final class HostCallbackContext")
        self.assertIn("let captureAdmission: HostCaptureAdmission", context)
        consume = body(self.bridge, "private func consumeState(")
        self.assertIn("admission.matches(receipt)", consume)
        self.assertIn("receipt.runtimeGeneration == callbackGeneration", consume)

    def test_owner_is_immutable_and_bound_when_callbacks_are_created(self):
        owner = body(self.lifecycle, "struct HostCaptureOwner:")
        for field in ("runtimeGeneration", "connectionSequence", "mediaGeneration", "streamID"):
            self.assertIn(f"let {field}: UInt64", owner)
        start = body(self.bridge, "private func startCaptureIfPermitted()")
        self.assertIn("[weak self, owner, admission]", start)
        self.assertIn("sendVideo(event, owner: owner, admission: admission)", start)
        self.assertIn("session: captureFactory(owner, callbacks)", start)
        self.assertLess(start.index("nextStreamID += 1"), start.index("admission.register("))
        self.assertLess(start.index("nextStreamID += 1"), start.index("await attempt.session.start()"))
        self.assertIn("nextStreamID < UInt64.max", start)
        self.assertNotIn("nextStreamID &+=", self.bridge)

    def test_final_submit_uses_captured_rust_generation_and_original_payload(self):
        submit = body(self.bridge, "private func sendVideo(")
        self.assertIn("guard eventStream == owner.streamID", submit)
        self.assertIn("admission.withAdmission(owner)", submit)
        for function in ("config", "access_unit"):
            self.assertRegex(submit, rf"desklink_host_send_video_{function}_for_generation\(handle, owner\.mediaGeneration,")
            self.assertNotIn(f"desklink_host_send_video_{function}(", self.bridge)
        self.assertIn("streamID, version, width, height", submit)
        self.assertIn("streamID, frameID, configVersion", submit)
        self.assertIn("result != DESKLINK_STALE_GENERATION", submit)
        self.assertIn("if result != DESKLINK_OK", submit)
        self.assertIn("lastError =", submit)
        self.assertNotIn("connectionReceipt", submit)  # Never relabel at send time.

    def test_scoped_api_matches_generated_c_generation_contract(self):
        headers = [source(path) for path in (
            "crates/desklink-ffi/include/desklink.h",
            "apps/apple/Sources/DeskLinkC/include/desklink.h",
        )]
        self.assertEqual(*headers)
        header = headers[0]
        self.assertRegex(header, r"DESKLINK_STALE_GENERATION\s*=\s*5")
        self.assertIn("uint64_t media_generation;", header)
        for function in ("config", "access_unit"):
            self.assertRegex(header, rf"desklink_host_send_video_{function}_for_generation\(DesklinkHostHandle \* handle, uint64_t media_generation, uint64_t stream_id,")
        rust = source("crates/desklink-ffi/src/lib.rs")
        self.assertRegex(rust, r"HostEvent::Connected\s*\{\s*media_generation\s*\}\s*=>\s*\{[^}]*c_event\.media_generation = media_generation;")

    def test_each_attempt_has_independent_native_resources_and_cleanup_barrier(self):
        native = body(self.lifecycle, "final class NativeHostCaptureSession:")
        self.assertIn("private let source = ScreenCaptureSource()", native)
        self.assertIn("private let encoder = MacH264Encoder()", native)
        initializer = body(native, "init(owner:")
        self.assertIn("encoder.onEvent = callbacks.video", initializer)
        self.assertIn("encoder.onError = callbacks.encodingError", initializer)
        self.assertNotIn("encoder.onEvent", self.bridge)
        start = body(self.bridge, "private func startCaptureIfPermitted()")
        self.assertLess(start.index("await previousCleanup.value"), start.index("await attempt.session.start()"))
        cleanup = body(self.bridge, "private func retireActiveCapture()")
        self.assertLess(cleanup.index("activeCapture = nil"), cleanup.index("await"))
        self.assertIn("attempt.startTask?.cancel()", cleanup)
        self.assertLess(cleanup.index("await starting.value"), cleanup.index("await attempt.session.stop()"))
        self.assertNotIn("captureRunning =", cleanup[cleanup.index("await"):])
        self.assertNotIn("captureStarting =", cleanup[cleanup.index("await"):])

    def test_old_cleanup_cannot_retire_a_successor_and_start_failure_can_retry(self):
        retire = body(self.bridge, "private func retireCaptureIfOwned(")
        self.assertIn("activeCapture?.owner == owner", retire)
        gate_retire = body(self.lifecycle, "func retire(_ owner:")
        self.assertIn("capture == owner", gate_retire)
        start = body(self.bridge, "private func startCaptureIfPermitted()")
        catch = start[start.index("} catch {"):]
        self.assertLess(catch.index("guard isCurrentCapture("), catch.index("lastError ="))
        self.assertLess(catch.index("let wasCancelled"), catch.index("retireActiveCapture()"))
        self.assertIn("if wasCancelled { return }", catch)
        self.assertNotIn("publishError(", catch)
        self.assertNotRegex(catch, r"\bstate\s*=|\bstop\(\)")
        refresh = body(self.bridge, "func refreshPermissions()")
        apply_permissions = body(self.bridge, "private func applyPermissions(")
        self.assertIn("applyPermissions(", refresh)
        self.assertIn("startCaptureIfPermitted()", apply_permissions)

    def test_capture_owned_errors_do_not_stop_or_fail_the_runtime(self):
        for signature in ("private func captureStoppedUnexpectedly(", "private func encodingFailed(", "private func sendVideo("):
            path = body(self.bridge, signature)
            self.assertIn("isCurrentCapture(owner, admission: admission)", path)
            self.assertIn("retireActiveCapture()", path)
            self.assertIn("lastError =", path)
            self.assertNotIn("publishError(", path)
            self.assertNotRegex(path, r"\bstate\s*=|\bstop\(\)|desklink_host_(stop|destroy)\(")
        # Native runtime failures retain the established terminal policy.
        consume = body(self.bridge, "fileprivate func consume(")
        self.assertRegex(consume, r"case \.error:\s*publishError\(")

    def test_synthetic_regressions_do_not_use_real_capture_or_permission_grants(self):
        testing = body(self.bridge, "static func testing(")
        self.assertIn("StaticMacPermissionProvider(", testing)
        self.assertIn("loadTrustedControllers: false", testing)
        inert = body(self.lifecycle, "final class InertHostCaptureSession:")
        self.assertNotIn("ScreenCaptureSource", inert)
        self.assertNotIn("MacH264Encoder", inert)
        self.assertIn("CaptureTestFactory", self.swift_tests)
        self.assertIn("testNativeGateRetiresOldDeliveryBeforeItsMainActorStateNotification", self.swift_tests)
        self.assertIn("testNewCaptureWaitsForCancelledStartAndCleanupBarrier", self.swift_tests)
        self.assertIn("testLateStartFailureCannotClearSuccessorFlagsOrPublishError", self.swift_tests)
        self.assertIn("testRustStaleGenerationResponseIsSilentWhileOtherSubmitErrorsAreRetryable", self.swift_tests)
        self.assertNotRegex(self.swift_tests, r"\.request(ScreenRecording|Accessibility)\(|\.inject\(|\.startCapture\(")


if __name__ == "__main__":
    unittest.main()
