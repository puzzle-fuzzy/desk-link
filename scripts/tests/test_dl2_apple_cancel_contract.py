"""Source contracts for Apple cancellation; these do not execute SwiftUI."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
MAC_VIEWS = ROOT / "apps/macos/Sources/DeskLinkApp/Views"
IOS_VIEWS = ROOT / "apps/ios/DeskLinkIOS/Views"


def source(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def balanced_body(text: str, signature: str) -> str:
    """Extract the matching Swift block rather than searching unrelated code."""
    start = text.index("{", text.index(signature))
    depth = 1
    for end in range(start + 1, len(text)):
        depth += (text[end] == "{") - (text[end] == "}")
        if depth == 0:
            return text[start + 1:end]
    raise AssertionError(f"Unclosed block: {signature}")


class AppleCancellationContractTests(unittest.TestCase):
    def test_all_waiting_states_have_reachable_mac_cancel(self):
        text = source(MAC_VIEWS / "ConnectView.swift")
        waiting = balanced_body(text, "func deskLinkConnectionIsWaiting")
        self.assertRegex(waiting, r"case \.pairing, \.connecting, \.reconnecting, \.recovering: true")
        details = balanced_body(text, "func deskLinkConnectionWaitingDetail")
        for state in ("connecting", "pairing", "reconnecting", "recovering"):
            self.assertRegex(details, rf'case \.{state}: "[^"\n]*取消[^"\n]*"')
        progress = balanced_body(text, "if let detail = deskLinkConnectionWaitingDetail")
        self.assertIn('Button("取消连接") { bridge.disconnect() }', progress)
        self.assertIn(".keyboardShortcut(.cancelAction)", progress)
        self.assertNotIn(".disabled(", progress)

    def test_cancel_preserves_mac_form_and_blocks_duplicate_attempts(self):
        text = source(MAC_VIEWS / "ConnectView.swift")
        self.assertIn('@State private var deviceID = ""', text)
        self.assertIn('@State private var accessPassword = ""', text)
        progress = balanced_body(text, "if let detail = deskLinkConnectionWaitingDetail")
        self.assertNotRegex(progress, r"(?:deviceID|accessPassword)\s*=")
        self.assertIn(".disabled(isBusy || !directoryCredentialsAreComplete)", text)
        self.assertIn("deskLinkConnectionIsWaiting(bridge.state)", text)
        home = source(MAC_VIEWS / "ControllerHomeView.swift")
        self.assertIn("deskLinkConnectionIsWaiting(bridge.state)", home)

    def test_mac_recovery_keeps_direct_cancel_in_session(self):
        text = source(MAC_VIEWS / "SessionView.swift")
        self.assertRegex(text, r'Button\(deskLinkConnectionIsWaiting\(bridge.state\) \? "取消连接" : "断开连接"\) \{\s*bridge.disconnect\(\)\s*\}')
        self.assertIn('accessibilityIdentifier("cancel-controller-session")', text)
        app = source(ROOT / "apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift")
        routing = balanced_body(app, "private func isControllerSessionState")
        self.assertIn("case .connected, .reconnecting, .recovering, .frozen: true", routing)

    def test_ios_waiting_has_direct_cancel_and_no_repeated_submit(self):
        text = source(IOS_VIEWS / "IOSConnectionHomeView.swift")
        self.assertRegex(text, r'Button\(isBusy \? "取消连接" : "断开连接", role: \.destructive\) \{\s*controller.disconnect\(\)\s*\}')
        self.assertIn(".disabled(isBusy || inviteText.trimmingCharacters", text)
        self.assertIn('accessibilityIdentifier("cancel-controller-connection")', text)
        session = source(IOS_VIEWS / "IOSSessionView.swift")
        control = balanced_body(session, "private var disconnectControl")
        self.assertIn("controller.disconnect()", control)
        self.assertNotIn(".disabled(", control)

    def test_disconnect_invalidates_late_callback_before_native_teardown(self):
        text = source(ROOT / "apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift")
        disconnect = balanced_body(text, "public func disconnect()")
        self.assertLess(disconnect.index("callbackGeneration &+= 1"), disconnect.index("handleOwner.destroy()"))
        self.assertIn("guard bridge.acceptsCallback(generation: generation) else { return }", text)


if __name__ == "__main__":
    unittest.main()
