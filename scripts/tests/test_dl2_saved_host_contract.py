"""Apple saved-host schema and UI source contracts; does not execute Swift."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
STORE = ROOT / "apps/apple/Sources/DeskLinkAppleCore/SavedHostStore.swift"
VIEWS = [
    ROOT / "apps/macos/Sources/DeskLinkApp/Views/ControllerHomeView.swift",
    ROOT / "apps/ios/DeskLinkIOS/Views/IOSConnectionHomeView.swift",
    ROOT / "apps/ios/DeskLinkIOS/Views/IOSSavedHostsView.swift",
]


def body(text: str, signature: str) -> str:
    start = text.index("{", text.index(signature))
    depth = 1
    for end in range(start + 1, len(text)):
        depth += (text[end] == "{") - (text[end] == "}")
        if depth == 0:
            return text[start + 1:end]
    raise AssertionError(signature)


class SavedHostContractTests(unittest.TestCase):
    def setUp(self):
        self.source = STORE.read_text(encoding="utf-8")

    def test_authorization_and_directory_metadata_remain_optional_for_legacy_decode(self):
        fields = {
            "expiresAtUnixS": "UInt64", "authorizationType": "UInt32",
            "directoryDeviceID": "UInt64", "directoryAccessCode": "[UInt8]",
            "alias": "String", "lastConnectedAt": "Date",
        }
        constructor = body(self.source, "public init(")
        for name, value_type in fields.items():
            self.assertIn(f"public let {name}: {value_type}?", self.source)
            self.assertIn(f"{name}: {value_type}? = nil", self.source)
            self.assertIn(f"self.{name} =", constructor)
        structural = body(self.source, "public var isValid")
        self.assertNotIn("canReconnect", structural)
        self.assertNotIn("Date()", structural)

    def test_unknown_never_becomes_permanent_and_temporary_ends_at_expiry(self):
        reconnect = body(self.source, "public func canReconnect(at")
        self.assertIn("guard isValid", reconnect)
        self.assertIn("case 1:", reconnect)
        self.assertIn("expiresAtUnixS > 0", reconnect)
        self.assertIn("date.timeIntervalSince1970 < TimeInterval(expiresAtUnixS)", reconnect)
        self.assertIn("case 2:", reconnect)
        self.assertIn("return expiresAtUnixS == 0", reconnect)
        self.assertRegex(reconnect, r"default:[\s\S]*return false")
        self.assertNotRegex(reconnect, r"expiresAtUnixS\s*\?\?\s*0")

    def test_material_refresh_preserves_local_identity_alias_and_updates_material(self):
        save = body(self.source, "public func save(")
        self.assertIn("$0.id == host.id && !Self.sameRemoteHost($0, host)", save)
        self.assertIn("id: previous?.id ?? host.id", save)
        self.assertIn("alias: previous?.alias ?? host.alias", save)
        self.assertIn("lastConnectedAt: host.lastConnectedAt ?? previous?.lastConnectedAt", save)
        copy = body(self.source, "fileprivate func withLocalMetadata")
        for field in ("expiresAtUnixS", "authorizationType", "directoryDeviceID", "directoryAccessCode"):
            self.assertIn(f"{field}: {field}", copy)
        identity = body(self.source, "private static func sameRemoteHost")
        self.assertIn("lhs.hostVerifyKey == rhs.hostVerifyKey", identity)
        self.assertIn("lhs.serverName == rhs.serverName", identity)

    def test_display_name_precedence_and_debug_do_not_expose_credentials(self):
        display = body(self.source, "public var displayName")
        self.assertLess(display.index("normalizedAlias"), display.index("directoryDeviceID"))
        self.assertLess(display.index("directoryDeviceID"), display.index("SHA256.hash"))
        self.assertNotIn("relayAuthentication", display)
        self.assertNotIn("directoryAccessCode", display)
        self.assertNotIn("serverName", display)
        self.assertIn('public var description: String { "SavedHost([REDACTED])" }', self.source)
        self.assertIn("public var debugDescription: String { description }", self.source)

    def test_all_saved_host_lists_use_identity_names_and_gate_unusable_authorization(self):
        for path in VIEWS:
            with self.subTest(path=path):
                view = path.read_text(encoding="utf-8")
                self.assertIn("host.displayName", view)
                self.assertNotIn("host.serverName", view)
                self.assertIn("!host.canReconnect", view)
                self.assertIn("请重新配对", view)
                self.assertNotIn("directoryAccessCode", view)
                self.assertNotIn("relayAuthentication", view)

    def test_rename_is_local_validated_and_persisted(self):
        rename = body(self.source, "public func rename(")
        self.assertIn("SavedHost.normalizedAlias(alias)", rename)
        self.assertIn("SavedHost.aliasIsValid(normalized)", rename)
        self.assertIn("try write(try Self.encode(hosts))", rename)
        self.assertIn("alias.count <= 80", self.source)
        self.assertIn("CharacterSet.controlCharacters.contains", self.source)
        self.assertIn("trimmingCharacters(in: .whitespacesAndNewlines)", self.source)
        for path in (VIEWS[0], VIEWS[2]):
            view = path.read_text(encoding="utf-8")
            self.assertIn('Button("重命名")', view)
            self.assertIn(".rename(id: host.id, alias: aliasDraft)", view)

    def test_directory_credentials_have_a_complete_structural_contract(self):
        directory = body(self.source, "private var directoryIsStructurallyValid")
        self.assertIn("case (nil, nil): true", directory)
        self.assertIn("deviceID > 0 && code.count == 8", directory)
        self.assertIn("Self.directoryAlphabet.contains($0)", directory)
        self.assertIn("default: false", directory)


if __name__ == "__main__":
    unittest.main()
