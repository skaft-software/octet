import Foundation

public protocol OctetIdentifier: Codable, Hashable, Sendable, CustomStringConvertible {
    var value: String { get }
    init(_ value: String) throws
}

public extension OctetIdentifier {
    var description: String { value }
}

private func validateIdentifier(_ value: String, path: String) throws {
    try requireIdentifier(value, path: path)
}

public struct HostId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "hostId"); self.value = value } }
public struct DeviceId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "deviceId"); self.value = value } }
public struct ProjectId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "projectId"); self.value = value } }
public struct SessionId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "sessionId"); self.value = value } }
public struct RunId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "runId"); self.value = value } }
public struct TurnId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "turnId"); self.value = value } }
public struct ItemId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "itemId"); self.value = value } }
public struct DurableEntryId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "durableEntryId"); self.value = value } }
public struct RequestId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "requestId"); self.value = value } }
public struct CommandId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "commandId"); self.value = value } }
public struct SourceId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "sourceId"); self.value = value } }
public struct ArtifactId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "artifactId"); self.value = value } }
public struct ThemeId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "themeId"); self.value = value } }
public struct DocumentId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "documentId"); self.value = value } }
public struct FileEntryId: OctetIdentifier { public let value: String; public init(_ value: String) throws { try validateIdentifier(value, path: "fileEntryId"); self.value = value } }

private extension OctetIdentifier {
    static func decodeIdentifier(from decoder: Decoder, path: String) throws -> Self {
        let value = try String(from: decoder)
        return try Self(value)
    }
}

// Identifier Codable conformance is intentionally explicit so malformed IDs
// cannot enter the native projection even when a caller uses JSONDecoder.
extension HostId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "hostId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension DeviceId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "deviceId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension ProjectId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "projectId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension SessionId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "sessionId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension RunId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "runId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension TurnId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "turnId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension ItemId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "itemId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension DurableEntryId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "durableEntryId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension RequestId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "requestId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension CommandId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "commandId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension SourceId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "sourceId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension ArtifactId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "artifactId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension ThemeId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "themeId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension DocumentId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "documentId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }
extension FileEntryId: Codable { public init(from decoder: Decoder) throws { self = try Self.decodeIdentifier(from: decoder, path: "fileEntryId") }; public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) } }

public struct RuntimeId: Codable, Hashable, Sendable, CustomStringConvertible {
    public let value: String
    public init(_ value: String) throws {
        try requireNonEmpty(value, path: "runtimeId")
        guard value.utf8.allSatisfy({ ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 90) || ($0 >= 97 && $0 <= 122) || $0 == 45 || $0 == 46 || $0 == 95 || $0 == 58 }) else {
            throw OctetDecodeError.invalidValue(path: "runtimeId", message: "invalid runtime identifier")
        }
        self.value = value
    }
    public var description: String { value }
    public init(from decoder: Decoder) throws { try self.init(decoder.singleValueContainer().decode(String.self)) }
    public func encode(to encoder: Encoder) throws { var c = encoder.singleValueContainer(); try c.encode(value) }
}

public struct CatalogCursor: Codable, Hashable, Sendable { public let value: UInt64; public init(_ value: UInt64) { self.value = value } }

public struct SessionCursor: Codable, Equatable, Hashable, Sendable {
    public let actorGeneration: UInt64
    public let sequence: UInt64
    public init(actorGeneration: UInt64, sequence: UInt64) { self.actorGeneration = actorGeneration; self.sequence = sequence }

    private enum CodingKeys: String, CodingKey { case actorGeneration, sequence }
    public init(from decoder: Decoder) throws {
        let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["actorGeneration", "sequence"], path: "cursor")
        actorGeneration = try c.decode(UInt64.self, forKey: .actorGeneration)
        sequence = try c.decode(UInt64.self, forKey: .sequence)
    }
}

internal func strictObject(_ decoder: Decoder, _ fields: [String], _ path: String) throws -> KeyedDecodingContainer<DynamicCodingKey> {
    try strictContainer(decoder, keys: DynamicCodingKey.self, allowed: Set(fields), path: path)
}

internal func dynamicKey(_ value: String) -> DynamicCodingKey { DynamicCodingKey(stringValue: value)! }

public enum AuthorityProfile: String, Codable, CaseIterable, Sendable { case readOnly, workspace, fullAccess }
public enum SessionLiveState: String, Codable, Sendable { case idle, working, needsApproval, needsInput, done, failed, stopped, offline, locked }
public enum AttentionState: String, Codable, Sendable { case none, unreadCompletion, approval, input, failure }
public enum PullRequestState: String, Codable, Sendable { case inProgress, ready, merged }
public enum ActorOwnerState: String, Codable, Sendable { case inactive, hosted, externallyLocked }
public enum InputModality: String, Codable, Sendable { case text, image, audio, document }
public enum SessionCatalogState: String, Codable, Sendable { case active, archived, trash }
public enum ConversationBranchOperation: String, Codable, Sendable { case editUserTurn, retryResponse, forkSession }
public enum SessionBranchEntryKind: String, Codable, Sendable { case userMessage, assistantMessage, compaction, `internal` }
public enum ItemLifecycle: String, Codable, Sendable { case provisional, committed }
public enum PlanStepState: String, Codable, Sendable { case pending, inProgress, completed, blocked }
public enum SourceKind: String, Codable, Sendable { case attachment, file, web, resource, other }
public enum ArtifactKind: String, Codable, Sendable { case file, image, document, spreadsheet, presentation, site, other }
public enum RunOutcome: String, Codable, Sendable { case completed, stopped, failed }
public enum ToolKind: String, Codable, Sendable { case read, search, edit, write, command, web, skill, other }
public enum ActivityPhase: String, Codable, Sendable { case investigated, changed, verified, produced, other }
public enum ToolActivityStatus: String, Codable, Sendable { case running, succeeded, failed, stopped }
public enum EvidenceCoverage: String, Codable, Sendable { case none, partial, complete }
public enum UserMessageDelivery: String, Codable, Sendable { case submit, steer, followUp }
public enum RequestState: String, Codable, Sendable { case pending, resolved, denied, expired }
public enum GoalStatus: String, Codable, Sendable { case active, paused, complete, blocked, budgetLimited }
public enum GoalAction: String, Codable, Sendable { case pause, resume, clear }

public struct GoalState: Codable, Equatable, Sendable {
    public let revision: UInt64
    public let objective: String
    public let status: GoalStatus
    public let turnBudget: UInt32?
    public let turnsUsed: UInt32
    public let createdAt: String
    private enum CodingKeys: String, CodingKey { case revision, objective, status, turnBudget, turnsUsed, createdAt }
    public init(revision: UInt64 = 0, objective: String, status: GoalStatus, turnBudget: UInt32?, turnsUsed: UInt32, createdAt: String) {
        self.revision = revision; self.objective = objective; self.status = status; self.turnBudget = turnBudget; self.turnsUsed = turnsUsed; self.createdAt = createdAt
    }
    public init(from decoder: Decoder) throws {
        let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["revision", "objective", "status", "turnBudget", "turnsUsed", "createdAt"], path: "goal")
        revision = try decodeDefault(c, UInt64.self, forKey: .revision, default: 0)
        objective = try c.decode(String.self, forKey: .objective)
        status = try c.decode(GoalStatus.self, forKey: .status)
        turnBudget = try c.decodeIfPresent(UInt32.self, forKey: .turnBudget)
        turnsUsed = try c.decode(UInt32.self, forKey: .turnsUsed)
        createdAt = try c.decode(String.self, forKey: .createdAt)
        try OctetJSONBounds.validateText(objective, path: "goal.objective", limit: 64 * 1024, multiline: false)
    }
}

public struct GoalMutation: Codable, Equatable, Sendable {
    public let objective: String?
    public let turnBudget: UInt32?
    public let action: GoalAction?
    public init(objective: String? = nil, turnBudget: UInt32? = nil, action: GoalAction? = nil) { self.objective = objective; self.turnBudget = turnBudget; self.action = action }
    private enum CodingKeys: String, CodingKey { case objective, turnBudget, action }
    public init(from decoder: Decoder) throws {
        let c = try strictContainer(decoder, keys: CodingKeys.self, allowed: ["objective", "turnBudget", "action"], path: "goalMutation")
        objective = try c.decodeIfPresent(String.self, forKey: .objective)
        turnBudget = try c.decodeIfPresent(UInt32.self, forKey: .turnBudget)
        action = try c.decodeIfPresent(GoalAction.self, forKey: .action)
        guard (objective != nil) != (action != nil) else { throw OctetDecodeError.invalidValue(path: "goalMutation", message: "must be a goal set or action") }
    }
}
