"""Portable source contracts for account restoration, not Swift execution proof."""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]
CLIENT = ROOT / "apps/apple/Sources/DeskLinkAppleCore/AccountClient.swift"


def read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def block(text: str, marker: str) -> str:
    start = text.index("{", text.index(marker))
    depth, quoted, escaped = 1, False, False
    index = start + 1
    while depth:
        value = text[index]
        if quoted:
            if escaped:
                escaped = False
            elif value == "\\":
                escaped = True
            elif value == '"':
                quoted = False
        elif value == '"':
            quoted = True
        elif value == "{":
            depth += 1
        elif value == "}":
            depth -= 1
        index += 1
    return text[start + 1:index - 1]


class AppleAccountRestoreContractTests(unittest.TestCase):
    def test_invalidation_uses_the_actual_service_status_and_stable_error_code(self):
        service = read(ROOT / "server/account/src/services/session-service.ts")
        response = read(ROOT / "server/account/src/http.ts")
        self.assertRegex(service, r'AccountError\("token_invalid",\s*401,')
        self.assertRegex(response, r'error:\s*error\.code.*error\.status')
        client = read(CLIENT)
        invalid = block(client, "var isInvalidToken")
        self.assertRegex(invalid, r'status\s*==\s*401\s*&&\s*code\s*==\s*"token_invalid"')
        self.assertNotIn("message", invalid)
        request = block(client, "private func request(")
        self.assertRegex(request, r'\.http\(status:\s*http\.statusCode,\s*code:\s*body\?\.error')

    def test_restore_deletion_is_reachable_only_for_typed_refresh_invalidation(self):
        client = read(CLIENT)
        restore = block(client, "public func restore()")
        self.assertEqual(restore.count("clearLocalSession()"), 1)
        decision = restore.index("clearLocalSession()")
        self.assertIn("error as? SessionRestoreFailure", restore[:decision])
        self.assertIn("case .invalidRefreshToken", restore[:decision])
        validate = block(client, "private func validateSession(")
        self.assertEqual(validate.count("failure.isInvalidToken"), 2)
        self.assertIn("throw SessionRestoreFailure.invalidRefreshToken", validate)
        self.assertIn("showOffline(error)", restore)
        # No additional blanket clear path may appear elsewhere in the client.
        self.assertEqual(len(re.findall(r"\bclearLocalSession\(\)", client)), 3)

    def test_refresh_result_cannot_mutate_a_newer_session_inside_network_helper(self):
        refresh = block(read(CLIENT), "private func refreshTokens(")
        self.assertNotIn("storedSession", refresh)
        self.assertNotIn("save(session:", refresh)
        self.assertNotIn("state =", refresh)

    def test_session_transitions_and_async_commits_have_generation_fences(self):
        client = read(CLIENT)
        for method in ["public func login(", "public func logout()", "public func skipLogin()"]:
            self.assertIn("advanceSessionGeneration()", block(client, method))
        fence = block(client, "private func checkGeneration(")
        self.assertIn("Task.checkCancellation()", fence)
        self.assertIn("checkSessionGeneration(generation)", fence)
        self.assertRegex(block(client, "private func checkSessionGeneration("), r"generation\s*==\s*sessionGeneration")
        restore = block(client, "public func restore()")
        self.assertLess(restore.index("checkGeneration(generation)"), restore.index("storedSession = validated.session"))
        logout = block(client, "public func logout()")
        self.assertLess(logout.index("clearLocalSession()"), logout.index("await request("))

    def test_retry_is_single_flight_and_preserves_rotated_tokens_when_save_fails(self):
        restore = block(read(CLIENT), "public func restore()")
        self.assertRegex(restore, r"guard\s+!isRestoring,\s*!isBusy")
        self.assertNotIn("guard storedSession == nil", restore)
        self.assertLess(restore.index("storedSession = validated.session"), restore.index("save(session: validated.session)"))
        self.assertIn("restoringGeneration == generation", restore)

    def test_keychain_failure_is_reported_instead_of_erasing_in_memory_state(self):
        clear = block(read(CLIENT), "private func clearLocalSession()")
        self.assertNotIn("try?", clear)
        self.assertLess(clear.index("try keychain.delete("), clear.index("storedSession = nil"))
        request = block(read(CLIENT), "private func request(")
        self.assertIn("catch is CancellationError", request)
        self.assertIn("error.code == .cancelled", request)

    def test_both_platforms_keep_local_workspace_and_show_retryable_offline_notice(self):
        for path in ["apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift", "apps/ios/DeskLinkIOS/DeskLinkIOSApp.swift"]:
            routing = read(ROOT / path)
            self.assertRegex(routing, r"case [^:]*\.offline:")
            self.assertIn("AccountRecoveryNotice(account: account)", routing)
        notice = block(read(CLIENT), "public struct AccountRecoveryNotice")
        self.assertIn("await account.restore()", notice)
        self.assertIn("retry-account-restore", notice)

    def test_behavior_regressions_use_synthetic_transport_and_keychain(self):
        tests = read(ROOT / "apps/apple/Tests/DeskLinkAppleCoreTests/AccountRestoreTests.swift")
        self.assertIn("actor AccountTestTransport: AccountHTTPTransport", tests)
        self.assertIn("transport: transport", tests)
        self.assertIn("AccountTestKeychain: KeychainStore", tests)
        self.assertNotIn("URLSession.shared", tests)
        for scenario in ["testTransientMeFailures", "testOnlyExplicitInvalidRefreshToken", "testOfflineSessionCanRetry", "testConcurrentRestoreIsSingleFlight", "testNewLoginSurvivesOldRefresh", "testCancelledRefreshEvenWithInvalidation", "testRotatedTokensRemainInMemory"]:
            self.assertIn(scenario, tests)

    def test_only_complete_successful_refresh_reply_may_cross_cancellation(self):
        client = read(CLIENT)
        request = block(client, "private func request(")
        rotation = request.split("let keepRotation", 1)[1].split("if !keepRotation", 1)[0]
        self.assertIn('path == "v1/account/refresh"', rotation)
        self.assertIn("(200..<300).contains(http.statusCode)", rotation)
        self.assertRegex(request, r"if !keepRotation\s*\{\s*try Task.checkCancellation\(\)")
        self.assertEqual(client.count("preserveSuccessfulRefreshResponse: true"), 1)
        self.assertIn("preserveSuccessfulRefreshResponse: true", block(client, "private func refreshTokens("))

    def test_rotated_pair_is_checkpointed_before_cancel_with_generation_only_fence(self):
        client = read(CLIENT)
        checkpoint = block(client, "private func checkpointRefreshedSession(")
        self.assertNotIn("Task.checkCancellation", checkpoint)
        self.assertLess(checkpoint.index("checkSessionGeneration(generation)"), checkpoint.index("storedSession = session"))
        self.assertLess(checkpoint.index("storedSession = session"), checkpoint.index("save(session: session)"))
        rotation = block(client, "private func validateSession(").split("candidate.tokens = try await refreshTokens", 1)[1]
        self.assertLess(rotation.index("checkpointRefreshedSession("), rotation.index("checkGeneration(generation)"))
        self.assertIn("tokensCheckpointed: true", rotation)
        tests = read(ROOT / "apps/apple/Tests/DeskLinkAppleCoreTests/AccountRestoreTests.swift")
        self.assertIn("testCancelledSuccessfulRotationRetainsReplacementEvenIfKeychainWriteFails", tests)
        self.assertIn("testCancelledOldRotationCannotCheckpointOverNewLogin", tests)
        self.assertIn("testCancelledOldRotationCannotResurrectLoggedOutCredential", tests)


if __name__ == "__main__":
    unittest.main()
