import Foundation
import XCTest
@testable import OctetCompanion

@MainActor
final class CompanionAppModelTests: XCTestCase {
    private func connectedModel(
        bootstrap: Data
    ) async throws -> (model: CompanionAppModel, transport: ScriptedTransport, credentialStore: MemoryCredentialStore) {
        let credentialStore = MemoryCredentialStore()
        let hostStore = MemoryHostStore()
        let pairing = Fixture.makePairing(credentialStore: credentialStore, hostStore: hostStore)
        try await Fixture.pair(pairing)
        let transport = ScriptedTransport(bootstrap: bootstrap)
        let service = CompanionSessionService(
            factory: ClosureServeClientFactory { _ in transport },
            pairing: pairing
        )
        let model = CompanionAppModel(service: service)
        model.startObserving()
        await model.connect()
        await waitUntil("connected snapshot") { model.snapshot.connection == .connected }
        return (model, transport, credentialStore)
    }

    func testUnconfiguredBuildFailsClosedWithoutPairingOrTransport() async throws {
        let credentialStore = MemoryCredentialStore()
        let hostStore = MemoryHostStore()
        XCTAssertFalse(CompanionComposition.isHostBound(factory: UnconfiguredServeClientFactory()))

        let service = CompanionComposition.makeService(
            credentialStore: credentialStore,
            hostStore: hostStore
        )
        let model = CompanionAppModel(service: service)
        model.startObserving()
        await model.connect()
        XCTAssertEqual(model.failure, CompanionError.hostNotFound.errorDescription)
        XCTAssertEqual(model.snapshot.connection, .needsPairing)

        // Pairing is refused as well: the default composition has no host adapter.
        let pairing = PairingCoordinator(credentialStore: credentialStore, hostStore: hostStore)
        do {
            _ = try await pairing.pair(candidate: Fixture.candidate(), confirmed: Fixture.confirmation())
            XCTFail("pairing must be refused without a composed host adapter")
        } catch let error as CompanionError {
            XCTAssertEqual(error, .configurationMissing)
        }
        let hosts = try await pairing.pairedHosts()
        XCTAssertTrue(hosts.isEmpty)
        model.stopObserving()
    }

    func testConnectedHostProjectsSessionsAndDeliversExactlyOnePrompt() async throws {
        let (model, transport, _) = try await connectedModel(bootstrap: Fixture.bootstrap())
        XCTAssertEqual(model.snapshot.host?.name, "Test Host")
        XCTAssertEqual(model.snapshot.sessions.count, 1)
        XCTAssertEqual(model.snapshot.selectedSession?.title, "Fix the build")
        XCTAssertEqual(model.snapshot.selectedSession?.items.first?.text, "hello")
        XCTAssertEqual(transport.bootstrapCount, 1)
        XCTAssertEqual(transport.verifiedIdentityCount, 1)

        // An empty prompt is refused locally and never reaches the transport.
        await model.send()
        XCTAssertEqual(model.failure, "Enter a prompt before sending.")
        XCTAssertTrue(transport.commandEnvelopes.isEmpty)

        model.composer = "please continue"
        await model.send()
        XCTAssertNil(model.failure)
        XCTAssertEqual(model.composer, "")
        XCTAssertEqual(transport.commandEnvelopes.count, 1)
        let envelope = try XCTUnwrap(transport.commandEnvelopes.first)
        XCTAssertEqual(try Fixture.commandType(envelope), "session.submitPrompt")
        XCTAssertEqual(try Fixture.decode(envelope)["hostId"] as? String, "host-1")
        XCTAssertEqual(try Fixture.decode(envelope)["sessionId"] as? String, "session-1")
        XCTAssertEqual(try Fixture.decode(envelope)["expectedActorGeneration"] as? UInt64, 1)
        let input = try Fixture.commandData(envelope)["input"] as? [String: Any]
        XCTAssertEqual(input?["text"] as? String, "please continue")
        await model.disconnect()
        model.stopObserving()
    }

    func testSteerAndFollowUpUseTheirOwnHostCommands() async throws {
        let (model, transport, _) = try await connectedModel(bootstrap: Fixture.bootstrap())
        model.composer = "keep going"
        await model.send(delivery: .steer)
        model.composer = "and then document it"
        await model.send(delivery: .followUp)
        let types = try transport.commandEnvelopes.map { try Fixture.commandType($0) }
        XCTAssertEqual(types, ["session.steer", "session.followUp"])
        await model.disconnect()
        model.stopObserving()
    }

    func testOversizedPromptIsRefusedLocally() async throws {
        let (model, transport, _) = try await connectedModel(bootstrap: Fixture.bootstrap())
        model.composer = String(repeating: "x", count: CompanionAppModel.maxPromptBytes + 1)
        await model.send()
        XCTAssertEqual(model.failure, "The prompt is too long to send.")
        XCTAssertTrue(transport.commandEnvelopes.isEmpty)
        XCTAssertFalse(model.composer.isEmpty)
        await model.disconnect()
        model.stopObserving()
    }

    func testSessionWithoutASelectedSnapshotCannotSend() async throws {
        let (model, transport, _) = try await connectedModel(
            bootstrap: Fixture.bootstrap(includeSelectedSession: false)
        )
        XCTAssertNil(model.snapshot.selectedSession)
        model.composer = "hello?"
        await model.send()
        XCTAssertEqual(model.failure, CompanionError.staleSession.errorDescription)
        XCTAssertTrue(transport.commandEnvelopes.isEmpty)
        await model.disconnect()
        model.stopObserving()
    }

    func testApprovalAnswerRequiresTheHostReportedRequest() async throws {
        let (model, transport, _) = try await connectedModel(
            bootstrap: Fixture.bootstrap(pending: [Fixture.approvalRequest(id: "request-1")])
        )
        let pending = try XCTUnwrap(model.snapshot.selectedSession?.pendingRequests)
        XCTAssertEqual(pending.count, 1)
        XCTAssertEqual(pending.first?.kind, "approval")
        XCTAssertTrue(pending.first?.isPending == true)

        await model.answerApproval(requestID: "unknown-request", allowed: true)
        XCTAssertEqual(model.failure, "That request is no longer pending.")
        XCTAssertTrue(transport.commandEnvelopes.isEmpty)

        await model.answerApproval(requestID: "request-1", allowed: false)
        XCTAssertNil(model.failure)
        let envelope = try XCTUnwrap(transport.commandEnvelopes.first)
        XCTAssertEqual(try Fixture.commandType(envelope), "session.answerRequest")
        let data = try Fixture.commandData(envelope)
        XCTAssertEqual(data["requestId"] as? String, "request-1")
        let answer = data["answer"] as? [String: Any]
        XCTAssertEqual(answer?["type"] as? String, "approval")
        XCTAssertEqual((answer?["data"] as? [String: Any])?["allowed"] as? Bool, false)
        await model.disconnect()
        model.stopObserving()
    }

    func testAnswerInputIsBoundedAndClearedOnlyOnSuccess() async throws {
        let (model, transport, _) = try await connectedModel(
            bootstrap: Fixture.bootstrap(pending: [Fixture.approvalRequest(id: "request-1")])
        )
        await model.answerText(requestID: "request-1")
        XCTAssertEqual(model.failure, "Enter an answer before responding.")

        model.answerInput = String(repeating: "y", count: CompanionAppModel.maxAnswerBytes + 1)
        await model.answerText(requestID: "request-1")
        XCTAssertEqual(model.failure, "The answer is too long to send.")
        XCTAssertTrue(transport.commandEnvelopes.isEmpty)

        model.answerInput = "go ahead"
        await model.answerText(requestID: "request-1")
        XCTAssertNil(model.failure)
        XCTAssertEqual(model.answerInput, "")
        XCTAssertEqual(try Fixture.commandType(XCTUnwrap(transport.commandEnvelopes.first)), "session.answerRequest")
        await model.disconnect()
        model.stopObserving()
    }

    func testStopRequiresAHostReportedActiveRun() async throws {
        let (model, transport, _) = try await connectedModel(bootstrap: Fixture.bootstrap())
        await model.stopRun()
        XCTAssertEqual(model.failure, "There is no active run to stop.")
        XCTAssertTrue(transport.commandEnvelopes.isEmpty)
        await model.disconnect()
        model.stopObserving()

        let (running, runningTransport, _) = try await connectedModel(
            bootstrap: Fixture.bootstrap(liveState: "running", activeRunID: "run-1")
        )
        await running.stopRun()
        XCTAssertNil(running.failure)
        let envelope = try XCTUnwrap(runningTransport.commandEnvelopes.first)
        XCTAssertEqual(try Fixture.commandType(envelope), "session.abort")
        XCTAssertEqual(try Fixture.commandData(envelope)["runId"] as? String, "run-1")
        await running.disconnect()
        running.stopObserving()
    }

    func testRejectedHostCommandSurfacesAndIsNotRetriedInline() async throws {
        let (model, transport, _) = try await connectedModel(bootstrap: Fixture.bootstrap())
        transport.rejectAcks()
        await model.createSession()
        XCTAssertEqual(model.failure, "The host rejected the new session request.")
        XCTAssertEqual(transport.commandEnvelopes.count, 1)
        XCTAssertEqual(try Fixture.commandType(transport.commandEnvelopes[0]), "host.createSession")
        await model.disconnect()
        model.stopObserving()
    }

    func testLiveHostEventUpdatesTheProjection() async throws {
        let (model, transport, _) = try await connectedModel(bootstrap: Fixture.bootstrap())
        transport.emit(Fixture.stateChangedEvent(hostSequence: 1, cursorSequence: 4, state: "running"))
        await waitUntil("live state") { model.snapshot.selectedSession?.liveState == "running" }
        XCTAssertEqual(model.snapshot.selectedSession?.title, "Fix the build")
        await model.disconnect()
        model.stopObserving()
    }

    func testPairingCredentialNeverAppearsInModelState() async throws {
        let (model, transport, _) = try await connectedModel(bootstrap: Fixture.bootstrap())
        XCTAssertEqual(model.snapshot.connection, .connected)
        XCTAssertFalse(String(describing: model.snapshot).contains("super-secret-token"))
        XCTAssertFalse(model.connectionSummary.contains("super-secret-token"))
        XCTAssertNil(model.failure)
        XCTAssertFalse(String(describing: transport.commandEnvelopes).contains("super-secret-token"))
        await model.disconnect()
        model.stopObserving()
    }
}
