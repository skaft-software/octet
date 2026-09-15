import Foundation

public struct WireAttachment: Equatable, Sendable {
    public let handle: String
    public let displayName: String
    public let mediaType: String
    public let byteLen: UInt64

    public init(handle: String, displayName: String, mediaType: String, byteLen: UInt64) {
        self.handle = handle
        self.displayName = displayName
        self.mediaType = mediaType
        self.byteLen = byteLen
    }
}

public enum AuthoritySelection: String, CaseIterable, Sendable {
    case readOnly
    case workspace
    case fullAccess
}

public enum AckInspection: Equatable, Sendable {
    case accepted
    case rejected
}

private struct PromptBody: Encodable {
    let text: String
    let attachments: [WireAttachment]
    let documentIDs: [String]
    let projectFileIDs: [String]

    enum CodingKeys: String, CodingKey {
        case text
        case attachments
        case documentIDs = "documentIds"
        case projectFileIDs = "projectFileIds"
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(text, forKey: .text)
        try container.encode(attachments.map { AttachmentBody($0) }, forKey: .attachments)
        try container.encodeIfNotEmpty(documentIDs, forKey: .documentIDs)
        try container.encodeIfNotEmpty(projectFileIDs, forKey: .projectFileIDs)
    }
}

private extension KeyedEncodingContainer where Key: CodingKey {
    func encodeIfNotEmpty<T: Encodable>(_ value: [T], forKey key: Key) throws {
        if !value.isEmpty { try encode(value, forKey: key) }
    }
}

private struct AttachmentBody: Encodable {
    let attachment: WireAttachment

    init(_ attachment: WireAttachment) { self.attachment = attachment }

    enum CodingKeys: String, CodingKey {
        case handle
        case displayName
        case mediaType
        case byteLen
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(attachment.handle, forKey: .handle)
        try container.encode(attachment.displayName, forKey: .displayName)
        try container.encode(attachment.mediaType, forKey: .mediaType)
        try container.encode(attachment.byteLen, forKey: .byteLen)
    }
}

private struct ModelBody: Encodable {
    let provider: String
    let model: String
    let reasoning: String
}

private enum WireCommand: Encodable {
    case createSession(projectID: String?, authority: AuthoritySelection, model: ModelBody?)
    case prompt(type: String, input: PromptBody)
    case answerRequest(requestID: String, answer: WireAnswer)
    case abort(runID: String?)

    enum CodingKeys: String, CodingKey {
        case type
        case data
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .createSession(let projectID, let authority, let model):
            try container.encode("host.createSession", forKey: .type)
            var data = container.nestedContainer(keyedBy: CreateKeys.self, forKey: .data)
            try data.encodeIfPresent(projectID, forKey: .projectID)
            try data.encode(authority.rawValue, forKey: .authority)
            try data.encodeIfPresent(model, forKey: .model)
        case .prompt(let type, let input):
            try container.encode(type, forKey: .type)
            var data = container.nestedContainer(keyedBy: PromptKeys.self, forKey: .data)
            try data.encode(input, forKey: .input)
        case .answerRequest(let requestID, let answer):
            try container.encode("session.answerRequest", forKey: .type)
            var data = container.nestedContainer(keyedBy: AnswerRequestKeys.self, forKey: .data)
            try data.encode(requestID, forKey: .requestID)
            try data.encode(answer, forKey: .answer)
        case .abort(let runID):
            try container.encode("session.abort", forKey: .type)
            var data = container.nestedContainer(keyedBy: AbortKeys.self, forKey: .data)
            try data.encodeIfPresent(runID, forKey: .runID)
        }
    }

    private enum CreateKeys: String, CodingKey {
        case projectID
        case authority
        case model
    }

    private enum PromptKeys: String, CodingKey {
        case input
    }

    private enum AnswerRequestKeys: String, CodingKey {
        case requestID
        case answer
    }

    private enum AbortKeys: String, CodingKey {
        case runID
    }
}

private enum WireAnswer: Encodable {
    case approval(allowed: Bool)
    case text(String)
    case choice(String)

    enum CodingKeys: String, CodingKey { case type, data }
    enum ApprovalKeys: String, CodingKey { case allowed }
    enum TextKeys: String, CodingKey { case text }
    enum ChoiceKeys: String, CodingKey { case choice }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .approval(let allowed):
            try container.encode("approval", forKey: .type)
            var data = container.nestedContainer(keyedBy: ApprovalKeys.self, forKey: .data)
            try data.encode(allowed, forKey: .allowed)
        case .text(let text):
            try container.encode("text", forKey: .type)
            var data = container.nestedContainer(keyedBy: TextKeys.self, forKey: .data)
            try data.encode(text, forKey: .text)
        case .choice(let choice):
            try container.encode("choice", forKey: .type)
            var data = container.nestedContainer(keyedBy: ChoiceKeys.self, forKey: .data)
            try data.encode(choice, forKey: .choice)
        }
    }
}

private struct HostEnvelope: Encodable {
    let protocolVersion: UInt16
    let hostID: String
    let deviceID: String
    let commandID: String
    let issuedAtMs: UInt64
    let command: WireCommand

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol"
        case hostID
        case deviceID
        case commandID
        case issuedAtMs
        case command
    }
}

private struct SessionEnvelope: Encodable {
    let protocolVersion: UInt16
    let hostID: String
    let deviceID: String
    let sessionID: String
    let commandID: String
    let issuedAtMs: UInt64
    let expectedActorGeneration: UInt64?
    let command: WireCommand

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol"
        case hostID
        case deviceID
        case sessionID
        case commandID
        case issuedAtMs
        case expectedActorGeneration
        case command
    }
}

public enum CommandEnvelopeEncoder {
    private static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }()

    public static func newCommandID() -> String {
        UUID().uuidString.lowercased()
    }

    public static func hostCreateSession(
        hostID: String,
        deviceID: String,
        commandID: String,
        projectID: String? = nil,
        authority: AuthoritySelection = .workspace,
        provider: String? = nil,
        model: String? = nil,
        reasoning: String? = nil,
        issuedAtMs: UInt64 = Self.nowMs()
    ) throws -> Data {
        try requireIdentity(hostID, field: "host")
        try requireIdentity(deviceID, field: "device")
        try requireIdentity(commandID, field: "command")
        let selection: ModelBody?
        if provider != nil || model != nil || reasoning != nil {
            selection = ModelBody(provider: provider ?? "", model: model ?? "", reasoning: reasoning ?? "")
        } else {
            selection = nil
        }
        return try encoder.encode(HostEnvelope(
            protocolVersion: octetProtocolMajor,
            hostID: hostID,
            deviceID: deviceID,
            commandID: commandID,
            issuedAtMs: issuedAtMs,
            command: .createSession(projectID: projectID, authority: authority, model: selection)
        ))
    }

    public static func sessionPrompt(
        hostID: String,
        deviceID: String,
        sessionID: String,
        commandID: String,
        actorGeneration: UInt64,
        delivery: PromptDelivery,
        text: String,
        attachments: [WireAttachment] = [],
        documentIDs: [String] = [],
        projectFileIDs: [String] = [],
        issuedAtMs: UInt64 = Self.nowMs()
    ) throws -> Data {
        try requireIdentity(hostID, field: "host")
        try requireIdentity(deviceID, field: "device")
        try requireIdentity(sessionID, field: "session")
        try requireIdentity(commandID, field: "command")
        guard actorGeneration > 0 else { throw CompanionError.invalidInput("The session owner generation is invalid.") }
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !attachments.isEmpty else {
            throw CompanionError.invalidInput("Enter a prompt or attach a file.")
        }
        guard text.utf8.count <= 256 * 1024 else { throw CompanionError.invalidInput("The prompt is too long.") }
        guard attachments.count <= 8 else { throw CompanionError.invalidInput("Too many attachments.") }
        let type: String
        switch delivery {
        case .submit: type = "session.submitPrompt"
        case .steer: type = "session.steer"
        case .followUp: type = "session.followUp"
        }
        return try encoder.encode(SessionEnvelope(
            protocolVersion: octetProtocolMajor,
            hostID: hostID,
            deviceID: deviceID,
            sessionID: sessionID,
            commandID: commandID,
            issuedAtMs: issuedAtMs,
            expectedActorGeneration: actorGeneration,
            command: .prompt(type: type, input: PromptBody(
                text: text,
                attachments: attachments,
                documentIDs: documentIDs,
                projectFileIDs: projectFileIDs
            ))
        ))
    }

    public static func answerRequest(
        hostID: String,
        deviceID: String,
        sessionID: String,
        commandID: String,
        actorGeneration: UInt64,
        requestID: String,
        allowed: Bool,
        issuedAtMs: UInt64 = Self.nowMs()
    ) throws -> Data {
        try sessionEnvelope(
            hostID: hostID,
            deviceID: deviceID,
            sessionID: sessionID,
            commandID: commandID,
            actorGeneration: actorGeneration,
            command: .answerRequest(requestID: requestID, answer: .approval(allowed: allowed)),
            issuedAtMs: issuedAtMs
        )
    }

    public static func answerTextRequest(
        hostID: String,
        deviceID: String,
        sessionID: String,
        commandID: String,
        actorGeneration: UInt64,
        requestID: String,
        text: String,
        issuedAtMs: UInt64 = Self.nowMs()
    ) throws -> Data {
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw CompanionError.invalidInput("Enter an answer.")
        }
        guard text.utf8.count <= 256 * 1024 else { throw CompanionError.invalidInput("The answer is too long.") }
        return try sessionEnvelope(
            hostID: hostID,
            deviceID: deviceID,
            sessionID: sessionID,
            commandID: commandID,
            actorGeneration: actorGeneration,
            command: .answerRequest(requestID: requestID, answer: .text(text)),
            issuedAtMs: issuedAtMs
        )
    }

    public static func stop(
        hostID: String,
        deviceID: String,
        sessionID: String,
        commandID: String,
        actorGeneration: UInt64,
        runID: String?,
        issuedAtMs: UInt64 = Self.nowMs()
    ) throws -> Data {
        try sessionEnvelope(
            hostID: hostID,
            deviceID: deviceID,
            sessionID: sessionID,
            commandID: commandID,
            actorGeneration: actorGeneration,
            command: .abort(runID: runID),
            issuedAtMs: issuedAtMs
        )
    }

    public static func inspectHostAck(_ data: Data, hostID: String, commandID: String) throws -> AckInspection {
        let value = try ackObject(data)
        try validateAck(value, protocolKey: "protocol", expectedHostID: hostID, expectedCommandID: commandID, sessionID: nil)
        return try disposition(value)
    }

    public static func inspectSessionAck(_ data: Data, hostID: String, sessionID: String, commandID: String) throws -> AckInspection {
        let value = try ackObject(data)
        try validateAck(value, protocolKey: "protocol", expectedHostID: hostID, expectedCommandID: commandID, sessionID: sessionID)
        return try disposition(value)
    }

    private static func sessionEnvelope(
        hostID: String,
        deviceID: String,
        sessionID: String,
        commandID: String,
        actorGeneration: UInt64,
        command: WireCommand,
        issuedAtMs: UInt64
    ) throws -> Data {
        try requireIdentity(hostID, field: "host")
        try requireIdentity(deviceID, field: "device")
        try requireIdentity(sessionID, field: "session")
        try requireIdentity(commandID, field: "command")
        guard actorGeneration > 0 else { throw CompanionError.invalidInput("The session owner generation is invalid.") }
        return try encoder.encode(SessionEnvelope(
            protocolVersion: octetProtocolMajor,
            hostID: hostID,
            deviceID: deviceID,
            sessionID: sessionID,
            commandID: commandID,
            issuedAtMs: issuedAtMs,
            expectedActorGeneration: actorGeneration,
            command: command
        ))
    }

    private static func requireIdentity(_ value: String, field: String) throws {
        guard !value.isEmpty, value.utf8.count <= 512,
              !value.contains("\n"), !value.contains("\r") else {
            throw CompanionError.invalidInput("The \(field) identity is invalid.")
        }
    }

    private static func nowMs() -> UInt64 {
        UInt64(max(0, Date().timeIntervalSince1970 * 1_000))
    }

    private static func ackObject(_ data: Data) throws -> [String: WireJSONValue] {
        guard data.count <= 256 * 1024 else { throw CompanionError.protocolViolation("acknowledgement is too large") }
        let value = try JSONDecoder().decode(WireJSONValue.self, from: data)
        guard let object = value.objectValue else { throw CompanionError.protocolViolation("acknowledgement is not an object") }
        return object
    }

    private static func validateAck(
        _ value: [String: WireJSONValue],
        protocolKey: String,
        expectedHostID: String,
        expectedCommandID: String,
        sessionID: String?
    ) throws {
        guard value[protocolKey]?.uint64Value == UInt64(octetProtocolMajor),
              value["commandID"]?.stringValue == expectedCommandID else {
            throw CompanionError.protocolViolation("acknowledgement identity mismatch")
        }
        if let sessionID, value["sessionID"]?.stringValue != sessionID {
            throw CompanionError.protocolViolation("acknowledgement session mismatch")
        }
        if let ackHostID = value["hostID"]?.stringValue, ackHostID != expectedHostID {
            throw CompanionError.hostIdentityChanged
        }
    }

    private static func disposition(_ value: [String: WireJSONValue]) throws -> AckInspection {
        guard let disposition = value["disposition"]?.objectValue,
              let status = disposition["status"]?.stringValue else {
            throw CompanionError.protocolViolation("acknowledgement has no disposition")
        }
        switch status {
        case "accepted": return .accepted
        case "rejected": return .rejected
        default: throw CompanionError.protocolViolation("unknown acknowledgement status")
        }
    }
}
