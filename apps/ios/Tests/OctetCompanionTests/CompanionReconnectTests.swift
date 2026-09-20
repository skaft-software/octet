import Foundation
import XCTest
@testable import OctetCompanion

private actor RetryingFactory: ServeClientFactory {
    private let bootstrap: Data
    private let holdSecond: Bool
    let secondTransport: ScriptedTransport
    private var release: CheckedContinuation<Void, Never>?
    private(set) var attempts = 0

    init(bootstrap: Data, holdSecond: Bool = false) {
        self.bootstrap = bootstrap
        self.holdSecond = holdSecond
        self.secondTransport = ScriptedTransport(bootstrap: bootstrap)
    }

    func releaseSecondAttempt() {
        release?.resume()
        release = nil
    }

    func makeClient(configuration: ServeClientConfiguration) async throws -> any ServeClientTransport {
        attempts += 1
        if attempts == 2 {
            if !holdSecond { throw CompanionError.transportUnavailable }
            await withCheckedContinuation { release = $0 }
            return secondTransport
        }
        return ScriptedTransport(bootstrap: bootstrap)
    }
}

final class CompanionReconnectTests: XCTestCase {
    func testAutomaticReconnectSurvivesAFailedAttempt() async throws {
        let pairing = Fixture.makePairing(credentialStore: MemoryCredentialStore(), hostStore: MemoryHostStore())
        try await Fixture.pair(pairing)
        let factory = RetryingFactory(bootstrap: Fixture.bootstrap())
        let service = CompanionSessionService(factory: factory, pairing: pairing)
        try await service.connect()
        await service.enterBackground()
        await service.enterForeground()
        await waitUntil("second automatic attempt succeeds") {
            let attempts = await factory.attempts
            let snapshot = await service.snapshot()
            return attempts == 3 && snapshot.connection == .connected
        }
        await service.disconnect()
    }

    func testDisconnectDuringEstablishmentDoesNotPublishAFailedOrConnectedState() async throws {
        let pairing = Fixture.makePairing(credentialStore: MemoryCredentialStore(), hostStore: MemoryHostStore())
        try await Fixture.pair(pairing)
        let factory = RetryingFactory(bootstrap: Fixture.bootstrap(), holdSecond: true)
        let service = CompanionSessionService(factory: factory, pairing: pairing)
        try await service.connect()
        await service.enterBackground()
        await service.enterForeground()
        await waitUntil("retry is establishing") { await factory.attempts == 2 }
        await service.disconnect()
        await factory.releaseSecondAttempt()
        await waitUntil("cancelled retry closes its own transport") { factory.secondTransport.isClosed }
        let snapshot = await service.snapshot()
        XCTAssertEqual(snapshot.connection, .offline)
        XCTAssertEqual(factory.secondTransport.bootstrapCount, 0)
    }

    func testDisconnectCancelsAutomaticRetry() async throws {
        let pairing = Fixture.makePairing(credentialStore: MemoryCredentialStore(), hostStore: MemoryHostStore())
        try await Fixture.pair(pairing)
        let factory = RetryingFactory(bootstrap: Fixture.bootstrap())
        let service = CompanionSessionService(factory: factory, pairing: pairing)
        try await service.connect()
        await service.enterBackground()
        await service.enterForeground()
        await waitUntil("failed retry") { await factory.attempts == 2 }
        await service.disconnect()
        try await Task.sleep(nanoseconds: 2_200_000_000)
        let attempts = await factory.attempts
        let snapshot = await service.snapshot()
        XCTAssertEqual(attempts, 2)
        XCTAssertEqual(snapshot.connection, .offline)
    }
}
