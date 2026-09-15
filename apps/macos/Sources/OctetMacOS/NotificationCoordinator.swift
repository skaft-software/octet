import Foundation
import UserNotifications

/// Notification delivery is deliberately passive. Presenting a banner never
/// activates the app; activation only happens after the user clicks it and the
/// host has revalidated the route.
final class NotificationCoordinator: NSObject, UNUserNotificationCenterDelegate, @unchecked Sendable {
    static let shared = NotificationCoordinator()

    private var routeHandler: ((NotificationRoute) -> Void)?

    private override init() {
        super.init()
        UNUserNotificationCenter.current().delegate = self
    }

    func setRouteHandler(_ handler: @escaping (NotificationRoute) -> Void) {
        routeHandler = handler
    }

    func requestAuthorization() async {
        _ = try? await UNUserNotificationCenter.current().requestAuthorization(
            options: [.alert, .sound]
        )
    }

    func enqueue(route: NotificationRoute) {
        let content = UNMutableNotificationContent()
        content.title = "Octet Serve"
        content.body = "A Serve session requires attention."
        content.sound = .default
        content.userInfo = NotificationPolicy.userInfo(for: route)

        let request = UNNotificationRequest(
            identifier: "serve-\(route.eventID)",
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request)
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        // Banner and sound do not take focus from the current application.
        [.banner, .sound]
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse
    ) async {
        guard let route = NotificationPolicy.route(from: response.notification.request.content.userInfo) else {
            return
        }
        await MainActor.run { [weak self] in
            self?.routeHandler?(route)
        }
    }
}
