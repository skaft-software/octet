import Foundation

#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

/// Wire limits shared with `extensions/octet-serve/src/bounds.rs`.
public enum OctetProtocolLimits {
    public static let commandBytes = 512 * 1024
    public static let eventBytes = 1024 * 1024
    public static let snapshotBytes = 8 * 1024 * 1024
    public static let bootstrapBytes = 12 * 1024 * 1024
    public static let promptBytes = 256 * 1024
    public static let itemTextBytes = 512 * 1024
    public static let publicTextBytes = 64 * 1024
    public static let diagnosticBytes = 8 * 1024
    public static let maximumJSONDepth = 32
}

public enum OctetDecodeError: Error, LocalizedError, Equatable {
    case payloadTooLarge(limit: Int)
    case nestingTooDeep(limit: Int)
    case invalidJSON
    case invalidPublicText(path: String)
    case unknownField(path: String, field: String)
    case missingField(path: String, field: String)
    case invalidValue(path: String, message: String)
    case protocolMismatch(expected: UInt16, actual: UInt16)
    case replayGap
    case unauthorized
    case revoked

    public var errorDescription: String? {
        switch self {
        case let .payloadTooLarge(limit): return "Octet payload exceeds the \(limit)-byte limit."
        case let .nestingTooDeep(limit): return "Octet JSON exceeds the \(limit)-level nesting limit."
        case .invalidJSON: return "Octet payload is not valid JSON."
        case let .invalidPublicText(path): return "Octet field \(path) contains unsafe public text."
        case let .unknownField(path, field): return "Unknown Octet field \(path).\(field)."
        case let .missingField(path, field): return "Missing Octet field \(path).\(field)."
        case let .invalidValue(path, message): return "Invalid Octet value at \(path): \(message)."
        case let .protocolMismatch(expected, actual): return "Unsupported Octet protocol \(actual); expected \(expected)."
        case .replayGap: return "Octet replay history does not cover the requested cursor."
        case .unauthorized: return "The Octet device is not authorized."
        case .revoked: return "The Octet device credential was revoked."
        }
    }
}

/// Inert JSON which may cross the native bridge. It intentionally has no
/// object-to-native coercions or executable interpretation.
public enum OctetJSONValue: Codable, Equatable, Sendable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([OctetJSONValue])
    case object([String: OctetJSONValue])

    public init(from decoder: Decoder) throws {
        let single = try decoder.singleValueContainer()
        if single.decodeNil() {
            self = .null
        } else if let value = try? single.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? single.decode(Double.self) {
            guard value.isFinite else {
                throw OctetDecodeError.invalidValue(path: "json", message: "non-finite number")
            }
            self = .number(value)
        } else if let value = try? single.decode(String.self) {
            self = .string(value)
        } else if let value = try? single.decode([OctetJSONValue].self) {
            self = .array(value)
        } else if let value = try? single.decode([String: OctetJSONValue].self) {
            self = .object(value)
        } else {
            throw OctetDecodeError.invalidJSON
        }
    }

    public func encode(to encoder: Encoder) throws {
        switch self {
        case .null:
            var container = encoder.singleValueContainer()
            try container.encodeNil()
        case let .bool(value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case let .number(value):
            guard value.isFinite else { throw OctetDecodeError.invalidJSON }
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case let .string(value):
            var container = encoder.singleValueContainer()
            try container.encode(value)
        case let .array(value):
            var container = encoder.unkeyedContainer()
            for item in value {
                try container.encode(item)
            }
        case let .object(value):
            var container = encoder.container(keyedBy: DynamicCodingKey.self)
            for (key, item) in value {
                try container.encode(item, forKey: DynamicCodingKey(stringValue: key)!)
            }
        }
    }

    public var objectValue: [String: OctetJSONValue]? {
        guard case let .object(value) = self else { return nil }
        return value
    }
}

public struct DynamicCodingKey: CodingKey, Hashable, Sendable {
    public let stringValue: String
    public let intValue: Int?

    public init?(stringValue: String) {
        self.stringValue = stringValue
        self.intValue = nil
    }

    public init?(intValue: Int) {
        self.stringValue = String(intValue)
        self.intValue = intValue
    }
}

/// Checks every object before Codable mapping. This is deliberately separate
/// from model decoding so raw bridge bodies receive the same depth/text bounds.
public enum OctetJSONBounds {
    public static func validate(_ data: Data, limit: Int) throws {
        guard data.count <= limit else { throw OctetDecodeError.payloadTooLarge(limit: limit) }
        let object: Any
        do {
            object = try JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
        } catch {
            throw OctetDecodeError.invalidJSON
        }
        try validateObject(object, path: "json", depth: 0)
    }

    public static func validate(_ value: OctetJSONValue, limit: Int) throws {
        let encoded = try JSONEncoder().encode(value)
        try validate(encoded, limit: limit)
    }

    private static func validateObject(_ object: Any, path: String, depth: Int) throws {
        guard depth <= OctetProtocolLimits.maximumJSONDepth else {
            throw OctetDecodeError.nestingTooDeep(limit: OctetProtocolLimits.maximumJSONDepth)
        }
        if let string = object as? String {
            try validateText(string, path: path, limit: OctetProtocolLimits.publicTextBytes, multiline: true)
        } else if let array = object as? [Any] {
            for (index, value) in array.enumerated() {
                try validateObject(value, path: "\(path)[\(index)]", depth: depth + 1)
            }
        } else if let dictionary = object as? [String: Any] {
            for (key, value) in dictionary {
                try validateText(key, path: "\(path).<key>", limit: 256, multiline: false)
                try validateObject(value, path: "\(path).\(key)", depth: depth + 1)
            }
        } else if let number = object as? NSNumber {
            guard number.doubleValue.isFinite else {
                throw OctetDecodeError.invalidValue(path: path, message: "non-finite number")
            }
        }
    }

    public static func validateText(_ text: String, path: String, limit: Int, multiline: Bool) throws {
        guard text.utf8.count <= limit else {
            throw OctetDecodeError.payloadTooLarge(limit: limit)
        }
        for scalar in text.unicodeScalars {
            let value = scalar.value
            let directional = value == 0x061c || value == 0x200e || value == 0x200f
                || (0x202a...0x202e).contains(value) || (0x2066...0x2069).contains(value)
            let allowedWhitespace = multiline && (scalar == "\n" || scalar == "\r" || scalar == "\t")
            if directional || (scalar.properties.generalCategory == .control && !allowedWhitespace) {
                throw OctetDecodeError.invalidPublicText(path: path)
            }
        }
    }
}

/// A Codable decoder that performs the protocol-wide recursive boundary check
/// before model decoding. DTOs in this package additionally call
/// `strictContainer` from their custom initializers to reject unknown keys.
public final class OctetJSONDecoder {
    public var decoder: JSONDecoder

    public init() {
        decoder = JSONDecoder()
    }

    public func decode<T: Decodable>(_ type: T.Type, from data: Data, limit: Int = OctetProtocolLimits.eventBytes) throws -> T {
        try OctetJSONBounds.validate(data, limit: limit)
        return try decoder.decode(type, from: data)
    }
}

public final class OctetJSONEncoder {
    public var encoder: JSONEncoder

    public init() {
        encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
    }

    public func encode<T: Encodable>(_ value: T, limit: Int = OctetProtocolLimits.eventBytes) throws -> Data {
        let data = try encoder.encode(value)
        try OctetJSONBounds.validate(data, limit: limit)
        return data
    }
}

/// Keyed decoding helper used by all object DTOs. A dynamic pass is required:
/// `KeyedDecodingContainer<CodingKeys>.allKeys` otherwise hides unknown keys.
public func strictContainer<K: CodingKey>(_ decoder: Decoder, keys: K.Type, allowed: Set<String>, path: String) throws -> KeyedDecodingContainer<K> {
    let dynamic = try decoder.container(keyedBy: DynamicCodingKey.self)
    for key in dynamic.allKeys where !allowed.contains(key.stringValue) {
        throw OctetDecodeError.unknownField(path: path, field: key.stringValue)
    }
    return try decoder.container(keyedBy: keys)
}

public func requireProtocol(_ protocolVersion: UInt16) throws {
    guard protocolVersion == 1 else {
        throw OctetDecodeError.protocolMismatch(expected: 1, actual: protocolVersion)
    }
}

public func requireNonEmpty(_ value: String, path: String, limit: Int = 128) throws {
    guard !value.isEmpty, value.utf8.count <= limit else {
        throw OctetDecodeError.invalidValue(path: path, message: "empty or oversized")
    }
    try OctetJSONBounds.validateText(value, path: path, limit: limit, multiline: false)
}

public func requireIdentifier(_ value: String, path: String) throws {
    try requireNonEmpty(value, path: path, limit: 128)
    guard value.utf8.allSatisfy({ byte in
        (byte >= 48 && byte <= 57) || (byte >= 65 && byte <= 90) ||
        (byte >= 97 && byte <= 122) || byte == 45 || byte == 95 || byte == 46 || byte == 58
    }) else {
        throw OctetDecodeError.invalidValue(path: path, message: "invalid identifier")
    }
}

public func decodeDefault<T: Decodable, K: CodingKey>(_ container: KeyedDecodingContainer<K>, _ type: T.Type, forKey key: K, default value: T) throws -> T {
    try container.decodeIfPresent(type, forKey: key) ?? value
}

public func decodeOptional<T: Decodable, K: CodingKey>(_ container: KeyedDecodingContainer<K>, _ type: T.Type, forKey key: K) throws -> T? {
    try container.decodeIfPresent(type, forKey: key)
}
