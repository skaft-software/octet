import Foundation
import OctetServe

/// Composition root for the native client. All transport, TLS, pairing,
/// Keychain, replay, and idempotency behavior is supplied by apple-shared.
enum MacOSClientFactory {
    static func make() -> ServeClient {
        let hostName = Host.current().localizedName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let deviceName = hostName.isEmpty ? "Mac" : hostName
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
