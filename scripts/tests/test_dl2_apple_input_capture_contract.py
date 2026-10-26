"""Portable source contracts for native input producers; no Swift/hardware claim."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
MAC_INPUT = "apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift"
MAC_KEYS = "apps/macos/Sources/DeskLinkApp/Input/KeyboardMapper.swift"
IOS_KEYS = "apps/ios/DeskLinkIOS/Input/IOSKeyboardInput.swift"
IOS_TOUCH = "apps/ios/DeskLinkIOS/Input/IOSTouchInputView.swift"


def source(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


def call_arguments(text: str, marker: str):
    offset = 0
    while (start := text.find(marker, offset)) >= 0:
        index, depth, quoted, escaped = start + len(marker), 1, False, False
        begin = index
        while depth:
            character = text[index]
            if quoted:
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == '"':
                    quoted = False
            elif character == '"':
                quoted = True
            elif character == "(":
                depth += 1
            elif character == ")":
                depth -= 1
            index += 1
        yield text[begin:index - 1]
        offset = index


class AppleInputCaptureContractTests(unittest.TestCase):
    def test_every_production_ui_input_send_requires_explicit_context(self):
        for path in [MAC_INPUT, IOS_KEYS, IOS_TOUCH, "apps/macos/Sources/DeskLinkApp/Views/SessionView.swift"]:
            text = source(path)
            for marker in ["bridge.send(", "bridge?.send("]:
                for arguments in call_arguments(text, marker):
                    with self.subTest(path=path, arguments=arguments):
                        self.assertTrue("context:" in arguments or arguments.startswith("captured:"))

    def test_ui_cleanup_never_uses_current_context_implicit_release_all(self):
        for path in [MAC_INPUT, IOS_KEYS, IOS_TOUCH, "apps/macos/Sources/DeskLinkApp/Views/SessionView.swift", "apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift", "apps/ios/DeskLinkIOS/Views/IOSSessionView.swift"]:
            self.assertNotRegex(source(path), r"(?:bridge|controller)\??\.releaseAll\(\)")
        mac = source(MAC_INPUT)
        self.assertNotIn("Task { @MainActor in bridge?", mac)
        self.assertIn("keyboard.activeContexts.union(pressedButtons.values)", mac)

    def test_key_release_and_modifier_restore_use_original_context(self):
        keys = source(MAC_KEYS)
        keyup = keys.split("mutating func keyUp", 1)[1].split("mutating func flagsChanged", 1)[0]
        self.assertIn("capture(releases, context: held.context) + restore", keyup)
        self.assertIn("original.hasSameInputTarget(as: held.context)", keyup)
        self.assertNotIn("captureInputContext()", keyup)
        self.assertIn("context: original", keys)

    def test_presentation_barrier_cannot_recreate_repeat_without_physical_down(self):
        keys = source(MAC_KEYS)
        self.assertIn("guard isRepeat, held.context.hasSameInputTarget(as: context) else { return [] }", keys)
        self.assertIn("guard !isRepeat else { return [] }", keys)
        view = source(MAC_INPUT)
        self.assertIn("view.synchronizeInputContext(bridge.presentedInputContext)", view)
        self.assertIn("private var pressedButtons: [MouseButton: CapturedInputContext]", view)

    def test_ime_keeps_original_interpretation_and_composition_context(self):
        view = source(MAC_INPUT)
        self.assertIn("interpretingInputContext = context", view)
        self.assertIn("compositionInputContext = context", view)
        commit = view.split("func insertText", 1)[1].split("override func doCommand", 1)[0]
        self.assertIn("compositionInputContext ?? interpretingInputContext ?? textFocusContext", commit)
        self.assertNotIn("captureInputContext()", commit)
        self.assertIn("compositionInputContext = nil", view)
        self.assertIn("textFocusContext = nil", view)

    def test_delayed_long_press_captures_began_ticket_without_recapturing(self):
        touch = source(IOS_TOUCH)
        schedule = touch.split("private func scheduleLongPress", 1)[1].split("private func cancelLongPress", 1)[0]
        self.assertIn("[weak self, ticket]", schedule)
        self.assertIn("gestureCapture.accepts(ticket)", schedule)
        self.assertIn("context: ticket.context", schedule)
        self.assertNotIn("captureInputContext()", schedule)
        self.assertIn("gestureCapture.begin(context: context)", touch)
        self.assertIn("gestureCapture.cancel()", touch)

    def test_ios_keyboard_retires_old_responder_and_resigns_synchronously(self):
        keys = source(IOS_KEYS)
        finish = keys.split("private func finishResigning()", 1)[1].split("func synchronizeInputContext", 1)[0]
        self.assertNotIn("Task", finish)
        self.assertIn("context: context", finish)
        self.assertIn("retired?.owner = nil", keys)
        self.assertIn("context: original.context", keys)
        session = source("apps/ios/DeskLinkIOS/Views/IOSSessionView.swift")
        self.assertIn(".id(controller.presentedInputContext?.targetIdentity)", session)
        self.assertIn("keyboard.synchronizeInputContext($0)", session)

    def test_production_mac_geometry_prefers_presented_rect_without_empty_fallback(self):
        view = source(MAC_INPUT)
        self.assertIn("let visibleVideoRect: CGRect?", view)
        self.assertIn("visibleVideoRect ?? VideoGeometry.aspectFit", view)
        self.assertNotIn("visibleVideoRect.isEmpty ?", view)
        self.assertIn("InputMapper(videoRect: inputVideoRect)", view)

    def test_quality_version_change_preserves_target_and_does_not_create_ui_barrier(self):
        context = source("apps/apple/Sources/DeskLinkAppleCore/CapturedInputContext.swift")
        identity = context.split("public struct TargetIdentity", 1)[1].split("public let streamID", 2)
        self.assertIn("public let inputEpoch", identity[1])
        self.assertIn("public let runtimeGeneration", identity[1])
        self.assertNotIn("configVersion", identity[1])
        for path in [MAC_INPUT, IOS_KEYS, IOS_TOUCH]:
            self.assertIn("context?.targetIdentity != surfaceContext?.targetIdentity", source(path))
        keys = source(MAC_KEYS)
        self.assertIn("KeyPress(context: held.context, commands: commands)", keys)
        self.assertIn("return releases + capture(commands, context: held.context)", keys)


if __name__ == "__main__":
    unittest.main()
