import Foundation

/// Bounded main-actor view state over `CompanionSessionService`.
///
/// The model owns no transport, credential or command-encoding logic: every
/// mutation goes through the service actor, which revalidates host identity,
/// pairing state and the session actor generation before anything reaches a
/// socket. The model bounds what a human can type, surfaces host-reported state
/// verbatim (already truncated by `SafeText` in the wire layer) and never
/// fabricates session content.
@MainActor
public final class CompanionAppModel: ObservableObject {
    public enum Activity: Equatable {
        case idle
        case working(String)
        case failed(String)
    }

    /// Mirrors `CommandEnvelopeEncoder.sessionPrompt` bounds so the UI refuses
    /// oversized input before it reaches the service.
    public static let maxPromptBytes = 256 * 1024
    /// Mirrors the bounded answer path; a free-text answer is never a document.
    public static let maxAnswerBytes = 16 * 1024

    @Published public private(set) var snapshot: CompanionSnapshot
    @Published public private(set) var activity: Activity = .idle
    @Published public var composer: String = ""
    @Published public var answerInput: String = ""

    private let service: CompanionSessionService
    private var updatesTask: Task<Void, Never>?

    public init(service: CompanionSessionService) {
        self.service = service
        self.snapshot = CompanionSnapshot(
            connection: .unconfigured,
            host: nil,
            sessions: [],
            selectedSessionID: nil,
            selectedSession: nil,
            error: nil
        )
    }

    public var isWorking: Bool {
        if case .working = activity { return true }
        return false
    }

    public var failure: String? {
        if case .failed(let message) = activity { return message }
        return nil
    }

    /// Human-readable connection summary. Only host-reported states appear here.
    public var connectionSummary: String {
        switch snapshot.connection {
        case .unconfigured: return "No host adapter is configured for this build."
        case .offline: return "The paired host is offline."
        case .discovering: return "Looking for a host on this network."
        case .pairing: return "Waiting for the host to confirm this device."
        case .connecting: return "Connecting to the paired host."
        case .connected: return "Connected to the host."
        case .reconnecting: return "Reconnecting to the host."
        case .backgrounded: return "Paused while the app is in the background."
        case .needsPairing: return "Pair this device with a host to continue."
        case .failed: return "The host connection failed."
        }
    }

    public var selectedSession: SessionProjection? { snapshot.selectedSession }

    public func startObserving() {
        guard updatesTask == nil else { return }
        updatesTask = Task { [service] in
            let stream = await service.updates()
            for await next in stream {
                if Task.isCancelled { return }
                self.snapshot = next
            }
        }
    }

    public func stopObserving() {
        updatesTask?.cancel()
        updatesTask = nil
    }

    public func connect(hostID: String? = nil) async {
        await run("Connecting") { try await self.service.connect(hostID: hostID) }
    }

    public func reconnect() async {
        await run("Reconnecting") { try await self.service.reconnect() }
    }

    public func disconnect() async {
        await service.disconnect()
        activity = .idle
    }

    public func enterBackground() async {
        await service.enterBackground()
    }

    public func enterForeground() async {
        await service.enterForeground()
    }

    public func select(sessionID: String) async {
        await run("Opening session") { try await self.service.selectSession(sessionID) }
    }

    public func createSession(authority: AuthoritySelection = .workspace) async {
        await run("Creating session") { try await self.service.createSession(authority: authority) }
    }

    public func send(delivery: PromptDelivery = .submit) async {
        let text = composer
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            activity = .failed("Enter a prompt before sending.")
            return
        }
        guard text.utf8.count <= Self.maxPromptBytes else {
            activity = .failed("The prompt is too long to send.")
            return
        }
        let label: String
        switch delivery {
        case .submit: label = "Sending"
        case .steer: label = "Steering"
        case .followUp: label = "Queuing"
        }
        await run(label) { try await self.service.submitPrompt(text: text, delivery: delivery) }
        if failure == nil { composer = "" }
    }

    public func answerApproval(requestID: String, allowed: Bool) async {
        await run(allowed ? "Approving" : "Declining") {
            try await self.service.answerApproval(requestID: requestID, allowed: allowed)
        }
    }

    public func answerText(requestID: String) async {
        let text = answerInput
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            activity = .failed("Enter an answer before responding.")
            return
        }
        guard text.utf8.count <= Self.maxAnswerBytes else {
            activity = .failed("The answer is too long to send.")
            return
        }
        await run("Answering") {
            try await self.service.answerText(requestID: requestID, text: text)
        }
        if failure == nil { answerInput = "" }
    }

    public func stopRun() async {
        await run("Stopping") { try await self.service.stop() }
    }

    private func run(_ label: String, _ operation: () async throws -> Void) async {
        activity = .working(label)
        do {
            try await operation()
            activity = .idle
        } catch let error as CompanionError {
            activity = .failed(error.errorDescription ?? "The operation failed.")
        } catch {
            activity = .failed(error.localizedDescription)
        }
        // Re-read host-owned state after every action so the UI never depends on
        // the observation stream's scheduling to reflect the result.
        snapshot = await service.snapshot()
    }
}
