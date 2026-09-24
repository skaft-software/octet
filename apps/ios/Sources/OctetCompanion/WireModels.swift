import Foundation

public let octetProtocolMajor: UInt16 = 1

/// JSON used for the small number of protocol unions whose presentation is
/// deliberately typed by the host. Unknown union cases are retained only long
/// enough to be ignored safely; they are never rendered as arbitrary JSON.
public indirect enum WireJSONValue: Codable, Equatable, Sendable {
    case object([String: WireJSONValue])
    case array([WireJSONValue])
    case string(String)
    case number(Double)
    case bool(Bool)
    case null

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Int64.self) {
            self = .number(Double(value))
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([WireJSONValue].self) {
            self = .array(value)
        } else {
            self = .object(try container.decode([String: WireJSONValue].self))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .object(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .bool(let value): try container.encode(value)
        case .null: try container.encodeNil()
        }
    }

    public var objectValue: [String: WireJSONValue]? {
        guard case .object(let value) = self else { return nil }
        return value
    }

    public var arrayValue: [WireJSONValue]? {
        guard case .array(let value) = self else { return nil }
        return value
    }

    public var stringValue: String? {
        guard case .string(let value) = self else { return nil }
        return value
    }

    public var boolValue: Bool? {
        guard case .bool(let value) = self else { return nil }
        return value
    }

    public var uint64Value: UInt64? {
        guard case .number(let value) = self else { return nil }
        return UInt64(exactly: value)
    }

    public func value(for key: String) -> WireJSONValue? {
        objectValue?[key]
    }

    public func string(for key: String) -> String? {
        value(for: key)?.stringValue
    }

    public func bool(for key: String) -> Bool? {
        value(for: key)?.boolValue
    }

    public func uint64(for key: String) -> UInt64? {
        value(for: key)?.uint64Value
    }

    public func object(for key: String) -> [String: WireJSONValue]? {
        value(for: key)?.objectValue
    }
}

public struct WireCursor: Codable, Equatable, Hashable, Sendable {
    public let actorGeneration: UInt64
    public let sequence: UInt64

    public init(actorGeneration: UInt64, sequence: UInt64) {
        self.actorGeneration = actorGeneration
        self.sequence = sequence
    }

    public static let zero = WireCursor(actorGeneration: 0, sequence: 0)
}

public struct WireHostDescriptor: Codable, Equatable, Sendable {
    public let id: String
    public let name: String
}

public struct WireModelSelection: Codable, Equatable, Sendable {
    public let provider: String
    public let model: String
    public let reasoning: String

    public init(provider: String = "", model: String = "", reasoning: String = "") {
        self.provider = provider
        self.model = model
        self.reasoning = reasoning
    }
}

public struct WireSessionSummary: Codable, Equatable, Sendable {
    public let id: String
    public let title: String
    public let modifiedAtMs: UInt64
    public let pinned: Bool
    public let archived: Bool
    public let provisional: Bool
    public let liveState: String
    public let attention: String
    public let model: WireModelSelection

    public init(
        id: String,
        title: String,
        modifiedAtMs: UInt64 = 0,
        pinned: Bool = false,
        archived: Bool = false,
        provisional: Bool = false,
        liveState: String = "idle",
        attention: String = "none",
        model: WireModelSelection = WireModelSelection()
    ) {
        self.id = id
        self.title = title
        self.modifiedAtMs = modifiedAtMs
        self.pinned = pinned
        self.archived = archived
        self.provisional = provisional
        self.liveState = liveState
        self.attention = attention
        self.model = model
    }
}

public struct WireSessionItem: Codable, Equatable, Sendable {
    public let id: String
    public let lifecycle: String
    public let payload: WireJSONValue

    public init(id: String, lifecycle: String, payload: WireJSONValue) {
        self.id = id
        self.lifecycle = lifecycle
        self.payload = payload
    }

    public func transcriptEntry() -> TranscriptEntry? {
        guard let type = payload.string(for: "type"),
              let data = payload.object(for: "data") else { return nil }
        let provisional = lifecycle == "provisional"
        switch type {
        case "userMessage":
            return TranscriptEntry(
                id: id,
                kind: .user,
                text: SafeText.value(data["text"]?.stringValue, limit: 64_000) ?? "",
                isProvisional: provisional
            )
        case "assistantMessage":
            return TranscriptEntry(
                id: id,
                kind: .assistant,
                text: SafeText.value(data["text"]?.stringValue, limit: 128_000) ?? "",
                isProvisional: provisional
            )
        case "reasoning":
            return TranscriptEntry(
                id: id,
                kind: .reasoning,
                text: SafeText.value(data["text"]?.stringValue, limit: 64_000) ?? "",
                isProvisional: provisional
            )
        case "toolCall":
            return toolEntry(id: id, data: data, provisional: provisional)
        case "toolResult":
            let summary = SafeText.value(data["summary"]?.stringValue, limit: 2_000)
            let output = SafeText.value(data["outputSummary"]?.stringValue, limit: 4_000)
            return TranscriptEntry(
                id: id,
                kind: .tool,
                title: "Tool result",
                summary: output ?? summary,
                status: data["status"]?.stringValue,
                isProvisional: provisional
            )
        case "plan":
            return TranscriptEntry(id: id, kind: .system, title: "Plan", summary: "A host-authored plan is available.", isProvisional: provisional)
        case "fileChange":
            return TranscriptEntry(id: id, kind: .tool, title: "Workspace change", summary: "A host-tracked workspace change is available.", isProvisional: provisional)
        case "source":
            return TranscriptEntry(id: id, kind: .system, title: "Source consulted", summary: "A host-authenticated source reference is available.", isProvisional: provisional)
        case "artifact":
            return TranscriptEntry(id: id, kind: .system, title: "Output produced", summary: "A host-authenticated output is available.", isProvisional: provisional)
        case "preview":
            return TranscriptEntry(id: id, kind: .system, title: "Preview available", summary: "A host preview is available.", isProvisional: provisional)
        case "compaction":
            return TranscriptEntry(id: id, kind: .system, title: "Context updated", summary: SafeText.value(data["reason"]?.stringValue, limit: 1_000), isProvisional: provisional)
        case "runOutcome":
            let review = data["review"]?.objectValue
            let summary = SafeText.value(review?["summary"]?.stringValue, limit: 2_000)
                ?? SafeText.value(data["message"]?.stringValue, limit: 2_000)
            return TranscriptEntry(id: id, kind: .system, title: "Run complete", summary: summary, status: data["outcome"]?.stringValue, isProvisional: provisional)
        default:
            // New host payloads must be added explicitly before presentation.
            return nil
        }
    }

    private func toolEntry(id: String, data: [String: WireJSONValue], provisional: Bool) -> TranscriptEntry {
        let activity = data["activity"]?.objectValue ?? data
        let title = SafeText.value(activity["title"]?.stringValue, limit: 512) ?? "Tool activity"
        let summary = SafeText.value(activity["summary"]?.stringValue, limit: 2_000)
            ?? SafeText.value(activity["outputSummary"]?.stringValue, limit: 4_000)
        let target = SafeText.value(activity["target"]?.stringValue, limit: 512)
        let status = activity["status"]?.stringValue
        return TranscriptEntry(
            id: id,
            kind: .tool,
            title: title,
            summary: summary,
            target: target,
            status: status,
            isProvisional: provisional
        )
    }
}

public struct WirePendingRequest: Codable, Equatable, Sendable {
    public let id: String
    public let actorGeneration: UInt64
    public let kind: WireJSONValue
    public let state: String

    public init(id: String, actorGeneration: UInt64, kind: WireJSONValue, state: String) {
        self.id = id
        self.actorGeneration = actorGeneration
        self.kind = kind
        self.state = state
    }

    public func view() -> PendingRequestView {
        let type = kind.string(for: "type") ?? "userInput"
        let data = kind.object(for: "data") ?? [:]
        switch type {
        case "approval":
            return PendingRequestView(
                id: id,
                actorGeneration: actorGeneration,
                kind: "approval",
                title: SafeText.value(data["action"]?.stringValue, limit: 1_000) ?? "Approval required",
                choices: [],
                state: state
            )
        case "userInput":
            return PendingRequestView(
                id: id,
                actorGeneration: actorGeneration,
                kind: "input",
                title: SafeText.value(data["prompt"]?.stringValue, limit: 2_000) ?? "Input required",
                choices: (data["choices"]?.arrayValue ?? []).compactMap(\.stringValue).map { SafeText.value($0, limit: 256) ?? "" }.filter { !$0.isEmpty },
                state: state
            )
        default:
            return PendingRequestView(id: id, actorGeneration: actorGeneration, kind: "request", title: "Host input required", choices: [], state: state)
        }
    }
}

public struct WireSessionSnapshot: Codable, Equatable, Sendable {
    public let sessionID: String
    public let actorGeneration: UInt64
    public let cursor: WireCursor
    public let liveState: String
    public let activeRunID: String?
    public let model: WireModelSelection
    public let items: [WireSessionItem]
    public let pendingRequests: [WirePendingRequest]

    public init(
        sessionID: String,
        actorGeneration: UInt64,
        cursor: WireCursor,
        liveState: String,
        activeRunID: String? = nil,
        model: WireModelSelection = WireModelSelection(),
        items: [WireSessionItem] = [],
        pendingRequests: [WirePendingRequest] = []
    ) {
        self.sessionID = sessionID
        self.actorGeneration = actorGeneration
        self.cursor = cursor
        self.liveState = liveState
        self.activeRunID = activeRunID
        self.model = model
        self.items = items
        self.pendingRequests = pendingRequests
    }

    public func projection(title: String) -> SessionProjection {
        SessionProjection(
            id: sessionID,
            actorGeneration: actorGeneration,
            cursor: cursor,
            title: title,
            liveState: liveState,
            activeRunID: activeRunID,
            modelLabel: model.model.isEmpty ? model.provider : [model.provider, model.model].filter { !$0.isEmpty }.joined(separator: " / "),
            items: items.compactMap { $0.transcriptEntry() },
            pendingRequests: pendingRequests.map { $0.view() }
        )
    }
}

public struct WireEventEnvelope: Codable, Equatable, Sendable {
    public let protocolVersion: UInt16
    public let sessionID: String
    public let cursor: WireCursor
    public let event: WireJSONValue

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol"
        case sessionID
        case cursor
        case event
    }

    public init(protocolVersion: UInt16, sessionID: String, cursor: WireCursor, event: WireJSONValue) {
        self.protocolVersion = protocolVersion
        self.sessionID = sessionID
        self.cursor = cursor
        self.event = event
    }

    public var eventType: String? { event.string(for: "type") }
    public var eventData: [String: WireJSONValue] { event.object(for: "data") ?? [:] }
}

public struct WireCatalogChange: Codable, Equatable, Sendable {
    public let catalogCursor: UInt64
    public let summary: WireSessionSummary
}

public struct WireHostStreamEvent: Codable, Equatable, Sendable {
    public let protocolVersion: UInt16
    public let hostSequence: UInt64
    public let event: WireEventEnvelope?
    public let catalog: WireCatalogChange?

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol"
        case hostSequence
        case event
        case catalog
    }
}

public struct WireHostBootstrap: Codable, Equatable, Sendable {
    public let protocolVersion: UInt16
    public let host: WireHostDescriptor
    public let catalogCursor: UInt64
    public let sessions: [WireSessionSummary]
    public let selectedSessionID: String?
    public let selectedSession: WireSessionSnapshot?

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol"
        case host
        case catalogCursor
        case sessions
        case selectedSessionID
        case selectedSession
    }
}

public struct WireReplayGap: Codable, Equatable, Sendable {
    public let requestedAfter: WireCursor
    public let earliestAvailable: WireCursor
    public let latestAvailable: WireCursor
}

public struct WireReplayResponse: Codable, Equatable, Sendable {
    public let type: String
    public let after: WireCursor?
    public let through: WireCursor?
    public let events: [WireEventEnvelope]?
    public let gap: WireReplayGap?
    public let snapshot: WireSessionSnapshot?

    public var isGap: Bool { type == "gap" }
}

public enum SafeText {
    public static func value(_ text: String?, limit: Int) -> String? {
        guard let text, !text.isEmpty else { return nil }
        if text.count <= limit { return text }
        let end = text.index(text.startIndex, offsetBy: max(0, limit - 1))
        return String(text[..<end]) + "…"
    }
}

public enum WireDecoder {
    private static let decoder = JSONDecoder()
    private static let maxBootstrapBytes = 8 * 1024 * 1024
    private static let maxSnapshotBytes = 8 * 1024 * 1024
    private static let maxEventBytes = 512 * 1024
    private static let maxReplayBytes = 8 * 1024 * 1024

    public static func bootstrap(_ data: Data, expectedHostID: String) throws -> WireHostBootstrap {
        guard data.count <= maxBootstrapBytes else { throw CompanionError.protocolViolation("bootstrap is too large") }
        let value = try decoder.decode(WireHostBootstrap.self, from: data)
        guard value.protocolVersion == octetProtocolMajor else { throw CompanionError.protocolViolation("unsupported protocol") }
        guard value.host.id == expectedHostID, !value.host.id.isEmpty else { throw CompanionError.hostIdentityChanged }
        guard value.sessions.count <= 2_000 else { throw CompanionError.protocolViolation("too many sessions") }
        if let snapshot = value.selectedSession {
            try validate(snapshot: snapshot, expectedSessionID: value.selectedSessionID)
        }
        return value
    }

    public static func snapshot(_ data: Data, expectedSessionID: String) throws -> WireSessionSnapshot {
        guard data.count <= maxSnapshotBytes else { throw CompanionError.protocolViolation("snapshot is too large") }
        let value = try decoder.decode(WireSessionSnapshot.self, from: data)
        try validate(snapshot: value, expectedSessionID: expectedSessionID)
        return value
    }

    public static func streamEvent(_ data: Data, expectedHostID: String? = nil) throws -> WireHostStreamEvent {
        guard data.count <= maxEventBytes else { throw CompanionError.protocolViolation("event is too large") }
        let value = try decoder.decode(WireHostStreamEvent.self, from: data)
        guard value.protocolVersion == octetProtocolMajor, value.hostSequence > 0,
              (value.event == nil) != (value.catalog == nil) else {
            throw CompanionError.protocolViolation("invalid host stream event")
        }
        if let event = value.event {
            guard event.protocolVersion == octetProtocolMajor,
                  !event.sessionID.isEmpty,
                  event.cursor.actorGeneration > 0,
                  event.cursor.sequence > 0 else {
                throw CompanionError.protocolViolation("invalid session event")
            }
        }
        if let summary = value.catalog?.summary {
            guard !summary.id.isEmpty else { throw CompanionError.protocolViolation("invalid catalog summary") }
        }
        _ = expectedHostID
        return value
    }

    public static func replay(_ data: Data) throws -> WireReplayResponse {
        guard data.count <= maxReplayBytes else { throw CompanionError.protocolViolation("replay is too large") }
        let value = try decoder.decode(WireReplayResponse.self, from: data)
        guard value.type == "events" || value.type == "gap" else {
            throw CompanionError.protocolViolation("unknown replay response")
        }
        if value.type == "events" {
            guard value.after != nil, value.through != nil, value.events != nil else {
                throw CompanionError.protocolViolation("events replay is incomplete")
            }
        } else if value.gap == nil || value.snapshot == nil {
            throw CompanionError.protocolViolation("gap replay is incomplete")
        }
        return value
    }

    private static func validate(snapshot: WireSessionSnapshot, expectedSessionID: String?) throws {
        guard !snapshot.sessionID.isEmpty,
              snapshot.actorGeneration > 0,
              snapshot.cursor.actorGeneration == snapshot.actorGeneration,
              snapshot.cursor.sequence >= 0,
              expectedSessionID == nil || expectedSessionID == snapshot.sessionID else {
            throw CompanionError.protocolViolation("invalid session snapshot")
        }
        guard snapshot.items.count <= 10_000, snapshot.pendingRequests.count <= 128 else {
            throw CompanionError.protocolViolation("snapshot exceeds bounds")
        }
    }
}
