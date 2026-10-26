import DeskLinkAppleCore

/// Queue admission and the host's confirmed state are separate UI states.
struct SessionOperationStatus {
    private(set) var pendingQuality: SessionVideoQualityPreference?
    private(set) var pendingDisplayID: UInt32?
    private(set) var message: String?
    var hasPending: Bool { pendingQuality != nil || pendingDisplayID != nil }

    mutating func qualityQueued(_ preference: SessionVideoQualityPreference) {
        pendingQuality = preference
        message = "已发送画质请求，正在等待远端确认。"
    }

    mutating func displayQueued(_ identifier: UInt32) {
        pendingDisplayID = identifier
        message = "已发送切屏请求，正在等待远端确认。"
    }

    mutating func qualityConfirmed(_ quality: SessionVideoQuality?) {
        guard let requested = pendingQuality, let quality else { return }
        pendingQuality = nil
        message = quality.preference == requested
            ? "远端已确认画质：\(quality.preference.title)（当前\(quality.preset.title)）。"
            : "远端仍使用\(quality.preference.title)画质，可稍后重试。"
    }

    mutating func displayConfirmed(_ identifier: UInt32?) {
        guard let requested = pendingDisplayID, let identifier else { return }
        pendingDisplayID = nil
        message = requested == identifier ? "远端已确认切换显示器。" : "远端仍使用原显示器，可稍后重试。"
    }

    mutating func couldNotSend(_ reason: String?) {
        pendingQuality = nil
        pendingDisplayID = nil
        message = reason ?? "操作尚未发送，请确认连接后重试。"
    }

    mutating func qualityTimedOut() {
        guard pendingQuality != nil else { return }
        pendingQuality = nil
        message = "远端尚未确认画质，当前画质以远端状态为准，可重试。"
    }

    mutating func displayTimedOut() {
        guard pendingDisplayID != nil else { return }
        pendingDisplayID = nil
        message = "远端尚未确认切屏，当前显示器以远端状态为准，可重试。"
    }

    mutating func reset() {
        pendingQuality = nil
        pendingDisplayID = nil
        message = nil
    }
}
