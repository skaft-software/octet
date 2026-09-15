import Foundation
import OctetServe

/// Pure app-side policies. Wire state and protocol transitions stay in
/// OctetServeClient; these helpers only decide how native UI presents it.
public enum SessionListPolicy {
    public static func filtered(_ sessions: [SessionSummary], query: String) -> [SessionSummary] {
        let needle = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return sessions }
        return sessions.filter { session in
            [session.title, session.preview, session.projectID, session.id]
                .map { $0.localizedCaseInsensitiveContains(needle) }
                .contains(true)
        }
    }

    public static func sort(_ sessions: [SessionSummary]) -> [SessionSummary] {
        sessions.sorted {
            if $0.pinned != $1.pinned { return $0.pinned && !$1.pinned }
            return $0.updatedAt > $1.updatedAt
        }
    }
}

public struct NotificationRoute: Codable, Equatable, Sendable {
    public let sessionID: String
    public let eventID: String

    public init(sessionID: String, eventID: String) {
        self.sessionID = sessionID
        self.eventID = eventID
    }
}

public struct PendingMutation: Identifiable, Equatable, Sendable {
    public let id: String
    public let sessionID: String?
    public let label: String
    public let createdAt: Date

    public init(id: String, sessionID: String?, label: String, createdAt: Date = .now) {
        self.id = id
        self.sessionID = sessionID
        self.label = label
        self.createdAt = createdAt
    }
}

public struct ReconnectionBackoff: Sendable {
    private var attempt = 0
    private let maximum: UInt64 = 30_000_000_000

    public init() {}

    public mutating func reset() {
        attempt = 0
    }

    public mutating func nextDelayNanoseconds() -> UInt64 {
        attempt += 1
        let exponent = min(attempt - 1, 5)
        let base = min(UInt64(1_000_000_000) << UInt64(exponent), maximum)
        let jitter = UInt64.random(in: 0...(base / 5))
        return min(base + jitter, maximum)
    }
}

public enum NotificationPolicy {
    /// Notifications carry only a revalidation route. They never contain
    /// prompt text, tool output, credentials, or host identity material.
    public static func userInfo(for route: NotificationRoute) -> [AnyHashable: Any] {
        [
            "sessionID": route.sessionID,
            "eventID": route.eventID,
        ]
    }

    public static func route(from userInfo: [AnyHashable: Any]) -> NotificationRoute? {
        guard let sessionID = userInfo["sessionID"] as? String,
              let eventID = userInfo["eventID"] as? String,
              !sessionID.isEmpty,
              !eventID.isEmpty else {
            return nil
        }
        return NotificationRoute(sessionID: sessionID, eventID: eventID)
    }
}

public enum PairingPresentation {
    case idle
    case discovering
    case checking(DiscoveredHost)
    case awaitingTicket(PairingInfo)
    case waiting(PairingAttempt)
    case approved(String)
    case denied(String)
    case failed(String)
}

public enum ComposerMode: Equatable, Sendable {
    case prompt
    case steer
    case followUp
    case disabled(String)
}

extension ComposerMode {
    var buttonTitle: String {
        switch self {
        case .prompt: return "Send"
        case .steer: return "Steer"
        case .followUp: return "Follow up"
        case .disabled(let reason): return reason
        }
    }
}
