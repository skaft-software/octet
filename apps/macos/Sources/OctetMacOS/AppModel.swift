import AppKit
import Combine
import Foundation
import OctetServeClient
import UserNotifications

@MainActor
public final class MacOSAppModel: ObservableObject {
    @Published public private(set) var connection: ServeConnectionState = .unpaired
    @Published public private(set) var bootstrap: ServeBootstrap?
    @Published public private(set) var sessions: [SessionSummary] = []
    @Published public private(set) var currentSession: SessionSnapshot?
    @Published public private(set) var pendingMutations: [PendingMutation] = []
    @Published public private(set) var discoveredHosts: [DiscoveredHost] = []
    @Published public private(set) var pairing: PairingPresentation = .idle
    @Published public private(set) var errorBanner: String?
    @Published public var selectedSessionID: String?
    @Published public var searchText = ""
    @Published public var draft = ""
    @Published public private(set) var isShowingCreateSession = false

    private let client: ServeClient
    private var eventTask: Task<Void, Never>?
    private var eventStreamGeneration: UInt64 = 0
    private var discoveryTask: Task<Void, Never>?
    private var pairingTask: Task<Void, Never>?
    private var activePairingAttempt: PairingAttempt?
    private var pairingEpoch: UInt64 = 0
    private var reconnectTask: Task<Void, Never>?
    private var connectInFlight = false
    private var connectWaiters: [CheckedContinuation<Void, Never>] = []
    private var connectionEpoch: UInt64 = 0
    private var selectionEpoch: UInt64 = 0
    private var draftSessionID: String?
    private var reconnectEnabled = true
    private var started = false
    private var backoff = ReconnectionBackoff()
    private var wakeConnection: ServeConnectionState?
    private var reconnectAfterWake = false
    private var wakeInFlight = false
    private var sleepObserver: NSObjectProtocol?
    private var wakeObserver: NSObjectProtocol?
    private var activeObserver: NSObjectProtocol?

    public init(client: ServeClient? = nil) {
        self.client = client ?? MacOSClientFactory.make()
        sleepObserver = NotificationCenter.default.addObserver(
            forName: NSWorkspace.willSleepNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in await self?.handleSleep() }
        }
        wakeObserver = NotificationCenter.default.addObserver(
            forName: NSWorkspace.didWakeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in await self?.handleWake() }
        }
        activeObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in await self?.handleApplicationActive() }
        }
    }

    deinit {
        if let sleepObserver { NotificationCenter.default.removeObserver(sleepObserver) }
        if let wakeObserver { NotificationCenter.default.removeObserver(wakeObserver) }
        if let activeObserver { NotificationCenter.default.removeObserver(activeObserver) }
        eventTask?.cancel()
        discoveryTask?.cancel()
        client.stopDiscovery()
        client.disconnect()
        pairingTask?.cancel()
        reconnectTask?.cancel()
    }

    public var filteredSessions: [SessionSummary] {
        SessionListPolicy.filtered(SessionListPolicy.sort(sessions), query: searchText)
    }

    public var pairingHostID: String? {
        switch pairing {
        case .checking(let host): return host.id
        case .awaitingTicket(let info): return info.hostID
        default: return nil
        }
    }

    public var composerMode: ComposerMode {
        guard let session = currentSession else { return .disabled("Select a session") }
        switch session.status {
        case .waitingForApproval:
            return .disabled("Resolve approval above")
        case .waitingForInput:
            return .disabled("Answer the request above")
        case .running:
            return bootstrap?.capabilities.supportsSteer == true ? .steer : .disabled("Run in progress")
        default:
            return bootstrap?.capabilities.supportsFollowUp == true ? .followUp : .prompt
        }
    }

    public func start() {
        guard !started else { return }
        started = true
        startEventStream()
        Task {
            await NotificationCoordinator.shared.requestAuthorization()
            await connect()
        }
    }

    public func connect() async {
        let requestEpoch = connectionEpoch
        reconnectEnabled = true
        reconnectTask?.cancel()
        reconnectTask = nil
        // A wake can request a connection while a pre-sleep attempt is still
        // unwinding. Queue that request rather than dropping it or overlapping
        // client connection operations.
        while connectInFlight {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                connectWaiters.append(continuation)
            }
            guard !Task.isCancelled, requestEpoch == connectionEpoch else { return }
        }
        guard requestEpoch == connectionEpoch else { return }
        reconnectTask?.cancel()
        reconnectTask = nil
        connectInFlight = true
        defer {
            connectInFlight = false
            let waiters = connectWaiters
            connectWaiters.removeAll()
            waiters.forEach { $0.resume() }
        }
        let epoch = requestEpoch
        do {
            connection = .connecting
            try await client.connect()
            try await refreshInventory()
            guard epoch == connectionEpoch, reconnectEnabled else { return }
            switch connection {
            case .unpaired, .sleeping, .revoked, .hostIdentityChanged, .disconnected:
                return
            default:
                reconnectTask?.cancel()
                reconnectTask = nil
                backoff.reset()
                startEventStream()
                connection = .connected
            }
        } catch let error as ServeClientError {
            guard epoch == connectionEpoch, reconnectEnabled else { return }
            switch connection {
            case .unpaired, .sleeping, .revoked, .hostIdentityChanged:
                return
            default:
                break
            }
            switch error {
            case .notPaired:
                applyConnection(.unpaired)
            case .deviceRevoked:
                applyConnection(.revoked)
                errorBanner = "This Mac was revoked by the Serve host. Pair it again to continue."
            case .hostIdentityChanged:
                applyConnection(.hostIdentityChanged)
                errorBanner = "The host identity changed. Pairing is required before reconnecting."
            default:
                applyConnection(.disconnected(reason: error.localizedDescription))
                if reconnectEnabled { scheduleReconnect() }
            }
        } catch is CancellationError {
            return
        } catch {
            guard epoch == connectionEpoch, reconnectEnabled else { return }
            applyConnection(.disconnected(reason: error.localizedDescription))
            if reconnectEnabled { scheduleReconnect() }
        }
    }

    public func disconnect() {
        reconnectEnabled = false
        reconnectAfterWake = false
        wakeConnection = .disconnected(reason: "Disconnected by user")
        connectionEpoch &+= 1
        reconnectTask?.cancel()
        reconnectTask = nil
        stopEventStream()
        invalidatePairingWork()
        pairing = .idle
        client.disconnect()
        connection = .disconnected(reason: "Disconnected by user")
    }

    private func applyConnection(_ value: ServeConnectionState) {
        connection = value
        switch value {
        case .unpaired, .revoked, .hostIdentityChanged:
            reconnectEnabled = false
            connectionEpoch &+= 1
            reconnectTask?.cancel()
            reconnectTask = nil
            stopEventStream()
            bootstrap = nil
            sessions = []
            currentSession = nil
            selectionEpoch &+= 1
            selectedSessionID = nil
            isShowingCreateSession = false
            discoveredHosts = []
            invalidatePairingWork()
            pairing = .idle
        default:
            break
        }
    }

    public func refreshInventory() async throws {
        let epoch = connectionEpoch
        let value = try await client.bootstrap(mode: .inventory)
        guard epoch == connectionEpoch, reconnectEnabled else { return }
        apply(value)
    }

    public func selectSession(_ id: String?) {
        // The sidebar uses a write-through Binding, so this method is called
        // only for user-driven selection changes. Context-menu actions can
        // still target the already selected row, which must remain idempotent.
        guard id != selectedSessionID else { return }
        selectSession(id, clearDraft: true)
    }

    private func selectSession(_ id: String?, clearDraft: Bool) {
        selectionEpoch &+= 1
        selectedSessionID = id
        guard let id else {
            currentSession = nil
            draft = ""
            draftSessionID = nil
            return
        }
        draftSessionID = id
        if clearDraft { draft = "" }
        loadSession(id, clearDraft: false, selectionEpoch: selectionEpoch)
    }

    private func loadSession(
        _ id: String,
        clearDraft: Bool,
        selectionEpoch expectedSelectionEpoch: UInt64? = nil
    ) {
        if clearDraft {
            draft = ""
            draftSessionID = id
        }
        let expectedEpoch = expectedSelectionEpoch ?? selectionEpoch
        Task { [weak self] in
            guard let self else { return }
            do {
                let snapshot = try await self.client.session(id: id)
                guard self.selectedSessionID == id,
                      self.selectionEpoch == expectedEpoch else { return }
                self.currentSession = snapshot
            } catch {
                guard self.selectedSessionID == id,
                      self.selectionEpoch == expectedEpoch else { return }
                self.errorBanner = "Unable to load this session: \(error.localizedDescription)"
            }
        }
    }

    public func openCreateSession() {
        isShowingCreateSession = true
    }

    public func closeCreateSession() {
        isShowingCreateSession = false
    }

    public func createSession(
        projectID: String,
        modelID: String?,
        title: String?,
        authorityProfileID: String? = nil
    ) {
        let request = CreateSessionRequest(
            projectID: projectID,
            modelID: modelID,
            title: title,
            authorityProfileID: authorityProfileID
        )
        closeCreateSession()
        Task {
            await submit(.createSession(request), label: "Create session", sessionID: nil)
        }
    }

    public func submitDraft() {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, let session = currentSession else { return }
        let mode = composerMode
        let command: ServeCommand
        let label: String
        switch mode {
        case .steer:
            command = .steer(SteerRequest(sessionID: session.id, text: text))
            label = "Steer session"
        case .followUp:
            command = .followUp(FollowUpRequest(sessionID: session.id, text: text))
            label = "Follow up"
        case .prompt:
            command = .submitPrompt(PromptRequest(sessionID: session.id, text: text))
            label = "Send prompt"
        case .disabled:
            return
        }
        draft = ""
        Task { await submit(command, label: label, sessionID: session.id) }
    }

    public func stopCurrentRun() {
        guard let session = currentSession else { return }
        Task {
            await submit(.interrupt(InterruptRequest(sessionID: session.id)), label: "Stop run", sessionID: session.id)
        }
    }

    public func resolveApproval(_ request: ApprovalRequest, decision: ApprovalDecision) {
        guard let session = currentSession else { return }
        let resolution = ApprovalResolution(sessionID: session.id, requestID: request.id, decision: decision)
        Task {
            await submit(.resolveApproval(resolution), label: "Resolve approval", sessionID: session.id)
        }
    }

    public func resolveInput(_ request: UserInputRequest, value: String) {
        guard let session = currentSession else { return }
        let resolution = InputResolution(sessionID: session.id, requestID: request.id, value: value)
        Task {
            await submit(.resolveInput(resolution), label: "Answer request", sessionID: session.id)
        }
    }

    public func retryReconciliation(_ mutation: PendingMutation) {
        Task {
            do {
                guard let result = try await client.reconcile(commandID: mutation.id) else {
                    errorBanner = "The host has not acknowledged this action yet. It was not sent again."
                    return
                }
                apply(result, fallback: mutation)
            } catch {
                errorBanner = "Unable to reconcile \(mutation.label): \(error.localizedDescription)"
            }
        }
    }

    public func dismissError() {
        errorBanner = nil
    }

    // MARK: Pairing

    public func startDiscovery() {
        invalidatePairingWork()
        discoveredHosts = []
        pairing = .discovering
        let epoch = pairingEpoch
        discoveryTask = Task { [client = self.client, weak self] in
            do {
                for try await host in client.discoverHosts() {
                    guard !Task.isCancelled else { return }
                    await self?.addDiscoveredHost(host, epoch: epoch)
                }
                await self?.finishDiscovery(epoch: epoch)
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled else { return }
                await self?.setPairingFailure(error.localizedDescription, epoch: epoch)
                await self?.finishDiscovery(epoch: epoch)
            }
        }
    }

    public func inspectHost(_ host: DiscoveredHost) {
        invalidatePairingWork()
        beginHostInspection(host, epoch: pairingEpoch)
    }

    public func inspectManualHost(endpoint: String) {
        let value = endpoint.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return }
        invalidatePairingWork()
        discoveredHosts = []
        pairing = .discovering
        let epoch = pairingEpoch
        pairingTask = Task { [client = self.client, weak self] in
            do {
                let host = try await client.resolveHost(endpoint: value)
                guard !Task.isCancelled else { return }
                await self?.addDiscoveredHost(host, epoch: epoch)
                guard await self?.isPairingEpochCurrent(epoch) == true else { return }
                await self?.beginHostInspection(host, epoch: epoch)
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled else { return }
                await self?.setPairingFailure(error.localizedDescription, epoch: epoch)
            }
        }
    }

    public func submitPairing(ticket: String, fingerprintWords: [String]) {
        guard case .awaitingTicket(let info) = pairing,
              let host = discoveredHosts.first(where: { $0.id == info.hostID }) else {
            errorBanner = "Choose a discovered host before pairing."
            return
        }
        let normalizedTicket = ticket.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalizedTicket.isEmpty else {
            errorBanner = "Enter the one-time pairing ticket."
            return
        }
        invalidatePairingWork()
        let epoch = pairingEpoch
        pairingTask = Task { [client = self.client, weak self] in
            do {
                let attempt = try await client.beginPairing(
                    host: host,
                    ticket: normalizedTicket,
                    confirmedFingerprintWords: fingerprintWords
                )
                guard !Task.isCancelled,
                      await self?.isPairingEpochCurrent(epoch) == true else { return }
                await self?.setPairingWaiting(attempt, epoch: epoch)
                while !Task.isCancelled {
                    try await Task.sleep(nanoseconds: 750_000_000)
                    guard !Task.isCancelled,
                          await self?.isPairingEpochCurrent(epoch) == true else { return }
                    let status = try await client.pairingStatus(for: attempt)
                    guard !Task.isCancelled,
                          await self?.isPairingEpochCurrent(epoch) == true else { return }
                    switch status {
                    case .pending:
                        continue
                    case .approved(let deviceName):
                        try await client.acknowledgePairing(attempt)
                        guard !Task.isCancelled,
                              await self?.isPairingEpochCurrent(epoch) == true else { return }
                        await self?.setPairingApproved(deviceName, epoch: epoch)
                        guard !Task.isCancelled,
                              await self?.isPairingEpochCurrent(epoch) == true else { return }
                        await self?.connect()
                        return
                    case .denied(let reason):
                        await self?.setPairingDenied(reason, epoch: epoch)
                        return
                    case .expired:
                        await self?.setPairingDenied("The pairing request expired.", epoch: epoch)
                        return
                    case .cancelled:
                        await self?.setPairingDenied("Pairing was cancelled.", epoch: epoch)
                        return
                    }
                }
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled else { return }
                await self?.setPairingFailure(error.localizedDescription, epoch: epoch)
            }
        }
    }

    public func cancelPairing() {
        invalidatePairingWork()
        pairing = .idle
    }

    private func invalidatePairingWork() {
        pairingEpoch &+= 1
        client.stopDiscovery()
        discoveryTask?.cancel()
        discoveryTask = nil
        pairingTask?.cancel()
        pairingTask = nil
        cancelActivePairingAttempt()
    }

    private func isPairingEpochCurrent(_ epoch: UInt64) -> Bool {
        pairingEpoch == epoch
    }

    private func beginHostInspection(_ host: DiscoveredHost, epoch: UInt64) {
        guard pairingEpoch == epoch else { return }
        pairing = .checking(host)
        pairingTask = Task { [client = self.client, weak self] in
            do {
                let info = try await client.pairingInfo(for: host)
                guard !Task.isCancelled else { return }
                await self?.setPairingInfo(info, epoch: epoch)
            } catch is CancellationError {
                return
            } catch {
                guard !Task.isCancelled else { return }
                await self?.setPairingFailure(error.localizedDescription, epoch: epoch)
            }
        }
    }

    private func startEventStream() {
        guard eventTask == nil else { return }
        eventStreamGeneration &+= 1
        let generation = eventStreamGeneration
        let epoch = connectionEpoch
        eventTask = Task { [client = self.client, weak self] in
            for await event in client.events {
                guard !Task.isCancelled else { return }
                await self?.consume(
                    event,
                    connectionEpoch: epoch,
                    generation: generation
                )
            }
            await self?.handleEventStreamEnded(
                connectionEpoch: epoch,
                generation: generation
            )
        }
    }

    private func stopEventStream() {
        eventStreamGeneration &+= 1
        eventTask?.cancel()
        eventTask = nil
    }

    // MARK: Lifecycle and notifications

    public func openNotification(_ route: NotificationRoute) {
        let expectedConnectionEpoch = connectionEpoch
        let expectedSelectionEpoch = selectionEpoch
        Task {
            do {
                // The shared client checks the event cursor and current host
                // authority before returning a snapshot. A stale notification
                // therefore cannot select a removed or unauthorized session.
                let snapshot = try await client.refreshSession(
                    id: route.sessionID,
                    throughEventID: route.eventID
                )
                guard connectionEpoch == expectedConnectionEpoch,
                      reconnectEnabled,
                      selectionEpoch == expectedSelectionEpoch else { return }
                let wasSelected = selectedSessionID == snapshot.id
                selectionEpoch &+= 1
                selectedSessionID = snapshot.id
                draftSessionID = snapshot.id
                if !wasSelected { draft = "" }
                currentSession = snapshot
                NSApplication.shared.activate(ignoringOtherApps: false)
            } catch {
                guard connectionEpoch == expectedConnectionEpoch,
                      selectionEpoch == expectedSelectionEpoch else { return }
                errorBanner = "That Serve event is no longer available."
            }
        }
    }

    private func handleSleep() async {
        wakeConnection = connection
        switch connection {
        case .connected, .connecting, .reconnecting, .disconnected:
            reconnectAfterWake = reconnectEnabled
        default:
            reconnectAfterWake = false
        }
        reconnectEnabled = false
        connectionEpoch &+= 1
        reconnectTask?.cancel()
        reconnectTask = nil
        stopEventStream()
        invalidatePairingWork()
        switch pairing {
        case .checking, .awaitingTicket, .waiting, .discovering:
            pairing = .failed("Pairing was cancelled because the Mac went to sleep.")
        default:
            break
        }
        client.suspend()
        connection = .sleeping
    }

    private func handleWake() async {
        guard case .sleeping = connection, !wakeInFlight else { return }
        wakeInFlight = true
        defer { wakeInFlight = false }
        let epoch = connectionEpoch
        let previousConnection = wakeConnection
        let shouldReconnect = reconnectAfterWake
        wakeConnection = nil
        reconnectAfterWake = false
        reconnectEnabled = shouldReconnect
        do {
            try await client.resume()
            guard epoch == connectionEpoch else { return }
            if shouldReconnect {
                guard reconnectEnabled else { return }
                await connect()
            } else {
                restoreConnectionAfterWake(previousConnection)
            }
        } catch is CancellationError {
            return
        } catch let error as ServeClientError {
            guard epoch == connectionEpoch else { return }
            if !shouldReconnect {
                restoreConnectionAfterWake(previousConnection)
                errorBanner = "Unable to resume Serve: \(error.localizedDescription)"
                return
            }
            switch error {
            case .notPaired:
                applyConnection(.unpaired)
            case .deviceRevoked:
                applyConnection(.revoked)
                errorBanner = "This Mac was revoked by the Serve host. Pair it again to continue."
            case .hostIdentityChanged:
                applyConnection(.hostIdentityChanged)
                errorBanner = "The host identity changed. Pairing is required before reconnecting."
            default:
                applyConnection(.disconnected(reason: error.localizedDescription))
                if reconnectEnabled { scheduleReconnect() }
            }
        } catch {
            guard epoch == connectionEpoch else { return }
            if !shouldReconnect {
                restoreConnectionAfterWake(previousConnection)
                errorBanner = "Unable to resume Serve: \(error.localizedDescription)"
                return
            }
            applyConnection(.disconnected(reason: error.localizedDescription))
            if reconnectEnabled { scheduleReconnect() }
        }
    }

    private func restoreConnectionAfterWake(_ previous: ServeConnectionState?) {
        switch previous {
        case .unpaired:
            applyConnection(.unpaired)
        case .revoked:
            applyConnection(.revoked)
        case .hostIdentityChanged:
            applyConnection(.hostIdentityChanged)
        case .disconnected(let reason):
            connection = .disconnected(reason: reason)
        default:
            connection = .disconnected(reason: "Disconnected before sleep")
        }
    }

    private func handleApplicationActive() async {
        guard case .disconnected = connection,
              reconnectEnabled,
              !Task.isCancelled else { return }
        await connect()
    }

    private func handleEventStreamEnded(
        connectionEpoch epoch: UInt64,
        generation: UInt64
    ) {
        guard generation == eventStreamGeneration else { return }
        eventTask = nil
        guard epoch == connectionEpoch,
              reconnectEnabled,
              !Task.isCancelled else { return }
        switch connection {
        case .unpaired, .sleeping, .revoked, .hostIdentityChanged:
            return
        default:
            applyConnection(.disconnected(reason: "Serve event stream ended"))
            scheduleReconnect()
        }
    }

    // MARK: Event reduction

    private func consume(
        _ event: ServeEvent,
        connectionEpoch epoch: UInt64,
        generation: UInt64
    ) {
        guard epoch == connectionEpoch,
              generation == eventStreamGeneration,
              !Task.isCancelled else { return }
        switch event {
        case .connection(let value):
            applyConnection(value)
            if case .disconnected = value, reconnectEnabled {
                scheduleReconnect()
            }
        case .bootstrap(let value):
            apply(value)
        case .sessionSnapshot(let snapshot):
            if selectedSessionID == snapshot.id {
                currentSession = snapshot
            }
            updateSummary(from: snapshot)
        case .sessionSummary(let summary):
            upsert(summary)
        case .attention(let attention):
            NotificationCoordinator.shared.enqueue(
                route: NotificationRoute(
                    sessionID: attention.route.sessionID,
                    eventID: attention.route.eventID
                )
            )
        }
    }

    private func apply(_ value: ServeBootstrap) {
        bootstrap = value
        sessions = SessionListPolicy.sort(value.sessions)
        guard let selectedID = selectedSessionID,
              let selected = sessions.first(where: { $0.id == selectedID }) else {
            let preservedID = draftSessionID.flatMap { draftID in
                sessions.first(where: { $0.id == draftID })?.id
            }
            let nextID = preservedID ?? sessions.first?.id
            currentSession = nil
            if let nextID {
                selectSession(nextID, clearDraft: nextID != draftSessionID)
            } else {
                selectSession(nil, clearDraft: true)
            }
            return
        }
        if currentSession?.id != selected.id {
            selectSession(selected.id, clearDraft: selected.id != draftSessionID)
        } else {
            draftSessionID = selected.id
            loadSession(selected.id, clearDraft: false)
        }
    }

    private func updateSummary(from snapshot: SessionSnapshot) {
        guard let existing = sessions.first(where: { $0.id == snapshot.id }) else { return }
        let summary = existing.updated(with: snapshot)
        upsert(summary)
    }

    private func upsert(_ summary: SessionSummary) {
        if let index = sessions.firstIndex(where: { $0.id == summary.id }) {
            sessions[index] = summary
        } else {
            sessions.append(summary)
        }
        sessions = SessionListPolicy.sort(sessions)
    }

    private func submit(_ command: ServeCommand, label: String, sessionID: String?) async {
        do {
            let result = try await client.submit(command)
            apply(result, fallback: PendingMutation(id: result.commandID, sessionID: sessionID, label: label))
        } catch let error as ServeClientError {
            if case .ambiguous(let commandID, _) = error {
                addPending(PendingMutation(id: commandID, sessionID: sessionID, label: label))
            } else {
                errorBanner = error.localizedDescription
            }
        } catch {
            errorBanner = error.localizedDescription
        }
    }

    private func apply(_ result: ServeCommandResult, fallback: PendingMutation) {
        switch result {
        case .accepted(let commandID, let createdSessionID):
            removePending(commandID)
            if let createdSessionID {
                selectSession(createdSessionID)
            } else if let sessionID = fallback.sessionID, sessionID == selectedSessionID {
                Task { try? await refreshSelectedSession() }
            }
        case .rejected(let commandID, let message, _):
            removePending(commandID)
            errorBanner = message
        case .uncertain(let commandID, let message):
            addPending(PendingMutation(id: commandID, sessionID: fallback.sessionID, label: fallback.label))
            errorBanner = "\(message) The action was not sent again; reconcile it below."
        }
    }

    private func refreshSelectedSession() async throws {
        guard let id = selectedSessionID else { return }
        let expectedEpoch = selectionEpoch
        let snapshot = try await client.session(id: id)
        guard selectedSessionID == id, selectionEpoch == expectedEpoch else { return }
        currentSession = snapshot
    }

    private func addPending(_ mutation: PendingMutation) {
        guard !pendingMutations.contains(where: { $0.id == mutation.id }) else { return }
        pendingMutations.append(mutation)
    }

    private func removePending(_ commandID: String) {
        pendingMutations.removeAll { $0.id == commandID }
    }

    private func scheduleReconnect() {
        guard reconnectTask == nil, reconnectEnabled else { return }
        let delay = backoff.nextDelayNanoseconds()
        let epoch = connectionEpoch
        reconnectTask = Task { [weak self] in
            do {
                try await Task.sleep(nanoseconds: delay)
            } catch {
                return
            }
            guard !Task.isCancelled, let self else { return }
            guard self.connectionEpoch == epoch, self.reconnectEnabled else {
                self.reconnectTask = nil
                return
            }
            self.reconnectTask = nil
            await self.connect()
        }
    }

    private func finishDiscovery(epoch: UInt64) {
        guard pairingEpoch == epoch else { return }
        discoveryTask = nil
        if case .discovering = pairing {
            pairing = .idle
        }
    }

    private func addDiscoveredHost(_ host: DiscoveredHost, epoch: UInt64) {
        guard pairingEpoch == epoch else { return }
        if let index = discoveredHosts.firstIndex(where: { $0.id == host.id }) {
            discoveredHosts[index] = host
        } else {
            discoveredHosts.append(host)
        }
    }

    private func cancelActivePairingAttempt() {
        guard let attempt = activePairingAttempt else { return }
        activePairingAttempt = nil
        Task { [client = self.client] in
            try? await client.cancelPairing(attempt)
        }
    }

    private func setPairingInfo(_ info: PairingInfo, epoch: UInt64) {
        guard pairingEpoch == epoch else { return }
        pairing = .awaitingTicket(info)
    }

    private func setPairingWaiting(_ attempt: PairingAttempt, epoch: UInt64) {
        guard pairingEpoch == epoch else { return }
        activePairingAttempt = attempt
        pairing = .waiting(attempt)
    }

    private func setPairingApproved(_ deviceName: String, epoch: UInt64) {
        guard pairingEpoch == epoch else { return }
        activePairingAttempt = nil
        pairing = .approved(deviceName)
    }

    private func setPairingDenied(_ reason: String, epoch: UInt64) {
        guard pairingEpoch == epoch else { return }
        activePairingAttempt = nil
        pairing = .denied(reason)
    }

    private func setPairingFailure(_ message: String, epoch: UInt64) {
        guard pairingEpoch == epoch else { return }
        cancelActivePairingAttempt()
        pairing = .failed(message)
    }
}
