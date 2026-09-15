import Foundation
import OctetServeClient

/// Composition root for the native client. All transport, TLS, pairing,
/// Keychain, replay, and idempotency behavior is supplied by apple-shared.
enum MacOSClientFactory {
    static func make() -> ServeClient {
        let deviceName = Host.current().localizedName?.trimmingCharacters(in: .whitespacesAndNewlines)
            .flatMap { $0.isEmpty ? nil : $0 }
            ?? "Mac"
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0"
        let configuration = ServeClientConfiguration(
            platform: .macOS,
            deviceName: deviceName,
            bundleIdentifier: "com.octet.serve.macos",
            appVersion: version
        )
        return ServeClient(configuration: configuration)
    }
}
