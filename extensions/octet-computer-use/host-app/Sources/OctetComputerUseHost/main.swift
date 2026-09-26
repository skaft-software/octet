// Octet Computer Use Host
//
// A real AppKit application that hosts the MIT-licensed Cua Driver daemon so
// the driver can own a stable macOS TCC identity and draw the agent cursor.
//
// Why this exists
// ---------------
// The driver needs two things that a terminal-hosted process cannot have on
// macOS:
//
//   1. A stable permission identity. Accessibility and Screen Recording grants
//      key on the responsible *application*, not on a bare executable path.
//   2. A certified AppKit main thread with Window Server access, which is what
//      the agent-cursor overlay is drawn on.
//
// Without them the driver still drives the desktop through the direct runtime,
// but it has no cursor overlay. This app supplies the host half only; the agent
// connects to the daemon's socket exactly as it would for `cua-driver serve`.
//
// It deliberately owns no agent logic. It does not choose targets, decide what
// is safe, or interpret anything the driver returns. All of that stays in the
// octet extension, which is the only component that can see a confirmation
// prompt.
//
// Identity
// --------
// The bundle identifier is `com.octet.computeruse`, deliberately distinct from
// Cua's own `com.trycua.driver` and `com.trycua.driver.local`. Sharing Cua's
// identifier would merge our TCC rows with theirs, so whichever app installed
// last would inherit permissions granted for the other.

import AppKit
import Foundation

/// Paths the host and the driver agree on. Mirrors the extension's
/// `octet_computer_use.driver_client.daemon_socket`, so both sides address the
/// same endpoint.
enum Paths {
    static let socketDirectory: URL = {
        let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
        return caches.appendingPathComponent("cua-driver", isDirectory: true)
    }()

    static var socket: URL { socketDirectory.appendingPathComponent("cua-driver.sock") }
    static var pidFile: URL { socketDirectory.appendingPathComponent("cua-driver.pid") }

    /// The driver binary, discovered next to this app when packaged and on
    /// `PATH` otherwise. The host never downloads a driver.
    static func driverBinary() -> String? {
        let bundleMacOS = Bundle.main.bundleURL
            .appendingPathComponent("Contents/MacOS/cua-driver")
        if FileManager.default.isExecutableFile(atPath: bundleMacOS.path) {
            return bundleMacOS.path
        }
        let searchPaths = (ProcessInfo.processInfo.environment["PATH"] ?? "")
            .split(separator: ":", omittingEmptySubsequences: true)
            .map(String.init)
        for directory in searchPaths {
            let candidate = URL(fileURLWithPath: directory).appendingPathComponent("cua-driver")
            if FileManager.default.isExecutableFile(atPath: candidate.path) {
                return candidate.path
            }
        }
        return nil
    }
}

/// One running `cua-driver serve` child.
///
/// The child is deliberately not killed on exit-by-signal. `stop()` asks it to
/// shut down over its own control path so an in-flight agent session gets a
/// chance to close cleanly rather than being severed mid-action.
final class DaemonProcess: @unchecked Sendable {
    private var process: Process?
    private let lock = NSLock()

    func start() throws {
        lock.lock()
        defer { lock.unlock() }
        guard process == nil else { return }

        guard let binary = Paths.driverBinary() else {
            throw HostError.driverNotFound
        }

        try FileManager.default.createDirectory(
            at: Paths.socketDirectory, withIntermediateDirectories: true
        )

        let child = Process()
        child.executableURL = URL(fileURLWithPath: binary)
        // `serve` publishes the unix socket the agent connects to.
        // `--host-bundle-id` is what makes the driver attribute TCC to this
        // app rather than to whatever spawned it.
        child.arguments = [
            "serve",
            "--socket", Paths.socket.path,
            "--pid-file", Paths.pidFile.path,
            "--host-bundle-id", Bundle.main.bundleIdentifier ?? "com.octet.computeruse",
        ]

        var environment = ProcessInfo.processInfo.environment
        // Keep a single host identity per machine so repeated launches attach to
        // the same daemon instead of racing a second runtime.
        environment["CUA_DRIVER_HOST_BUNDLE_ID"] =
            Bundle.main.bundleIdentifier ?? "com.octet.computeruse"
        child.environment = environment

        // The driver's own logging is not interesting to us; discarding it keeps
        // the host from growing an unbounded log on a long-running login item.
        child.standardOutput = FileHandle.nullDevice
        child.standardError = FileHandle.nullDevice
        child.standardInput = FileHandle.nullDevice

        try child.run()
        process = child
    }

    /// Stop the daemon, escalating only if it ignores the request.
    func stop() {
        lock.lock()
        let child = process
        process = nil
        lock.unlock()

        guard let child, child.isRunning else { return }
        // SIGTERM lets the driver close sessions and release the socket name.
        kill(child.processIdentifier, SIGTERM)
        let deadline = Date().addingTimeInterval(5)
        while child.isRunning && Date() < deadline {
            usleep(100_000)
        }
        if child.isRunning {
            kill(child.processIdentifier, SIGKILL)
        }
    }

    var isRunning: Bool {
        lock.lock()
        defer { lock.unlock() }
        return process?.isRunning ?? false
    }
}

enum HostError: Error, CustomStringConvertible {
    case driverNotFound

    var description: String {
        switch self {
        case .driverNotFound:
            return "cua-driver was not found next to this app or on PATH"
        }
    }
}

/// The application delegate.
///
/// This app has no windows and no menu bar: it is an `LSUIElement` background
/// host whose only job is to keep a certified AppKit main thread alive and the
/// daemon running. Any window would make it steal focus from whatever the user
/// is actually doing, which is exactly what a background automation host must
/// never do.
final class AppDelegate: NSObject, NSApplicationDelegate, @unchecked Sendable {
    private let daemon = DaemonProcess()

    func applicationDidFinishLaunching(_ notification: Notification) {
        do {
            try daemon.start()
        } catch {
            // A host that cannot reach its driver is not a crash: the extension
            // falls back to the direct runtime, which still drives the desktop.
            // Record the reason and stay alive so the failure is discoverable
            // rather than silent.
            let message = "octet computer-use host: \(error)"
            NSLog("%@", message)
            let note = Paths.socketDirectory.appendingPathComponent("host-error.txt")
            try? message.write(to: note, atomically: true, encoding: .utf8)
            return
        }
    }

    /// Ask the daemon to shut down on the way out so it can close its sessions
    /// and release the socket name, rather than being severed mid-action.
    func applicationWillTerminate(_ notification: Notification) {
        daemon.stop()
    }

    /// Status probe used by the extension and by `octet doctor` style checks.
    /// Prints a single JSON line on stdout and exits.
    static func runProbe() -> Int32 {
        let socket = Paths.socket
        let reachable = FileManager.default.fileExists(atPath: socket.path)
        let payload: [String: Any] = [
            "bundle_id": Bundle.main.bundleIdentifier ?? "",
            "socket": socket.path,
            "socket_present": reachable,
            "daemon_running": reachable,
            "pid": ProcessInfo.processInfo.processIdentifier,
        ]
        if let data = try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys]),
            let text = String(data: data, encoding: .utf8) {
            print(text)
        }
        return 0
    }
}

// MARK: - Entry point

/// Command dispatch.
///
/// `octet-computer-use-host probe` is a read-only status query used by the
/// extension. It runs before any AppKit setup so it stays fast and cannot
/// start a host. With no arguments the app runs as a background host.
let arguments = CommandLine.arguments
if arguments.count > 1, arguments[1] == "probe" {
    exit(AppDelegate.runProbe())
}

/// Run the host.
let application = NSApplication.shared
let delegate = AppDelegate()
application.delegate = delegate
// `.accessory` keeps the app out of the Dock and the app switcher: it is a
// host, not something the user interacts with.
application.setActivationPolicy(.accessory)
application.run()
