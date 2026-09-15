import Foundation

public enum ConnectionState: String, Equatable, Sendable {
    case unconfigured
    case offline
    case discovering
    case pairing
    case connecting
    case connected
    case reconnecting
    case backgrounded
    case needsPairing
    case failed
}

public enum PairingState: Equatable, Sendable {
    case idle
    case discovering
    case awaitingFingerprint(PairingCandidate)
    case pairing
    case paired(PairedHost)
    case failed(CompanionError)
}

public struct PairedHost: Codable, Identifiable, Hashable, Sendable {
    public let hostID: String
    public let name: String
    public let endpoint: URL
    public let fingerprint: String
    public let deviceID: String
    public let credentialReference: String
    public let pairedAtMs: UInt64

    public var id: String { hostID }

    public init(
        hostID: String,
        name: String,
        endpoint: URL,
        fingerprint: String,
        deviceID: String,
        credentialReference: String,
        pairedAtMs: UInt64
    ) {
        self.hostID = hostID
        self.name = name
        self.endpoint = endpoint
        self.fingerprint = fingerprint
        self.deviceID = deviceID
        self.credentialReference = credentialReference
        self.pairedAtMs = pairedAtMs
    }
}

/// A discovery result. `ticket` is deliberately transient: it is never
/// encoded, persisted, logged, put in a URL, or copied into a notification.
public struct PairingCandidate: Identifiable, Equatable, Sendable {
    public let hostID: String
    public let name: String
    public let endpoint: URL
    public let fingerprint: String
    public let ticket: String
    public let expiresAtMs: UInt64

    public var id: String { hostID }

    public init(
        hostID: String,
        name: String,
        endpoint: URL,
        fingerprint: String,
        ticket: String,
        expiresAtMs: UInt64
    ) {
        self.hostID = hostID
        self.name = name
        self.endpoint = endpoint
        self.fingerprint = fingerprint
        self.ticket = ticket
        self.expiresAtMs = expiresAtMs
    }

    public var isExpired: Bool {
        UInt64(Date().timeIntervalSince1970 * 1_000) >= expiresAtMs
    }
}

public struct FingerprintConfirmation: Sendable {
    public let hostID: String
    public let fingerprint: String

    public init(hostID: String, fingerprint: String) {
        self.hostID = hostID
        self.fingerprint = fingerprint
    }
}

public struct PairingReceipt: Sendable {
    public let hostID: String
    public let name: String
    public let endpoint: URL
    public let fingerprint: String
    public let deviceID: String
    public let credential: Data

    public init(
        hostID: String,
        name: String,
        endpoint: URL,
        fingerprint: String,
        deviceID: String,
        credential: Data
    ) {
        self.hostID = hostID
        self.name = name
        self.endpoint = endpoint
        self.fingerprint = fingerprint
        self.deviceID = deviceID
        self.credential = credential
    }
}

public enum CompanionError: Error, Equatable, LocalizedError, Sendable {
    case configurationMissing
    case transportUnavailable
    case credentialRejected
    case hostIdentityChanged
    case hostNotFound
    case pairingExpired
    case invalidPairingInput
    case invalidInput(String)
    case protocolViolation(String)
    case staleSession
    case cancelled

    public var errorDescription: String? {
        switch self {
        case .configurationMissing:
            return "The host integration is not configured."
        case .transportUnavailable:
            return "The host is unavailable."
        case .credentialRejected:
            return "This device is no longer paired with the host."
        case .hostIdentityChanged:
            return "The host identity changed. Pair again only after verifying the new fingerprint."
        case .hostNotFound:
            return "The paired host could not be found."
        case .pairingExpired:
            return "The pairing ticket expired. Request a new ticket from the host."
        case .invalidPairingInput:
            return "That pairing input is not valid."
        case .invalidInput(let message):
            return message
        case .protocolViolation(let message):
            return "The host sent an invalid response: \(message)."
        case .staleSession:
            return "The session changed on the host. It was refreshed before another command can be sent."
        case .cancelled:
            return "The operation was cancelled."
        }
    }
}

public struct SessionSummaryView: Identifiable, Equatable, Sendable {
    public let id: String
    public var title: String
    public var liveState: String
    public var attention: String
    public var pinned: Bool
    public var archived: Bool
    public var modifiedAtMs: UInt64

    public init(
        id: String,
        title: String,
        liveState: String,
        attention: String,
        pinned: Bool,
        archived: Bool,
        modifiedAtMs: UInt64
    ) {
        self.id = id
        self.title = title
        self.liveState = liveState
        self.attention = attention
        self.pinned = pinned
        self.archived = archived
        self.modifiedAtMs = modifiedAtMs
    }
}

public enum TranscriptKind: String, Equatable, Sendable {
    case user
    case assistant
    case reasoning
    case tool
    case system
}

public struct TranscriptEntry: Identifiable, Equatable, Sendable {
    public let id: String
    public var kind: TranscriptKind
    public var text: String
    public var title: String?
    public var summary: String?
    public var target: String?
    public var status: String?
    public var isProvisional: Bool

    public init(
        id: String,
        kind: TranscriptKind,
        text: String = "",
        title: String? = nil,
        summary: String? = nil,
        target: String? = nil,
        status: String? = nil,
        isProvisional: Bool = false
    ) {
        self.id = id
        self.kind = kind
        self.text = text
        self.title = title
        self.summary = summary
        self.target = target
        self.status = status
        self.isProvisional = isProvisional
    }
}

public struct PendingRequestView: Identifiable, Equatable, Sendable {
    public let id: String
    public let actorGeneration: UInt64
    public let kind: String
    public let title: String
    public let choices: [String]
    public let state: String

    public var isPending: Bool { state == "pending" }

    public init(
        id: String,
        actorGeneration: UInt64,
        kind: String,
        title: String,
        choices: [String],
        state: String
    ) {
        self.id = id
        self.actorGeneration = actorGeneration
        self.kind = kind
        self.title = title
        self.choices = choices
        self.state = state
    }
}

public struct SessionProjection: Identifiable, Equatable, Sendable {
    public let id: String
    public var actorGeneration: UInt64
    public var cursor: WireCursor
    public var title: String
    public var liveState: String
    public var activeRunID: String?
    public var modelLabel: String
    public var items: [TranscriptEntry]
    public var pendingRequests: [PendingRequestView]

    public init(
        id: String,
        actorGeneration: UInt64,
        cursor: WireCursor,
        title: String,
        liveState: String,
        activeRunID: String?,
        modelLabel: String,
        items: [TranscriptEntry],
        pendingRequests: [PendingRequestView]
    ) {
        self.id = id
        self.actorGeneration = actorGeneration
        self.cursor = cursor
        self.title = title
        self.liveState = liveState
        self.activeRunID = activeRunID
        self.modelLabel = modelLabel
        self.items = items
        self.pendingRequests = pendingRequests
    }
}

public enum PromptDelivery: String, Sendable {
    case submit
    case steer
    case followUp
}

public struct PendingCommand: Identifiable, Sendable {
    public let id: String
    public let sessionID: String?
    public let actorGeneration: UInt64?
    public let envelope: Data
    public var attempts: Int
    public var inFlight: Bool

    public init(
        id: String,
        sessionID: String?,
        actorGeneration: UInt64?,
        envelope: Data,
        attempts: Int = 0,
        inFlight: Bool = false
    ) {
        self.id = id
        self.sessionID = sessionID
        self.actorGeneration = actorGeneration
        self.envelope = envelope
        self.attempts = attempts
        self.inFlight = inFlight
    }
}
