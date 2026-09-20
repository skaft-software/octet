import Foundation
import XCTest
@testable import OctetCompanion

final class CompanionWireTests: XCTestCase {
    func testServeCamelCaseIdentifiers() throws {
        let host = try CommandEnvelopeEncoder.hostCreateSession(
            hostID: "h", deviceID: "d", commandID: "c", projectID: "p", issuedAtMs: 1
        )
        XCTAssertEqual(Set(try Fixture.decode(host).keys),
                       Set(["protocol", "hostId", "deviceId", "commandId", "issuedAtMs", "command"]))
        XCTAssertEqual(try Fixture.commandData(host)["projectId"] as? String, "p")
        let answer = try CommandEnvelopeEncoder.answerRequest(
            hostID: "h", deviceID: "d", sessionID: "s", commandID: "c",
            actorGeneration: 1, requestID: "r", allowed: true, issuedAtMs: 1
        )
        XCTAssertEqual(Set(try Fixture.decode(answer).keys),
                       Set(["protocol", "hostId", "deviceId", "sessionId", "commandId",
                            "issuedAtMs", "expectedActorGeneration", "command"]))
        XCTAssertEqual(try Fixture.commandData(answer)["requestId"] as? String, "r")
        // This is a Serve-shaped literal, independent of the app's encoder.
        let ack = Data(#"{"protocol":1,"sessionId":"s","commandId":"c","acknowledgedAtMs":1,"cursor":{"actorGeneration":1,"sequence":1},"disposition":{"status":"accepted"}}"#.utf8)
        XCTAssertEqual(try CommandEnvelopeEncoder.inspectSessionAck(
            ack, hostID: "h", sessionID: "s", commandID: "c"), .accepted)
    }

    func testPromptEncodingRequiresBoundedTextAndCarriesTheGeneration() throws {
        XCTAssertThrowsError(try CommandEnvelopeEncoder.sessionPrompt(
            hostID: "host-1",
            deviceID: "device-1",
            sessionID: "session-1",
            commandID: "cmd-1",
            actorGeneration: 1,
            delivery: .submit,
            text: "   "
        ))
        XCTAssertThrowsError(try CommandEnvelopeEncoder.sessionPrompt(
            hostID: "host-1",
            deviceID: "device-1",
            sessionID: "session-1",
            commandID: "cmd-1",
            actorGeneration: 1,
            delivery: .submit,
            text: String(repeating: "x", count: (256 * 1024) + 1)
        ))
        XCTAssertThrowsError(try CommandEnvelopeEncoder.sessionPrompt(
            hostID: "host-1",
            deviceID: "device-1",
            sessionID: "session-1",
            commandID: "cmd-1",
            actorGeneration: 1,
            delivery: .submit,
            text: "hi",
            attachments: (0...8).map { index in
                WireAttachment(handle: "h\(index)", displayName: "f", mediaType: "text/plain", byteLen: 1)
            }
        ))
        XCTAssertThrowsError(try CommandEnvelopeEncoder.sessionPrompt(
            hostID: "host-1",
            deviceID: "device-1",
            sessionID: "session-1",
            commandID: "cmd-1",
            actorGeneration: 0,
            delivery: .submit,
            text: "hi"
        ))

        let envelope = try CommandEnvelopeEncoder.sessionPrompt(
            hostID: "host-1",
            deviceID: "device-1",
            sessionID: "session-1",
            commandID: "cmd-1",
            actorGeneration: 4,
            delivery: .followUp,
            text: "hi"
        )
        XCTAssertEqual(try Fixture.commandType(envelope), "session.followUp")
        XCTAssertEqual(try Fixture.decode(envelope)["expectedActorGeneration"] as? UInt64, 4)
        XCTAssertEqual(try Fixture.decode(envelope)["protocol"] as? UInt16, octetProtocolMajor)
    }

    func testAcceptanceInspectionRequiresMatchingIdentity() throws {
        let envelope = try CommandEnvelopeEncoder.sessionPrompt(
            hostID: "host-1",
            deviceID: "device-1",
            sessionID: "session-1",
            commandID: "cmd-1",
            actorGeneration: 1,
            delivery: .submit,
            text: "hi"
        )
        let accepted = try Fixture.ack(envelope: envelope, commandID: "cmd-1", status: "accepted")
        let rejected = try Fixture.ack(envelope: envelope, commandID: "cmd-1", status: "rejected")
        let unknown = try Fixture.ack(envelope: envelope, commandID: "cmd-1", status: "partial")

        XCTAssertEqual(
            try CommandEnvelopeEncoder.inspectSessionAck(accepted, hostID: "host-1", sessionID: "session-1", commandID: "cmd-1"),
            .accepted
        )
        XCTAssertEqual(
            try CommandEnvelopeEncoder.inspectSessionAck(rejected, hostID: "host-1", sessionID: "session-1", commandID: "cmd-1"),
            .rejected
        )
        XCTAssertThrowsError(
            try CommandEnvelopeEncoder.inspectSessionAck(accepted, hostID: "host-1", sessionID: "session-2", commandID: "cmd-1")
        )
        XCTAssertThrowsError(
            try CommandEnvelopeEncoder.inspectSessionAck(accepted, hostID: "host-1", sessionID: "session-1", commandID: "cmd-2")
        )
        XCTAssertThrowsError(
            try CommandEnvelopeEncoder.inspectHostAck(accepted, hostID: "host-2", commandID: "cmd-1")
        )
        XCTAssertThrowsError(
            try CommandEnvelopeEncoder.inspectSessionAck(unknown, hostID: "host-1", sessionID: "session-1", commandID: "cmd-1")
        )
    }

    func testBootstrapDecoderRefusesForeignHostsAndFutureProtocols() throws {
        let data = Fixture.bootstrap()
        XCTAssertEqual(try WireDecoder.bootstrap(data, expectedHostID: "host-1").sessions.count, 1)
        XCTAssertThrowsError(try WireDecoder.bootstrap(data, expectedHostID: "host-2"))

        var document = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        document["protocol"] = 9
        let future = try JSONSerialization.data(withJSONObject: document)
        XCTAssertThrowsError(try WireDecoder.bootstrap(future, expectedHostID: "host-1"))
    }

    func testUnknownItemPayloadsAreDroppedInsteadOfRendered() throws {
        let unknown: [String: Any] = [
            "id": "item-2",
            "lifecycle": "committed",
            "payload": ["type": "futureUnknownPayload", "data": ["html": "<script>alert(1)</script>"]]
        ]
        let bootstrap = try WireDecoder.bootstrap(
            Fixture.bootstrap(items: [Fixture.item(), unknown]),
            expectedHostID: "host-1"
        )
        let projection = try XCTUnwrap(bootstrap.selectedSession).projection(title: "Fix the build")
        XCTAssertEqual(projection.items.count, 1)
        XCTAssertFalse(projection.items.contains { $0.text.contains("<script>") })
    }
}

final class SessionReducerTests: XCTestCase {
    private func projection(sequence: UInt64 = 3, actorGeneration: UInt64 = 1) -> SessionProjection {
        SessionProjection(
            id: "session-1",
            actorGeneration: actorGeneration,
            cursor: WireCursor(actorGeneration: actorGeneration, sequence: sequence),
            title: "Fix the build",
            liveState: "idle",
            activeRunID: nil,
            modelLabel: "anthropic / claude",
            items: [],
            pendingRequests: []
        )
    }

    private func envelope(sessionID: String = "session-1", actorGeneration: UInt64 = 1, sequence: UInt64, state: String = "running") -> WireEventEnvelope {
        WireEventEnvelope(
            protocolVersion: octetProtocolMajor,
            sessionID: sessionID,
            cursor: WireCursor(actorGeneration: actorGeneration, sequence: sequence),
            event: .object([
                "type": .string("session.stateChanged"),
                "data": .object(["state": .string(state)])
            ])
        )
    }

    func testReducerAppliesContiguousEventsAndIgnoresDuplicates() throws {
        var value = projection()
        XCTAssertEqual(SessionReducer.apply(envelope(sequence: 4), to: &value), .applied)
        XCTAssertEqual(value.liveState, "running")
        XCTAssertEqual(value.cursor.sequence, 4)
        XCTAssertEqual(SessionReducer.apply(envelope(sequence: 4), to: &value), .ignored)
        XCTAssertEqual(value.liveState, "running")
    }

    func testReducerRefusesGapsStaleGenerationsAndForeignSessions() throws {
        var value = projection()
        XCTAssertEqual(SessionReducer.apply(envelope(sequence: 9), to: &value), .needsSnapshot)
        XCTAssertEqual(SessionReducer.apply(envelope(actorGeneration: 2, sequence: 4), to: &value), .needsSnapshot)
        XCTAssertEqual(SessionReducer.apply(envelope(sessionID: "session-2", sequence: 4), to: &value), .needsSnapshot)
        XCTAssertEqual(value.liveState, "idle")
        XCTAssertEqual(value.cursor.sequence, 3)
    }
}

final class CompanionPairingTests: XCTestCase {
    private func makeCoordinator() -> (PairingCoordinator, MemoryCredentialStore, MemoryHostStore) {
        let credentialStore = MemoryCredentialStore()
        let hostStore = MemoryHostStore()
        let pairing = PairingCoordinator(
            credentialStore: credentialStore,
            hostStore: hostStore,
            adapter: AcceptingPairingAdapter()
        )
        return (pairing, credentialStore, hostStore)
    }

    func testPairingRefusesExpiredTicketsMismatchedFingerprintsAndPlaintext() async throws {
        let (pairing, _, hostStore) = makeCoordinator()
        do {
            _ = try await pairing.pair(candidate: Fixture.candidate(expiresAtMs: 1), confirmed: Fixture.confirmation())
            XCTFail("an expired ticket must be refused")
        } catch let error as CompanionError {
            XCTAssertEqual(error, .pairingExpired)
        }
        do {
            _ = try await pairing.pair(
                candidate: Fixture.candidate(fingerprint: "AB:CD:EF"),
                confirmed: Fixture.confirmation(fingerprint: "00:11:22")
            )
            XCTFail("a mismatched fingerprint must be refused")
        } catch let error as CompanionError {
            XCTAssertEqual(error, .hostIdentityChanged)
        }
        do {
            _ = try await pairing.pair(
                candidate: Fixture.candidate(endpoint: "http://host.invalid"),
                confirmed: Fixture.confirmation()
            )
            XCTFail("a plaintext endpoint must be refused")
        } catch let error as CompanionError {
            XCTAssertEqual(error, .invalidPairingInput)
        }
        let hosts = try await hostStore.all()
        XCTAssertTrue(hosts.isEmpty)
    }

    func testPairingStoresOnlyAReferenceAndUnpairRevokesIt() async throws {
        let (pairing, _, hostStore) = makeCoordinator()
        let host = try await Fixture.pair(pairing)
        XCTAssertTrue(host.credentialReference.hasPrefix("credential."))
        XCTAssertFalse(host.credentialReference.contains("super-secret-token"))
        let configuration = try await pairing.configuration(for: host)
        XCTAssertEqual(configuration.deviceID, "device-1")
        XCTAssertEqual(configuration.credential, Data("super-secret-token".utf8))
        XCTAssertEqual(configuration.endpoint.absoluteString, "https://host.invalid:8443")

        try await pairing.unpair(host)
        let hosts = try await hostStore.all()
        XCTAssertTrue(hosts.isEmpty)
        do {
            _ = try await pairing.configuration(for: host)
            XCTFail("an unpaired host must not resolve a credential")
        } catch let error as CompanionError {
            XCTAssertEqual(error, .credentialRejected)
        }
    }
}
