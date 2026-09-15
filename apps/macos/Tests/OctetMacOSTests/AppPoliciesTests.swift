import XCTest
@testable import OctetMacOS

final class AppPoliciesTests: XCTestCase {
    func testNotificationRouteContainsOnlyRevalidationKeys() {
        let route = NotificationRoute(sessionID: "session-1", eventID: "event-9")
        let payload = NotificationPolicy.userInfo(for: route)

        XCTAssertEqual(payload["sessionID"] as? String, "session-1")
        XCTAssertEqual(payload["eventID"] as? String, "event-9")
        XCTAssertNil(payload["prompt"])
        XCTAssertNil(payload["toolOutput"])
        XCTAssertNil(payload["credential"])
        XCTAssertEqual(NotificationPolicy.route(from: payload), route)
    }

    func testMalformedNotificationRouteIsRejected() {
        XCTAssertNil(NotificationPolicy.route(from: ["sessionID": "", "eventID": "event"]))
        XCTAssertNil(NotificationPolicy.route(from: ["sessionID": "session"]))
        XCTAssertNil(NotificationPolicy.route(from: ["sessionID": 42, "eventID": "event"]))
    }

    func testReconnectionBackoffIsBounded() {
        var backoff = ReconnectionBackoff()
        var delays: [UInt64] = []
        for _ in 0..<20 { delays.append(backoff.nextDelayNanoseconds()) }
        XCTAssertTrue(delays.allSatisfy { $0 <= 30_000_000_000 })
        backoff.reset()
        XCTAssertLessThan(backoff.nextDelayNanoseconds(), 2_000_000_000)
    }

    func testPendingMutationIdentityIsStable() {
        let date = Date(timeIntervalSince1970: 10)
        let first = PendingMutation(id: "cmd-1", sessionID: "session-1", label: "Send", createdAt: date)
        let second = PendingMutation(id: "cmd-1", sessionID: "session-1", label: "Send", createdAt: date)
        XCTAssertEqual(first, second)
    }
}
