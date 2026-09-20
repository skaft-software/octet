import Foundation
import XCTest
@testable import OctetCompanion

// MARK: - Deterministic doubles

private extension NSLock {
    /// Scoped locking keeps `lock()`/`unlock()` out of asynchronous contexts,
    /// which Swift warns about (and rejects in Swift 6 language mode).
    @discardableResult
    func withCriticalSection<T>(_ body: () -> T) -> T {
        lock()
        defer { unlock() }
        return body()
    }
}

final class MemoryCredentialStore: CompanionCredentialStore, @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Data] = [:]

    func save(_ credential: Data, reference: String) async throws {
        lock.withCriticalSection { values[reference] = credential }
    }

    func load(reference: String) async throws -> Data {
        let value = lock.withCriticalSection { values[reference] }
        guard let value, !value.isEmpty else { throw CompanionError.credentialRejected }
        return value
    }

    func delete(reference: String) async throws {
        lock.withCriticalSection { values.removeValue(forKey: reference) }
    }

    func deviceID() async throws -> String { "device-1" }
}

final class MemoryHostStore: PairedHostStore, @unchecked Sendable {
    private let lock = NSLock()
    private var hosts: [String: PairedHost] = [:]

    func all() async throws -> [PairedHost] {
        lock.withCriticalSection { hosts.keys.sorted().compactMap { hosts[$0] } }
    }

    func save(_ host: PairedHost) async throws {
        lock.withCriticalSection { hosts[host.hostID] = host }
    }

    func remove(hostID: String) async throws {
        lock.withCriticalSection { hosts.removeValue(forKey: hostID) }
    }
}

struct AcceptingPairingAdapter: PairingAdapter {
    func verifyPeerIdentity(candidate: PairingCandidate) async throws {}

    func pair(candidate: PairingCandidate, confirmation: FingerprintConfirmation, deviceID: String) async throws -> PairingReceipt {
        PairingReceipt(
            hostID: candidate.hostID,
            name: candidate.name,
            endpoint: candidate.endpoint,
            fingerprint: candidate.fingerprint,
            deviceID: deviceID,
            credential: Data("super-secret-token".utf8)
        )
    }
}

/// A scripted Serve transport. It answers `bootstrap` with a fixture document and
/// echoes the identity of every command envelope back as an acknowledgement, so
/// the service's own ack inspection is exercised end to end.
final class ScriptedTransport: ServeClientTransport, @unchecked Sendable {
    private let lock = NSLock()
    private let bootstrap: Data
    private var requests: [ServeRequest] = []
    private var verifications: [(hostID: String, fingerprint: String)] = []
    private var continuations: [AsyncThrowingStream<Data, Error>.Continuation] = []
    private var closed = false
    private var ackStatus = "accepted"

    init(bootstrap: Data) {
        self.bootstrap = bootstrap
    }

    var isClosed: Bool {
        lock.withCriticalSection { closed }
    }

    var commandEnvelopes: [Data] {
        recorded.compactMap { request in
            if case .command(let data, _) = request { return data }
            return nil
        }
    }

    var bootstrapCount: Int {
        recorded.filter { request in
            if case .bootstrap = request { return true }
            return false
        }.count
    }

    var verifiedIdentityCount: Int {
        lock.withCriticalSection { verifications.count }
    }

    var recorded: [ServeRequest] {
        lock.withCriticalSection { requests }
    }

    func rejectAcks() {
        lock.withCriticalSection { ackStatus = "rejected" }
    }

    func verifyPeerIdentity(hostID: String, fingerprint: String) async throws {
        lock.withCriticalSection { verifications.append((hostID, fingerprint)) }
    }

    func request(_ request: ServeRequest) async throws -> Data {
        let status = lock.withCriticalSection { () -> String in
            requests.append(request)
            return ackStatus
        }
        switch request {
        case .bootstrap:
            return bootstrap
        case .command(let payload, let commandID):
            return try Fixture.ack(envelope: payload, commandID: commandID, status: status)
        case .replay:
            return Data(#"{"type":"events","events":[]}"#.utf8)
        }
    }

    func events() -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream { continuation in
            let alreadyClosed = lock.withCriticalSection { () -> Bool in
                continuations.append(continuation)
                return closed
            }
            if alreadyClosed { continuation.finish() }
        }
    }

    func emit(_ data: Data) {
        let streams = lock.withCriticalSection { continuations }
        for stream in streams { stream.yield(data) }
    }

    func close() async {
        let streams = lock.withCriticalSection { () -> [AsyncThrowingStream<Data, Error>.Continuation] in
            closed = true
            let pending = continuations
            continuations = []
            return pending
        }
        for stream in streams { stream.finish() }
    }
}

// MARK: - Wire fixtures

enum Fixture {
    static func timestampMs() -> UInt64 { UInt64(max(0, Date().timeIntervalSince1970 * 1_000)) }

    static func candidate(
        hostID: String = "host-1",
        endpoint: String = "https://host.invalid:8443",
        fingerprint: String = "AB:CD:EF",
        expiresAtMs: UInt64? = nil
    ) -> PairingCandidate {
        PairingCandidate(
            hostID: hostID,
            name: "Test Host",
            endpoint: URL(string: endpoint)!,
            fingerprint: fingerprint,
            ticket: "ticket-1",
            expiresAtMs: expiresAtMs ?? timestampMs() + 60_000
        )
    }

    static func confirmation(hostID: String = "host-1", fingerprint: String = "ab:cd:ef") -> FingerprintConfirmation {
        FingerprintConfirmation(hostID: hostID, fingerprint: fingerprint)
    }

    static func item(id: String = "item-1", text: String = "hello", lifecycle: String = "committed") -> [String: Any] {
        [
            "id": id,
            "lifecycle": lifecycle,
            "payload": ["type": "assistantMessage", "data": ["text": text]]
        ]
    }

    static func approvalRequest(id: String = "request-1", actorGeneration: UInt64 = 1) -> [String: Any] {
        [
            "id": id,
            "actorGeneration": actorGeneration,
            "kind": ["type": "approval", "data": ["action": "write file"]],
            "state": "pending"
        ]
    }

    static func bootstrap(
        hostID: String = "host-1",
        sessionID: String = "session-1",
        actorGeneration: UInt64 = 1,
        cursorSequence: UInt64 = 3,
        liveState: String = "idle",
        activeRunID: String? = nil,
        items: [[String: Any]] = [item()],
        pending: [[String: Any]] = [],
        includeSelectedSession: Bool = true
    ) -> Data {
        let model: [String: Any] = ["provider": "anthropic", "model": "claude", "reasoning": ""]
        var document: [String: Any] = [
            "protocol": 1,
            "host": ["id": hostID, "name": "Test Host"],
            "catalogCursor": 0,
            "sessions": [[
                "id": sessionID,
                "title": "Fix the build",
                "modifiedAtMs": 1,
                "pinned": false,
                "archived": false,
                "provisional": false,
                "liveState": liveState,
                "attention": "none",
                "model": model
            ]],
            "selectedSessionId": sessionID
        ]
        if includeSelectedSession {
            var snapshot: [String: Any] = [
                "sessionId": sessionID,
                "actorGeneration": actorGeneration,
                "cursor": ["actorGeneration": actorGeneration, "sequence": cursorSequence],
                "liveState": liveState,
                "model": model,
                "items": items,
                "pendingRequests": pending
            ]
            if let activeRunID { snapshot["activeRunId"] = activeRunID }
            document["selectedSession"] = snapshot
        }
        return try! JSONSerialization.data(withJSONObject: document, options: [.sortedKeys])
    }

    static func stateChangedEvent(
        hostSequence: UInt64,
        cursorSequence: UInt64,
        state: String,
        sessionID: String = "session-1",
        actorGeneration: UInt64 = 1
    ) -> Data {
        let document: [String: Any] = [
            "protocol": 1,
            "hostSequence": hostSequence,
            "event": [
                "protocol": 1,
                "sessionId": sessionID,
                "cursor": ["actorGeneration": actorGeneration, "sequence": cursorSequence],
                "event": ["type": "session.stateChanged", "data": ["state": state]]
            ]
        ]
        return try! JSONSerialization.data(withJSONObject: document, options: [.sortedKeys])
    }

    /// Echoes the envelope identity, exactly like the host contract requires.
    static func ack(envelope: Data, commandID: String, status: String) throws -> Data {
        let object = (try? JSONSerialization.jsonObject(with: envelope)) as? [String: Any] ?? [:]
        var ack: [String: Any] = [
            "protocol": 1,
            "commandId": commandID,
            "disposition": ["status": status]
        ]
        if let hostID = object["hostId"] as? String { ack["hostId"] = hostID }
        if let sessionID = object["sessionId"] as? String { ack["sessionId"] = sessionID }
        return try JSONSerialization.data(withJSONObject: ack, options: [.sortedKeys])
    }

    static func decode(_ envelope: Data) throws -> [String: Any] {
        (try JSONSerialization.jsonObject(with: envelope)) as? [String: Any] ?? [:]
    }

    static func commandType(_ envelope: Data) throws -> String {
        try decode(envelope)["command"].flatMap { ($0 as? [String: Any])?["type"] as? String } ?? ""
    }

    static func commandData(_ envelope: Data) throws -> [String: Any] {
        try decode(envelope)["command"].flatMap { ($0 as? [String: Any])?["data"] as? [String: Any] } ?? [:]
    }

    static func makePairing(credentialStore: MemoryCredentialStore, hostStore: MemoryHostStore) -> PairingCoordinator {
        PairingCoordinator(
            credentialStore: credentialStore,
            hostStore: hostStore,
            adapter: AcceptingPairingAdapter()
        )
    }

    @discardableResult
    static func pair(_ pairing: PairingCoordinator, hostID: String = "host-1") async throws -> PairedHost {
        try await pairing.pair(
            candidate: candidate(hostID: hostID),
            confirmed: confirmation(hostID: hostID)
        )
    }
}

extension XCTestCase {
    /// Bounded polling so a test never hangs on a host-driven state transition.
    func waitUntil(
        _ description: String,
        timeout: TimeInterval = 5,
        _ predicate: () async -> Bool
    ) async {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if await predicate() { return }
            try? await Task.sleep(nanoseconds: 10_000_000)
        }
        XCTFail("timed out waiting for \(description)")
    }
}
