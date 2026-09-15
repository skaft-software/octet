import Foundation

/// App-owned boundary for the shared OctetServeClient package. The package's
/// concrete API is intentionally not assumed here; the application target
/// supplies a small adapter at integration time.
public struct ServeClientConfiguration: Sendable {
    public let endpoint: URL
    public let hostID: String
    public let fingerprint: String
    public let deviceID: String
    public let credential: Data

    public init(endpoint: URL, hostID: String, fingerprint: String, deviceID: String, credential: Data) throws {
        guard endpoint.scheme?.lowercased() == "https" || endpoint.scheme?.lowercased() == "wss" else {
            throw CompanionError.invalidInput("The companion requires an encrypted host connection.")
        }
        guard !hostID.isEmpty, !fingerprint.isEmpty, !deviceID.isEmpty, !credential.isEmpty else {
            throw CompanionError.configurationMissing
        }
        self.endpoint = endpoint
        self.hostID = hostID
        self.fingerprint = fingerprint
        self.deviceID = deviceID
        self.credential = credential
    }
}

public enum ServeRequest: Sendable {
    case bootstrap(selectedSessionID: String?)
    case replay(sessionID: String, after: WireCursor)
    case command(Data, commandID: String)
}

public protocol ServeClientTransport: Sendable {
    /// The adapter must perform TLS validation and certificate/public-key
    /// pinning before returning. The app checks the resulting identity again.
    func verifyPeerIdentity(hostID: String, fingerprint: String) async throws
    func request(_ request: ServeRequest) async throws -> Data
    func events() -> AsyncThrowingStream<Data, Error>
    func close() async
}

public protocol ServeClientFactory: Sendable {
    func makeClient(configuration: ServeClientConfiguration) async throws -> any ServeClientTransport
}

/// Useful for tests and for the eventual package adapter. It contains no
/// knowledge of an unavailable shared-package initializer or wire API.
public struct ClosureServeClientFactory: ServeClientFactory, Sendable {
    public typealias Builder = @Sendable (ServeClientConfiguration) async throws -> any ServeClientTransport
    private let builder: Builder

    public init(builder: @escaping Builder) {
        self.builder = builder
    }

    public func makeClient(configuration: ServeClientConfiguration) async throws -> any ServeClientTransport {
        try await builder(configuration)
    }
}

public struct UnconfiguredServeClientFactory: ServeClientFactory, Sendable {
    public init() {}

    public func makeClient(configuration: ServeClientConfiguration) async throws -> any ServeClientTransport {
        _ = configuration
        throw CompanionError.configurationMissing
    }
}

public protocol PairingAdapter: Sendable {
    func verifyPeerIdentity(candidate: PairingCandidate) async throws
    func pair(candidate: PairingCandidate, confirmation: FingerprintConfirmation, deviceID: String) async throws -> PairingReceipt
}

public struct ClosurePairingAdapter: PairingAdapter, Sendable {
    public typealias Verifier = @Sendable (PairingCandidate) async throws -> Void
    public typealias Pairer = @Sendable (PairingCandidate, FingerprintConfirmation, String) async throws -> PairingReceipt
    private let verifier: Verifier
    private let pairer: Pairer

    public init(verifier: @escaping Verifier, pairer: @escaping Pairer) {
        self.verifier = verifier
        self.pairer = pairer
    }

    public func verifyPeerIdentity(candidate: PairingCandidate) async throws {
        try await verifier(candidate)
    }

    public func pair(candidate: PairingCandidate, confirmation: FingerprintConfirmation, deviceID: String) async throws -> PairingReceipt {
        try await pairer(candidate, confirmation, deviceID)
    }
}

public struct NoPairingAdapter: PairingAdapter, Sendable {
    public init() {}
    public func verifyPeerIdentity(candidate: PairingCandidate) async throws {
        _ = candidate
        throw CompanionError.configurationMissing
    }
    public func pair(candidate: PairingCandidate, confirmation: FingerprintConfirmation, deviceID: String) async throws -> PairingReceipt {
        _ = candidate
        _ = confirmation
        _ = deviceID
        throw CompanionError.configurationMissing
    }
}
