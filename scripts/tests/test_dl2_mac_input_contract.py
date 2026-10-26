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
        for token in ["keyboard.keyDown(", "keyboard.keyUp(keyCode: event.keyCode)", "keyboard.flagsChanged(event.modifierFlags)", "keyboard.reset()", "event.isARepeat", "event.charactersIgnoringModifiers"]:
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


if __name__ == "__main__":
    unittest.main()
