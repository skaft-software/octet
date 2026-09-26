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

/// A read-only view of what the driver is currently doing.
///
/// Polled from `cua-driver sessions --json` rather than pushed by the agent, so
/// the indicator reflects the driver's own state and cannot claim activity the
/// driver does not have.
struct SessionSummary {
    var activeSessions: Int = 0
    var cursorVisible: Bool = false
    var recordingActive: Bool = false
    var hasSession: Bool = false

    /// True when the agent currently holds the desktop.
    var isActing: Bool { activeSessions > 0 }

    static func read() -> SessionSummary {
        guard let binary = Paths.driverBinary() else { return SessionSummary() }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: binary)
        process.arguments = ["sessions", "--json"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            return SessionSummary()
        }
        // Read on a background thread so the host's main thread - which owns the
        // menu bar item - never blocks on a wedged child. A small box keeps the
        // handoff to the background thread free of a shared mutable capture.
        final class Box: @unchecked Sendable {
            var data = Data()
            let lock = NSLock()
            func set(_ value: Data) { lock.lock(); data = value; lock.unlock() }
            func get() -> Data { lock.lock(); defer { lock.unlock() }; return data }
        }
        let box = Box()
        let group = DispatchGroup()
        DispatchQueue.global(qos: .utility).async(group: group) {
            box.set(pipe.fileHandleForReading.readDataToEndOfFile())
        }
        let finished = group.wait(timeout: .now() + 5) == .success
        if !finished {
            process.terminate()
            _ = group.wait(timeout: .now() + 2)
            return SessionSummary()
        }
        guard let text = String(data: box.get(), encoding: .utf8),
            let payload = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
            let sessions = payload["sessions"] as? [[String: Any]]
        else {
            return SessionSummary()
        }

        var summary = SessionSummary()
        for session in sessions {
            if (session["state"] as? String) == "active" { summary.activeSessions += 1 }
            if (session["cursor_visible"] as? Bool) == true { summary.cursorVisible = true }
            if (session["recording_active"] as? Bool) == true { summary.recordingActive = true }
            if session["session"] is String { summary.hasSession = true }
        }
        return summary
    }
}

/// The menu bar indicator.
///
/// This exists for two reasons, and the second is the important one. It tells
/// the user that an agent currently has control of their screen, which is a
/// transparency obligation, and its "Stop" item revokes that control in one
/// click. An agent that can act on the desktop but leaves no trace that it is
/// acting is the failure mode this prevents.
@MainActor
final class StatusItemController: NSObject, NSMenuDelegate {
    private var item: NSStatusItem?
    private var timer: Timer?
    private let onStop: () -> Void
    private let onQuit: () -> Void
    private var lastActing = false

    init(onStop: @escaping () -> Void, onQuit: @escaping () -> Void) {
        self.onStop = onStop
        self.onQuit = onQuit
    }

    func install() {
        let statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        statusItem.button?.image = Self.icon(acting: false)
        statusItem.button?.image?.isTemplate = true
        statusItem.button?.toolTip = "Octet computer use is idle"
        statusItem.menu = buildMenu()
        item = statusItem

        // Poll rather than wait for a callback: the driver exposes state, not
        // events, and a poll keeps the host free of any agent-side protocol.
        let timer = Timer(timeInterval: 1.0, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.refresh() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
        refresh()
    }

    private func buildMenu() -> NSMenu {
        let menu = NSMenu()
        let status = NSMenuItem(title: "Idle", action: nil, keyEquivalent: "")
        status.isEnabled = false
        menu.addItem(status)
        menu.addItem(.separator())

        let stop = NSMenuItem(
            title: "Stop computer use",
            action: #selector(handleStop),
            keyEquivalent: ""
        )
        stop.target = self
        menu.addItem(stop)

        let quit = NSMenuItem(title: "Quit Octet Computer Use", action: #selector(handleQuit), keyEquivalent: "q")
        quit.target = self
        menu.addItem(quit)
        return menu
    }

    private func refresh() {
        let summary = SessionSummary.read()
        let acting = summary.isActing
        if let button = item?.button {
            button.image = Self.icon(acting: acting)
            // A template image tints with the menu bar, which reads as normal.
            // While the agent is acting, colour is meaningful: it is the signal
            // that the user is not looking at an idle system.
            button.image?.isTemplate = !acting
            button.contentTintColor = acting ? .systemOrange : nil
            button.toolTip = acting
                ? "Octet computer use is controlling this Mac"
                : "Octet computer use is idle"
        }
        if let menu = item?.menu, let status = menu.items.first {
            if acting {
                status.title = "Active - \(summary.activeSessions) session(s)"
            } else {
                status.title = "Idle"
            }
        }
        lastActing = acting
    }

    /// Draw the status glyph: a pointer arrow, the shape the driver uses for
    /// the agent cursor, so the menu bar and the overlay read as the same thing.
    ///
    /// Drawn rather than shipped so the indicator needs no bundled artwork and
    /// stays crisp at every display scale. The geometry is the classic pointer:
    /// a sharp tip at the top left, a wide head, a notch, and a short angled
    /// tail - which is what separates a real cursor from an arbitrary blob.
    private static func icon(acting: Bool, paused: Bool = false) -> NSImage? {
        let size = NSSize(width: 16, height: 16)
        let image = NSImage(size: size, flipped: false) { _ in
            let path = NSBezierPath()
            // Scaled from a 12x13 design grid to the menu bar's 16pt box.
            func p(_ x: CGFloat, _ y: CGFloat) -> NSPoint {
                NSPoint(x: x / 12 * size.width, y: y / 13 * size.height)
            }
            path.move(to: p(0.7, 12.4))      // tip
            path.line(to: p(10.9, 7.0))      // right shoulder
            path.line(to: p(8.0, 4.8))       // notch on the right
            path.line(to: p(11.9, 0.7))      // tail tip
            path.line(to: p(8.4, 0.2))       // tail heel
            path.line(to: p(4.9, 4.1))       // back up the left of the tail
            path.line(to: p(0.7, 12.4))      // close to the tip
            path.close()

            if acting {
                NSColor.systemOrange.setFill()
            } else {
                NSColor.labelColor.setFill()
            }
            path.fill()

            // While paused, strike the cursor through. A colour change alone
            // would be missed by a user who is not looking for it, and "stopped"
            // must never be mistaken for "live".
            if paused {
                let bar = NSBezierPath()
                bar.lineWidth = 1.8
                bar.lineCapStyle = .round
                bar.move(to: p(1.6, 1.2))
                bar.line(to: p(10.4, 11.4))
                // Knock the glyph out in the menu bar's own background colour
                // first, so the slash reads clearly on both light and dark menu
                // bars without the host having to know which one it is.
                NSColor.windowBackgroundColor.setStroke()
                bar.stroke()
                (acting ? NSColor.systemOrange : NSColor.labelColor).setStroke()
                bar.lineWidth = 1.0
                bar.stroke()
            }
            return true
        }
        return image
    }

    @objc private func handleStop() {
        onStop()
        refresh()
    }

    @objc private func handleQuit() {
        onQuit()
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate, @unchecked Sendable {
    private let daemon = DaemonProcess()
    private var status: StatusItemController?

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

        // The indicator is a transparency affordance, so it appears even when
        // the agent is idle: its presence tells the user computer use is
        // installed and available, and its colour tells them when it is live.
        let controller = StatusItemController(
            onStop: { [weak self] in self?.revokeAllSessions() },
            onQuit: { NSApp.terminate(nil) }
        )
        controller.install()
        status = controller
    }

    /// Revoke every live agent session.
    ///
    /// This is the one-click kill switch. `cua-driver revoke` is deny-only: it
    /// stops and revokes sessions and never accepts an approval, so it cannot be
    /// used to widen access. The agent may reconnect later, but the user gets an
    /// immediate, honest interruption rather than a machine that stays under
    /// agent control with no way in.
    private func revokeAllSessions() {
        guard let binary = Paths.driverBinary() else { return }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: binary)
        process.arguments = ["revoke"]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try? process.run()
        process.waitUntilExit()
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
