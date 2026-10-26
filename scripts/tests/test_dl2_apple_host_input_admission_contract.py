"""Windows source contracts for the native-to-MainActor Host input boundary.

Swift scheduling tests use synthetic C events and a CGEventBackend recorder;
neither this suite nor those fixtures prove real macOS input/capture behavior.
"""
from pathlib import Path
import re
import unittest

ROOT = Path(__file__).resolve().parents[2]


def source(relative):
    return (ROOT / relative).read_text(encoding="utf-8")


def body(text, signature):
    start = text.index(signature)
    parentheses = 0
    while text[start] != "{" or parentheses:
        parentheses += (text[start] == "(") - (text[start] == ")")
        start += 1
    depth = 1
    cursor = start + 1
    while depth:
        depth += (text[cursor] == "{") - (text[cursor] == "}")
        cursor += 1
    return text[start + 1:cursor - 1]


class HostInputAdmissionContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.bridge = source("apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift")
        cls.gate = source("apps/macos/Sources/DeskLinkApp/Bridge/HostCaptureLifecycle.swift")
        cls.tests = source("apps/macos/Tests/DeskLinkAppTests/HostInputAdmissionTests.swift")

    def test_generated_c_retains_original_input_owner_and_context(self):
        rust = source("crates/desklink-ffi/src/lib.rs")
        host = source("crates/desklink-ffi/src/host.rs")
        self.assertRegex(host, r"Input\s*\{\s*media_generation:\s*u64,\s*input:\s*InputEnvelope")
        headers = [source(path) for path in (
            "crates/desklink-ffi/include/desklink.h", "apps/apple/Sources/DeskLinkC/include/desklink.h")]
        self.assertEqual(*headers)
        c_input = body(headers[0], "typedef struct DesklinkHostInput")
        self.assertRegex(c_input, r"uint64_t stream_id;\s*uint64_t input_epoch;")
        # The C conversion copies its event payload rather than querying a
        # current runtime/Noise when the callback queue eventually drains.
        input_case = rust.split("HostEvent::Input {", 1)[1].split("HostEvent::", 1)[0]
        self.assertIn("c_event.media_generation = media_generation", input_case)
        self.assertIn("convert_host_input(input.event)", input_case)
        self.assertIn("c_event.input.stream_id = input.stream_id", input_case)
        self.assertIn("c_event.input.input_epoch = input.input_epoch", input_case)

    def test_input_preparation_is_the_production_native_scheduling_path(self):
        native = body(self.bridge, "private func desklinkHostEventCallback(")
        fixture = body(self.bridge, "func enqueueNativeInputForTesting(")
        self.assertIn("scheduleHostCallbackEvent(callbackContext, event.pointee)", native)
        self.assertIn("scheduleHostCallbackEvent(callbackContextForTesting(), event)", fixture)
        schedule = body(self.bridge, "private func scheduleHostCallbackEvent(")
        input_case = schedule.split("case .input:", 1)[1].split("default:", 1)[0]
        self.assertIn("captureAdmission.captureInput(", input_case)
        self.assertIn("mediaGeneration: value.media_generation", input_case)
        self.assertIn("HostInputContext(streamID: value.input.stream_id, inputEpoch: value.input.input_epoch)", input_case)
        self.assertNotIn(".observe(", input_case)
        self.assertNotIn(".register(", input_case)
        self.assertLess(schedule.index("captureAdmission.captureInput("), schedule.index("DispatchQueue.main.async"))
        self.assertLess(schedule.index("captureAdmission.matches(receipt)"), schedule.index("bridge.consume(mapped)"))

    def test_snapshot_checks_existing_grant_without_learning_a_current_epoch(self):
        capture = body(self.gate, "func captureInput(")
        for check in ("permits(capture)", "mediaGeneration == capture.mediaGeneration",
                      "context.streamID == capture.streamID", "context.inputEpoch > 0"):
            self.assertIn(check, capture)
        self.assertIn("mediaGeneration: mediaGeneration, context: context, command: command", capture)
        self.assertNotRegex(capture, r"\b(phase|capture|greatestMediaGeneration|sequence)\s*=(?!=)")
        self.assertNotIn("observe(", capture)
        delivery = body(self.gate, "struct HostInputDelivery:")
        for field in ("admission", "connectionReceipt", "captureOwner", "mediaGeneration", "context", "command"):
            self.assertRegex(delivery, rf"\blet {field}:")
        self.assertNotIn("var ", delivery)

    def test_final_injection_uses_short_gate_and_current_capture_geometry(self):
        inject = body(self.bridge, "private func inject(_ input: HostInputDelivery)")
        for check in ("canInjectInput", "captureRunning", "captureAdmission === input.admission",
                      "activeCapture?.owner == input.captureOwner", "runtimeGeneration == callbackGeneration"):
            self.assertIn(check, inject)
        self.assertLess(inject.index("withInputAdmission(input)"), inject.index("inputInjector.inject(input.command)"))
        admission = body(self.gate, "func withInputAdmission<")
        self.assertLess(admission.index("lock.lock()"), admission.index("input.connectionReceipt == receipt()"))
        self.assertIn("defer { lock.unlock() }", admission)
        self.assertLess(admission.index("permits(input.captureOwner)"), admission.index("return body()"))
        self.assertIn("input.context.streamID == input.captureOwner.streamID", admission)
        self.assertNotRegex(admission, r"await|desklink_host_|secure|crypto")

    def test_recording_is_debug_only_opt_in_and_inside_admission(self):
        debug = re.findall(r"#if DEBUG\n(.*?)#endif", self.bridge, re.S)
        self.assertTrue(any("recordInputDeliveriesForTesting = false" in block for block in debug))
        self.assertTrue(any("installInputBackendForTesting" in block for block in debug))
        self.assertTrue(any("if recordInputDeliveriesForTesting { injectedInputsForTesting.append(input) }" in block for block in debug))
        inject = body(self.bridge, "private func inject(_ input: HostInputDelivery)")
        self.assertLess(inject.index("withInputAdmission(input)"), inject.index("injectedInputsForTesting.append(input)"))
        self.assertLess(inject.index("inputInjector.inject(input.command)"), inject.index("injectedInputsForTesting.append(input)"))
        factory = body(self.bridge, "private func makeInputInjector(")
        self.assertIn("return MacInputInjector(displayFrame: displayFrame)", factory)
        self.assertIn("#if DEBUG", factory)

    def test_release_all_and_stop_are_not_filtered_as_ordinary_input(self):
        consume = body(self.bridge, "fileprivate func consume(")
        release = consume.split("case .releaseAll:", 1)[1].split("case .metrics:", 1)[0]
        self.assertIn("releaseAllLocalInput()", release)
        self.assertNotIn("inputDelivery", release)
        self.assertNotIn("withInputAdmission", release)
        stop = body(self.bridge, "private func beginStop()")
        self.assertIn("captureAdmission?.close()", stop)
        self.assertIn("retireActiveCapture()", stop)

    def test_swift_regressions_cover_both_queue_interleavings_and_successor_input(self):
        for name in (
            "testQueuedInputIsRejectedAfterNativeConnectingWhileUIStillConnected",
            "testQueuedInputIsRejectedAfterNativeNewNoiseWhileUIStillHasOldCapture",
            "testOriginalRustOwnerAndContextAreNeverRelabelledByCurrentCapture",
            "testRetiringCaptureRejectsAlreadyPreparedInputAtFinalGate",
            "testReusedStreamCannotBlessAnOldNoiseInputOrOverwriteOriginalEpoch",
            "testReleaseAllAndUserStopKeepTheirCleanupSemantics",
        ):
            self.assertIn(name, self.tests)
        self.assertIn("HostInputRecordingBackend", self.tests)
        self.assertIn("installInputBackendForTesting(backend)", self.tests)
        self.assertIn("injected.context, HostInputContext(streamID: owner.streamID, inputEpoch: epoch)", self.tests)
        self.assertNotRegex(self.tests, r"\.request(ScreenRecording|Accessibility)\(|SystemCGEventBackend\(\)|NativeHostCaptureSession\(")


if __name__ == "__main__":
    unittest.main()
