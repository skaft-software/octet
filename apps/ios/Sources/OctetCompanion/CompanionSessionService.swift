import Foundation

public struct CompanionSnapshot: Equatable, Sendable {
    public let connection: ConnectionState
    public let host: PairedHost?
    public let sessions: [SessionSummaryView]
    public let selectedSessionID: String?
    public let selectedSession: SessionProjection?
    public let error: CompanionError?

    public init(
        connection: ConnectionState,
        host: PairedHost?,
        sessions: [SessionSummaryView],
        selectedSessionID: String?,
        selectedSession: SessionProjection?,
        error: CompanionError?
    ) {
        self.connection = connection
        self.host = host
        self.sessions = sessions
        self.selectedSessionID = selectedSessionID
        self.selectedSession = selectedSession
        self.error = error
    }
}

public actor CompanionSessionService {
    private let factory: any ServeClientFactory
    private let pairing: PairingCoordinator

    private var connection: ConnectionState = .unconfigured
    private var host: PairedHost?
    private var transport: (any ServeClientTransport)?
    private var connectionToken: UUID?
    private var listenerTask: Task<Void, Never>?
    private var reconnectTask: Task<Void, Never>?
    private var hostSequence: UInt64 = 0
    private var summaries: [String: SessionSummaryView] = [:]
    private var projections: [String: SessionProjection] = [:]
    private var selectedSessionID: String?
    private var pendingCommands: [String: PendingCommand] = [:]
    private var lastError: CompanionError?
    private var observers: [UUID: AsyncStream<CompanionSnapshot>.Continuation] = [:]

    public init(
        factory: any ServeClientFactory,
        pairing: PairingCoordinator
    ) {
        self.factory = factory
        self.pairing = pairing
    }

    public func updates() -> AsyncStream<CompanionSnapshot> {
        let id = UUID()
        return AsyncStream { continuation in
            Task { await self.addObserver(id: id, continuation: continuation) }
        }
    }

    public func snapshot() -> CompanionSnapshot {
        makeSnapshot()
    }

    public func connect(hostID: String? = nil) async throws {
        let hosts = try await pairing.pairedHosts()
        guard let selected = hosts.first(where: { hostID == nil || $0.hostID == hostID }) else {
            connection = .needsPairing
            lastError = .hostNotFound
            publish()
            throw CompanionError.hostNotFound
        }
        reconnectTask?.cancel()
        reconnectTask = nil
        await closeTransport()
        host = selected
        connection = .connecting
        lastError = nil
        publish()
        do {
            try await establish(selected)
        } catch let error as CompanionError {
            await closeTransport()
            connection = .failed
            lastError = error
            publish()
            throw error
        } catch {
            await closeTransport()
            connection = .failed
            lastError = .transportUnavailable
            publish()
            throw CompanionError.transportUnavailable
        }
    }

    public func reconnect() async throws {
        guard let host else { throw CompanionError.hostNotFound }
        try await connect(hostID: host.hostID)
    }

    public func disconnect() async {
        reconnectTask?.cancel()
        reconnectTask = nil
        await closeTransport()
        connection = host == nil ? .unconfigured : .offline
        lastError = nil
        publish()
    }

    public func enterBackground() async {
        reconnectTask?.cancel()
        reconnectTask = nil
        await closeTransport()
        if host == nil {
            connection = .unconfigured
        } else {
            connection = .backgrounded
        }
        publish()
    }

    public func enterForeground() async {
        guard connection == .backgrounded, let host else { return }
        connection = .reconnecting
        publish()
        scheduleReconnect(hostID: host.hostID)
    }

    public func selectSession(_ sessionID: String) async throws {
        guard summaries[sessionID] != nil else { throw CompanionError.invalidInput("That session is no longer available.") }
        guard let transport, let host else { throw CompanionError.transportUnavailable }
        let token = connectionToken
        let data: Data
        do {
            data = try await transport.request(.bootstrap(selectedSessionID: sessionID))
        } catch {
            startReconnectAfterTransportError()
            throw CompanionError.transportUnavailable
        }
        guard token == connectionToken else { throw CompanionError.cancelled }
        let bootstrap = try WireDecoder.bootstrap(data, expectedHostID: host.hostID)
        try apply(bootstrap, retainingSelection: false)
        selectedSessionID = sessionID
        if let snapshot = bootstrap.selectedSession, snapshot.sessionID == sessionID {
            projections[sessionID] = snapshot.projection(title: summaries[sessionID]?.title ?? "Session")
        } else {
            throw CompanionError.protocolViolation("selected session snapshot is missing")
        }
        publish()
    }

    public func createSession(
        projectID: String? = nil,
        authority: AuthoritySelection = .workspace,
        provider: String? = nil,
        model: String? = nil,
        reasoning: String? = nil
    ) async throws {
        guard let host else { throw CompanionError.hostNotFound }
        let commandID = CommandEnvelopeEncoder.newCommandID()
        let deviceID = try await pairingDeviceID(for: host)
        let envelope = try CommandEnvelopeEncoder.hostCreateSession(
            hostID: host.hostID,
            deviceID: deviceID,
            commandID: commandID,
            projectID: projectID,
            authority: authority,
            provider: provider,
            model: model,
            reasoning: reasoning
        )
        let ack = try await deliver(
            PendingCommand(id: commandID, sessionID: nil, actorGeneration: nil, envelope: envelope),
            host: host
        )
        guard case .accepted = ack else { throw CompanionError.invalidInput("The host rejected the new session request.") }
        try await refreshBootstrap(selectedSessionID: nil)
    }

    public func submitPrompt(
        text: String,
        attachments: [WireAttachment] = [],
        documentIDs: [String] = [],
        projectFileIDs: [String] = [],
        delivery: PromptDelivery = .submit
    ) async throws {
        try await prompt(
            text: text,
            attachments: attachments,
            documentIDs: documentIDs,
            projectFileIDs: projectFileIDs,
            delivery: delivery
        )
    }

    public func answerApproval(requestID: String, allowed: Bool) async throws {
        guard let selected = selectedProjection(), let host else { throw CompanionError.staleSession }
        guard let request = selected.pendingRequests.first(where: { $0.id == requestID && $0.isPending }) else {
            throw CompanionError.invalidInput("That request is no longer pending.")
        }
        let commandID = CommandEnvelopeEncoder.newCommandID()
        let deviceID = try await pairingDeviceID(for: host)
        let envelope = try CommandEnvelopeEncoder.answerRequest(
            hostID: host.hostID,
            deviceID: deviceID,
            sessionID: selected.id,
            commandID: commandID,
            actorGeneration: request.actorGeneration,
            requestID: requestID,
            allowed: allowed
        )
        let ack = try await deliver(PendingCommand(id: commandID, sessionID: selected.id, actorGeneration: request.actorGeneration, envelope: envelope), host: host)
        guard case .accepted = ack else { throw CompanionError.invalidInput("The host rejected that answer.") }
    }

    public func answerText(requestID: String, text: String) async throws {
        guard let selected = selectedProjection(), let host else { throw CompanionError.staleSession }
        guard let request = selected.pendingRequests.first(where: { $0.id == requestID && $0.isPending }) else {
            throw CompanionError.invalidInput("That request is no longer pending.")
        }
        let commandID = CommandEnvelopeEncoder.newCommandID()
        let deviceID = try await pairingDeviceID(for: host)
        let envelope = try CommandEnvelopeEncoder.answerTextRequest(
            hostID: host.hostID,
            deviceID: deviceID,
            sessionID: selected.id,
            commandID: commandID,
            actorGeneration: request.actorGeneration,
            requestID: requestID,
            text: text
        )
        let ack = try await deliver(PendingCommand(id: commandID, sessionID: selected.id, actorGeneration: request.actorGeneration, envelope: envelope), host: host)
        guard case .accepted = ack else { throw CompanionError.invalidInput("The host rejected that answer.") }
    }

    public func stop() async throws {
        guard let selected = selectedProjection(), let host else { throw CompanionError.staleSession }
        guard let runID = selected.activeRunID else { throw CompanionError.invalidInput("There is no active run to stop.") }
        let commandID = CommandEnvelopeEncoder.newCommandID()
        let deviceID = try await pairingDeviceID(for: host)
        let envelope = try CommandEnvelopeEncoder.stop(
            hostID: host.hostID,
            deviceID: deviceID,
            sessionID: selected.id,
            commandID: commandID,
            actorGeneration: selected.actorGeneration,
            runID: runID
        )
        let ack = try await deliver(PendingCommand(id: commandID, sessionID: selected.id, actorGeneration: selected.actorGeneration, envelope: envelope), host: host)
        guard case .accepted = ack else { throw CompanionError.invalidInput("The host rejected the stop request.") }
    }

    private func prompt(
        text: String,
        attachments: [WireAttachment],
        documentIDs: [String],
        projectFileIDs: [String],
        delivery: PromptDelivery
    ) async throws {
        guard let selected = selectedProjection(), let host else { throw CompanionError.staleSession }
        guard selected.actorGeneration > 0 else { throw CompanionError.staleSession }
        let commandID = CommandEnvelopeEncoder.newCommandID()
        let deviceID = try await pairingDeviceID(for: host)
        let envelope = try CommandEnvelopeEncoder.sessionPrompt(
            hostID: host.hostID,
            deviceID: deviceID,
            sessionID: selected.id,
            commandID: commandID,
            actorGeneration: selected.actorGeneration,
            delivery: delivery,
            text: text,
            attachments: attachments,
            documentIDs: documentIDs,
            projectFileIDs: projectFileIDs
        )
        let ack = try await deliver(PendingCommand(id: commandID, sessionID: selected.id, actorGeneration: selected.actorGeneration, envelope: envelope), host: host)
        guard case .accepted = ack else { throw CompanionError.invalidInput("The host rejected the prompt.") }
    }

    private func establish(_ pairedHost: PairedHost) async throws {
        let configuration = try await pairing.configuration(for: pairedHost)
        let newTransport = try await factory.makeClient(configuration: configuration)
        do {
            try await newTransport.verifyPeerIdentity(hostID: pairedHost.hostID, fingerprint: pairedHost.fingerprint)
            let data = try await newTransport.request(.bootstrap(selectedSessionID: selectedSessionID))
            let bootstrap = try WireDecoder.bootstrap(data, expectedHostID: pairedHost.hostID)
            guard bootstrap.host.id == pairedHost.hostID else { throw CompanionError.hostIdentityChanged }
            transport = newTransport
            host = pairedHost
            connectionToken = UUID()
            hostSequence = 0
            try apply(bootstrap, retainingSelection: true)
            connection = .connected
            lastError = nil
            publish()
            startListener(transport: newTransport, token: connectionToken!)
            try await retryPendingCommands()
        } catch {
            await newTransport.close()
            throw map(error)
        }
    }

    private func apply(_ bootstrap: WireHostBootstrap, retainingSelection: Bool) throws {
        summaries = Dictionary(uniqueKeysWithValues: bootstrap.sessions.map { summary in
            (summary.id, SessionSummaryView(
                id: summary.id,
                title: SafeText.value(summary.title, limit: 512) ?? "Untitled session",
                liveState: summary.liveState,
                attention: summary.attention,
                pinned: summary.pinned,
                archived: summary.archived,
                modifiedAtMs: summary.modifiedAtMs
            ))
        })
        let nextSelection = retainingSelection && selectedSessionID != nil ? selectedSessionID : bootstrap.selectedSessionID
        selectedSessionID = nextSelection
        projections.removeAll(keepingCapacity: true)
        if let snapshot = bootstrap.selectedSession,
           let title = summaries[snapshot.sessionID]?.title ?? Optional("Session") {
            projections[snapshot.sessionID] = snapshot.projection(title: title)
            selectedSessionID = snapshot.sessionID
        } else if bootstrap.selectedSessionID == nil {
            selectedSessionID = nil
        }
    }

    private func refreshBootstrap(selectedSessionID: String?) async throws {
        guard let transport, let host else { throw CompanionError.transportUnavailable }
        let data = try await transport.request(.bootstrap(selectedSessionID: selectedSessionID))
        let bootstrap = try WireDecoder.bootstrap(data, expectedHostID: host.hostID)
        try apply(bootstrap, retainingSelection: false)
        publish()
    }

    private func startListener(transport: any ServeClientTransport, token: UUID) {
        listenerTask?.cancel()
        listenerTask = Task { [weak self] in
            do {
                for try await data in transport.events() {
                    guard !Task.isCancelled else { return }
                    await self?.receive(data: data, token: token)
                }
                await self?.transportEnded(token: token)
            } catch {
                guard !Task.isCancelled else { return }
                await self?.transportFailed(token: token, error: error)
            }
        }
    }

    private func receive(data: Data, token: UUID) async {
        guard token == connectionToken, let host else { return }
        do {
            let event = try WireDecoder.streamEvent(data, expectedHostID: host.hostID)
            guard event.hostSequence > hostSequence else { return }
            let hostGap = hostSequence != 0 && event.hostSequence > hostSequence + 1
            hostSequence = event.hostSequence
            if hostGap {
                try await refreshBootstrap(selectedSessionID: selectedSessionID)
                return
            }
            if let catalog = event.catalog {
                let summary = catalog.summary
                summaries[summary.id] = SessionSummaryView(
                    id: summary.id,
                    title: SafeText.value(summary.title, limit: 512) ?? "Untitled session",
                    liveState: summary.liveState,
                    attention: summary.attention,
                    pinned: summary.pinned,
                    archived: summary.archived,
                    modifiedAtMs: summary.modifiedAtMs
                )
                publish()
                return
            }
            guard let sessionEvent = event.event, let projection = projections[sessionEvent.sessionID] else {
                return
            }
            var next = projection
            switch SessionReducer.apply(sessionEvent, to: &next) {
            case .applied:
                projections[sessionEvent.sessionID] = next
                if var summary = summaries[sessionEvent.sessionID] {
                    summary.liveState = next.liveState
                    summary.modifiedAtMs = UInt64(Date().timeIntervalSince1970 * 1_000)
                    summaries[sessionEvent.sessionID] = summary
                }
                publish()
            case .ignored:
                return
            case .needsSnapshot:
                try await recover(sessionID: sessionEvent.sessionID, token: token, after: projection.cursor)
            }
        } catch let error as CompanionError {
            await protocolFailure(error, token: token)
        } catch {
            await protocolFailure(.protocolViolation("event decoding failed"), token: token)
        }
    }

    private func recover(sessionID: String, token: UUID, after cursor: WireCursor) async throws {
        guard token == connectionToken, let transport else { return }
        let data = try await transport.request(.replay(sessionID: sessionID, after: cursor))
        let response = try WireDecoder.replay(data)
        if response.isGap, let snapshot = response.snapshot {
            try WireDecoder.validateForService(snapshot, expectedSessionID: sessionID)
            replace(snapshot)
            publish()
            return
        }
        guard let events = response.events, var projection = projections[sessionID] else { return }
        for event in events {
            guard event.sessionID == sessionID else { throw CompanionError.protocolViolation("replay session mismatch") }
            switch SessionReducer.apply(event, to: &projection) {
            case .applied, .ignored: continue
            case .needsSnapshot:
                if let snapshot = response.snapshot {
                    replace(snapshot)
                    publish()
                    return
                }
                throw CompanionError.staleSession
            }
        }
        projections[sessionID] = projection
        publish()
    }

    private func replace(_ snapshot: WireSessionSnapshot) {
        let title = summaries[snapshot.sessionID]?.title ?? "Session"
        projections[snapshot.sessionID] = snapshot.projection(title: title)
        selectedSessionID = snapshot.sessionID
        if var summary = summaries[snapshot.sessionID] {
            summary.liveState = snapshot.liveState
            summaries[snapshot.sessionID] = summary
        }
    }

    private func deliver(_ command: PendingCommand, host: PairedHost) async throws -> AckInspection {
        guard connection == .connected, let transport else { throw CompanionError.transportUnavailable }
        if command.sessionID == nil {
            var queued = command
            queued.inFlight = true
            pendingCommands[command.id] = queued
            do {
                let data = try await transport.request(.command(command.envelope, commandID: command.id))
                let ack = try CommandEnvelopeEncoder.inspectHostAck(data, hostID: host.hostID, commandID: command.id)
                pendingCommands.removeValue(forKey: command.id)
                return ack
            } catch {
                pendingCommands[command.id]?.inFlight = false
                startReconnectAfterTransportError()
                throw map(error)
            }
        }
        guard let sessionID = command.sessionID,
              let selected = projections[sessionID],
              selected.actorGeneration == command.actorGeneration else {
            throw CompanionError.staleSession
        }
        var queued = command
        queued.inFlight = true
        pendingCommands[command.id] = queued
        do {
            let data = try await transport.request(.command(command.envelope, commandID: command.id))
            let ack = try CommandEnvelopeEncoder.inspectSessionAck(data, hostID: host.hostID, sessionID: sessionID, commandID: command.id)
            pendingCommands.removeValue(forKey: command.id)
            return ack
        } catch {
            pendingCommands[command.id]?.inFlight = false
            startReconnectAfterTransportError()
            throw map(error)
        }
    }

    private func retryPendingCommands() async throws {
        guard let host, connection == .connected, let transport else { return }
        for id in pendingCommands.keys.sorted() {
            guard var pending = pendingCommands[id] else { continue }
            if let sessionID = pending.sessionID,
               projections[sessionID]?.actorGeneration != pending.actorGeneration {
                pendingCommands.removeValue(forKey: id)
                continue
            }
            pending.attempts += 1
            pending.inFlight = true
            pendingCommands[id] = pending
            do {
                let data = try await transport.request(.command(pending.envelope, commandID: pending.id))
                let ack: AckInspection
                if let sessionID = pending.sessionID {
                    ack = try CommandEnvelopeEncoder.inspectSessionAck(data, hostID: host.hostID, sessionID: sessionID, commandID: pending.id)
                } else {
                    ack = try CommandEnvelopeEncoder.inspectHostAck(data, hostID: host.hostID, commandID: pending.id)
                }
                pendingCommands.removeValue(forKey: id)
                if case .rejected = ack { continue }
            } catch {
                pendingCommands[id]?.inFlight = false
                throw map(error)
            }
        }
    }

    private func pairingDeviceID(for host: PairedHost) async throws -> String {
        let configuration = try await pairing.configuration(for: host)
        return configuration.deviceID
    }

    private func selectedProjection() -> SessionProjection? {
        guard let selectedSessionID else { return nil }
        return projections[selectedSessionID]
    }

    private func closeTransport() async {
        listenerTask?.cancel()
        listenerTask = nil
        let current = transport
        transport = nil
        connectionToken = nil
        if let current {
            await current.close()
        }
    }

    private func transportEnded(token: UUID) async {
        guard token == connectionToken else { return }
        await handleTransportLoss()
    }

    private func transportFailed(token: UUID, error: Error) async {
        guard token == connectionToken else { return }
        if error is CancellationError { return }
        await handleTransportLoss()
    }

    private func handleTransportLoss() async {
        await closeTransport()
        guard host != nil, connection != .backgrounded else { return }
        connection = .reconnecting
        lastError = .transportUnavailable
        publish()
        if let host { scheduleReconnect(hostID: host.hostID) }
    }

    private func protocolFailure(_ error: CompanionError, token: UUID) async {
        guard token == connectionToken else { return }
        await closeTransport()
        connection = .failed
        lastError = error
        publish()
    }

    private func startReconnectAfterTransportError() {
        guard let host, connection != .backgrounded else { return }
        connection = .reconnecting
        lastError = .transportUnavailable
        publish()
        scheduleReconnect(hostID: host.hostID)
    }

    private func scheduleReconnect(hostID: String) {
        guard reconnectTask == nil else { return }
        reconnectTask = Task { [weak self] in
            var delay: UInt64 = 1
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: delay * 1_000_000_000)
                guard !Task.isCancelled, let self else { return }
                do {
                    try await self.connect(hostID: hostID)
                    await self.clearReconnectTask()
                    return
                } catch {
                    delay = min(delay * 2, 30)
                }
            }
        }
    }

    private func clearReconnectTask() {
        reconnectTask = nil
    }

    private func map(_ error: Error) -> CompanionError {
        if let error = error as? CompanionError { return error }
        if error is CancellationError { return .cancelled }
        return .transportUnavailable
    }

    private func makeSnapshot() -> CompanionSnapshot {
        CompanionSnapshot(
            connection: connection,
            host: host,
            sessions: summaries.values.sorted { lhs, rhs in
                if lhs.pinned != rhs.pinned { return lhs.pinned && !rhs.pinned }
                return lhs.modifiedAtMs > rhs.modifiedAtMs
            },
            selectedSessionID: selectedSessionID,
            selectedSession: selectedProjection(),
            error: lastError
        )
    }

    private func publish() {
        let value = makeSnapshot()
        for continuation in observers.values {
            continuation.yield(value)
        }
    }

    private func addObserver(id: UUID, continuation: AsyncStream<CompanionSnapshot>.Continuation) {
        observers[id] = continuation
        continuation.yield(makeSnapshot())
        continuation.onTermination = { [weak self] _ in
            Task { await self?.removeObserver(id: id) }
        }
    }

    private func removeObserver(id: UUID) {
        observers.removeValue(forKey: id)
    }
}

private extension WireDecoder {
    static func validateForService(_ snapshot: WireSessionSnapshot, expectedSessionID: String) throws {
        guard snapshot.sessionID == expectedSessionID,
              snapshot.actorGeneration > 0,
              snapshot.cursor.actorGeneration == snapshot.actorGeneration else {
            throw CompanionError.staleSession
        }
    }
}
