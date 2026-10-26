"""Source-contract checks runnable on Windows; these do not execute AppKit/IME."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]


def source(path: str) -> str:
    return (ROOT / path).read_text(encoding="utf-8")


class MacInputSourceContractTests(unittest.TestCase):
    def test_physical_character_reaches_bridge_with_pressed_state(self) -> None:
        events = source("apps/apple/Sources/DeskLinkAppleCore/DeskLinkEvents.swift")
        bridge = source("apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift")
        self.assertIn("case character(UInt32, pressed: Bool, modifiers: Modifiers)", events)
        character_case = bridge.split("case let .character(scalar, pressed, modifiers):", 1)[1].split("case let .unicode", 1)[0]
        self.assertIn("character: scalar, pressed: pressed ? 1 : 0", character_case)

    def test_committed_text_has_down_and_up_before_next_scalar(self) -> None:
        bridge = source("apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift")
        text_case = bridge.split("case let .unicode(text, modifiers):", 1)[1].split("return", 1)[0]
        self.assertRegex(text_case, r"(?s)pressed: 1.*sendInput\(&characterInput\).*characterInput\.pressed = 0.*sendInput\(&characterInput\)")

    def test_production_view_tracks_physical_keyup_repeat_flags_and_reset(self) -> None:
        view = source("apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift")
        mapper = source("apps/macos/Sources/DeskLinkApp/Input/KeyboardMapper.swift")
        for token in ["keyboard.keyDown(", "keyboard.keyUp(keyCode: event.keyCode)", "keyboard.flagsChanged(event.modifierFlags, policy: keyboardPolicy)", "keyboard.reset()", "event.isARepeat", "event.charactersIgnoringModifiers"]:
            self.assertIn(token, view)
        self.assertIn("pressedKeys.removeValue(forKey: keyCode)", mapper)
        self.assertIn("return isRepeat ? held : []", mapper)
        self.assertIn("pressed: false, modifiers: modifiers", mapper)

    def test_mac_host_forwards_unicode_keyup_and_tracks_failed_releases(self) -> None:
        host = source("apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift")
        injector = source("apps/macos/Sources/DeskLinkApp/Input/MacInputInjector.swift")
        self.assertIn(".character(input.character, pressed: input.pressed != 0, modifiers: modifiers)", host)
        self.assertIn("keyDown: pressed", injector)
        self.assertIn("pressedCharacters[value] = modifiers", injector)
        self.assertRegex(injector, r"(?s)for value in characters.*postUnicode\(String\(scalar\), pressed: false.*pressedCharacters\.removeValue")

    def test_all_modifier_codes_match_rust_ffi(self) -> None:
        events = source("apps/apple/Sources/DeskLinkAppleCore/DeskLinkEvents.swift")
        ffi = source("crates/desklink-ffi/src/lib.rs")
        for name, number, rust in [("control", 28, "Control"), ("alt", 29, "Alt"), ("shift", 30, "Shift"), ("meta", 31, "Meta")]:
            self.assertRegex(events, rf"let {name}: UInt32 = {number}\b")
            self.assertIn(f"{number} => Ok(KeyCode::{rust})", ffi)

    def test_test_input_recording_is_debug_only_and_opt_in(self) -> None:
        bridge = source("apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift")
        debug_blocks = re.findall(r"#if DEBUG\n(.*?)#endif", bridge, re.S)
        self.assertTrue(any("recordInputsForTesting = false" in block for block in debug_blocks))
        self.assertTrue(any("if recordInputsForTesting { sentInputsForTesting.append(input) }" in block for block in debug_blocks))

    def test_native_text_input_is_connected_to_production_keydown(self) -> None:
        view = source("apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift")
        self.assertIn("final class InputView: NSView, NSTextInputClient", view)
        self.assertIn("interpretKeyEvents([event])", view)
        for method in ["hasMarkedText()", "markedRange()", "selectedRange()", "setMarkedText(_ value:", "unmarkText()", "insertText(_ value:", "doCommand(by selector:", "attributedSubstring(forProposedRange", "firstRect(forCharacterRange", "characterIndex(for point:"]:
            self.assertIn(method, view)
        keydown = view.split("override func keyDown(with event: NSEvent)", 1)[1].split("override func keyUp", 1)[0]
        self.assertIn("compositionAtKeyDown = hasMarkedText()", keydown)

    def test_composition_and_cancel_never_forward_raw_candidate_commands(self) -> None:
        view = source("apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift")
        marked = view.split("func setMarkedText(", 1)[1].split("func unmarkText", 1)[0]
        self.assertNotIn("bridge?.send(", marked)
        command = view.split("override func doCommand(by selector:", 1)[1].split("func attributedSubstring", 1)[0]
        self.assertRegex(command, r"(?s)if hasMarkedText\(\).*cancelOperation.*discardComposition\(\).*return.*sendKeyboard")
        release = view.split("func releaseAll()", 1)[1].split("func synchronizeSession", 1)[0]
        self.assertIn("discardComposition()", release)

    def test_candidate_coordinates_are_screen_coordinates_from_remote_click_anchor(self) -> None:
        view = source("apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift")
        self.assertIn("textFocusAnchor = point", view)
        self.assertIn("window.convertToScreen(convert(anchor, to: nil))", view)
        self.assertIn("textFocusAnchor.x * videoRect.width", view)
        self.assertIn("textFocusAnchor.y * videoRect.height", view)

    def test_production_keyboard_routes_use_authenticated_platform_policy(self) -> None:
        view = source("apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift")
        policy = source("apps/macos/Sources/DeskLinkApp/Input/KeyboardMappingPolicy.swift")
        self.assertIn("mode == .automatic && remotePlatform == .windows", policy)
        self.assertIn("mapsCommandToControl ? .control : .meta", policy)
        self.assertIn("remotePlatform: bridge.remotePlatform", view)
        self.assertGreaterEqual(view.count("policy: keyboardPolicy"), 2)
        key_equivalent = view.split("override func performKeyEquivalent", 1)[1].split("override func flagsChanged", 1)[0]
        self.assertIn("sendKeyboard(event, isDown: true)", key_equivalent)
        self.assertIn("sendKeyboard(event, isDown: false)", key_equivalent)
        changed_policy = view.split("func synchronizeKeyboardPolicy", 1)[1].split("func hasMarkedText", 1)[0]
        self.assertIn("releaseAll()", changed_policy)

    def test_session_menu_exposes_raw_mode_and_remote_system_chords(self) -> None:
        view = source("apps/macos/Sources/DeskLinkApp/Views/SessionView.swift")
        self.assertIn("modifierMode: modifierMode", view)
        self.assertIn('Picker("按键映射", selection: $modifierMode)', view)
        self.assertIn("sendSystemKey(DeskLinkKeyCode.tab, modifiers: .option)", view)
        self.assertIn("sendSystemKey(DeskLinkKeyCode.meta)", view)
        self.assertIn(".character(114, pressed: true, modifiers: .meta)", view)


if __name__ == "__main__":
    unittest.main()
