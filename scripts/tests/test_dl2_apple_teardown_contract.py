"""Cross-language ownership checks; this does not measure AppKit responsiveness."""
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[2]


class TeardownContractTests(unittest.TestCase):
    def test_main_actor_retires_handle_without_joining_worker(self):
        bridge = (ROOT / "apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift").read_text(encoding="utf-8")
        owner = bridge.split("private final class DeskLinkHandleOwner", 1)[1].split("private struct RetiredControllerHandle", 1)[0]
        self.assertIn("DispatchQueue.global(qos: .userInitiated).async", owner)
        self.assertNotIn("desklink_destroy(pointer)", owner)
        self.assertNotIn("desklink_reject(handle)", bridge)

    def test_callback_context_is_released_only_after_native_destroy(self):
        source = (ROOT / "apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift").read_text(encoding="utf-8")
        retired = source.split("private struct RetiredControllerHandle", 1)[1].split("@MainActor", 1)[0]
        self.assertLess(retired.index("desklink_destroy(pointer)"), retired.index(".release()"))
        destroy = (ROOT / "crates/desklink-ffi/src/lib.rs").read_text(encoding="utf-8").split('fn desklink_destroy(', 1)[1].split('#[unsafe(no_mangle)]', 1)[0]
        self.assertIn("stop_worker()", destroy)

    def test_every_connected_send_uses_cancellation_boundary(self):
        worker = (ROOT / "crates/desklink-ffi/src/worker.rs").read_text(encoding="utf-8")
        connected = worker.split("async fn run_connected(", 1)[1].split("async fn send_cancellable", 1)[0]
        self.assertNotIn("controller.send_input(event).await", connected)
        self.assertNotIn("controller.request_keyframe().await", connected)
        self.assertNotIn("controller.send_input_with_context(", connected)
        self.assertIn("let next_event = controller.next_event();", connected)
        self.assertIn("tokio::pin!(next_event)", connected)
        sender = worker.split("fn start_connected_send(", 1)[1].split("enum ConnectedWork", 1)[0]
        self.assertIn("send_cancellable(&mut cancellation", sender)
        self.assertIn("ConnectedSend::Input(input)", sender)
        self.assertIn("ConnectedSend::Control(message)", sender)
        self.assertIn("ConnectedSend::Transfer(message)", sender)


if __name__ == "__main__":
    unittest.main()
