import SwiftUI
import OctetServeClient

struct RootView: View {
    @EnvironmentObject private var model: MacOSAppModel

    var body: some View {
        Group {
            if model.needsPairing {
                PairingView()
            } else {
                WorkspaceView()
            }
        }
        .overlay(alignment: .top) {
            if let error = model.errorBanner {
                ErrorBanner(message: error) {
                    model.dismissError()
                }
                .padding(.top, 8)
                .padding(.horizontal, 12)
            }
        }
        .sheet(
            isPresented: Binding(
                get: { model.isShowingCreateSession },
                set: { if !$0 { model.closeCreateSession() } }
            )
        ) {
            CreateSessionView()
                .environmentObject(model)
        }
    }
}

extension MacOSAppModel {
    var needsPairing: Bool {
        switch connection {
        case .unpaired, .revoked, .hostIdentityChanged:
            return true
        default:
            return false
        }
    }
}

struct WorkspaceView: View {
    @EnvironmentObject private var model: MacOSAppModel

    var body: some View {
        NavigationSplitView {
            SidebarView()
                .navigationSplitViewColumnWidth(min: 250, ideal: 300, max: 380)
        } detail: {
            SessionDetailView()
        }
        .safeAreaInset(edge: .top, spacing: 0) {
            ConnectionBanner()
        }
        .background(Color(nsColor: .windowBackgroundColor))
    }
}

struct ErrorBanner: View {
    let message: String
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.yellow)
                .accessibilityHidden(true)
            Text(message)
                .font(.callout)
                .lineLimit(3)
            Spacer(minLength: 8)
            Button("Dismiss", action: dismiss)
                .buttonStyle(.borderless)
        }
        .padding(10)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(.quaternary))
        .shadow(radius: 4)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Serve error: \(message)")
    }
}

struct ConnectionBanner: View {
    @EnvironmentObject private var model: MacOSAppModel

    var body: some View {
        HStack(spacing: 8) {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
                .accessibilityHidden(true)
            Text(label)
                .font(.caption)
                .foregroundStyle(.secondary)
            Spacer()
            if canReconnect {
                Button("Reconnect") {
                    Task { await model.connect() }
                }
                .buttonStyle(.link)
                .keyboardShortcut("r", modifiers: [.command, .shift])
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .background(.bar)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Serve connection: \(label)")
    }

    private var label: String {
        switch model.connection {
        case .connected:
            return "Connected to authoritative Serve host"
        case .connecting:
            return "Connecting to Serve host…"
        case .reconnecting(let attempt):
            return "Reconnecting (attempt \(attempt)); local drafts are preserved"
        case .sleeping:
            return "Mac is sleeping; reconnecting after wake"
        case .revoked:
            return "Pairing revoked"
        case .hostIdentityChanged:
            return "Host identity changed"
        case .unpaired:
            return "Not paired"
        case .discovering:
            return "Discovering Serve hosts…"
        case .disconnected(let reason):
            return reason.isEmpty ? "Disconnected; no new action was sent" : "Disconnected: \(reason)"
        }
    }

    private var color: Color {
        switch model.connection {
        case .connected: return .green
        case .connecting, .reconnecting: return .orange
        case .sleeping: return .gray
        default: return .red
        }
    }

    private var canReconnect: Bool {
        if case .disconnected = model.connection { return true }
        return false
    }
}

struct PairingView: View {
    @EnvironmentObject private var model: MacOSAppModel
    @State private var manualEndpoint = ""
    @State private var ticket = ""
    @State private var confirmedFingerprint = false

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            HStack {
                Image(systemName: "lock.shield")
                    .font(.system(size: 34))
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Pair with Serve")
                        .font(.title)
                        .fontWeight(.semibold)
                    Text("Choose the authoritative host for this Mac.")
                        .foregroundStyle(.secondary)
                }
            }

            Text("Pairing verifies the host identity before this app receives session data. A ticket is one-time and is stored only by the shared client after the host approves this device.")
                .fixedSize(horizontal: false, vertical: true)
                .foregroundStyle(.secondary)

            HStack(spacing: 10) {
                TextField("Host address or Bonjour name", text: $manualEndpoint)
                    .textFieldStyle(.roundedBorder)
                    .accessibilityLabel("Manual Serve host address")
                Button("Inspect address") {
                    model.inspectManualHost(endpoint: manualEndpoint)
                }
                .disabled(manualEndpoint.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }

            HStack {
                Text("Nearby hosts")
                    .font(.headline)
                Spacer()
                Button("Discover") { model.startDiscovery() }
                    .keyboardShortcut("d", modifiers: [.command])
            }

            if model.discoveredHosts.isEmpty {
                VStack(spacing: 8) {
                    Label("No host selected", systemImage: "network")
                        .font(.headline)
                    Text("Discover a host on the local network or inspect a host address.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                }
                .frame(maxWidth: .infinity, minHeight: 130)
                .accessibilityElement(children: .combine)
            } else {
                List(model.discoveredHosts) { host in
                    Button {
                        model.inspectHost(host)
                    } label: {
                        HStack {
                            Image(systemName: "server.rack")
                                .accessibilityHidden(true)
                            VStack(alignment: .leading) {
                                Text(host.name)
                                Text(host.endpoint)
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            Image(systemName: "chevron.right")
                                .foregroundStyle(.secondary)
                                .accessibilityHidden(true)
                        }
                    }
                    .buttonStyle(.plain)
                    .padding(.vertical, 4)
                    .accessibilityLabel("Inspect host \(host.name) at \(host.endpoint)")
                }
                .listStyle(.inset)
                .frame(minHeight: 120, maxHeight: 190)
            }

            pairingForm
        }
        .padding(32)
        .frame(minWidth: 720, minHeight: 570)
        .task {
            if model.discoveredHosts.isEmpty { model.startDiscovery() }
        }
        .onChange(of: model.pairingHostID) { _ in
            confirmedFingerprint = false
            ticket = ""
        }
        .accessibilityElement(children: .contain)
    }

    @ViewBuilder
    private var pairingForm: some View {
        switch model.pairing {
        case .awaitingTicket(let info):
            VStack(alignment: .leading, spacing: 10) {
                Text("Verify and approve \(info.hostName)")
                    .font(.headline)
                Text("Compare these words with the host's approval screen before continuing:")
                    .foregroundStyle(.secondary)
                Text(info.fingerprintWords.joined(separator: "   "))
                    .font(.system(.body, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 6))
                    .accessibilityLabel("Host fingerprint words: \(info.fingerprintWords.joined(separator: ", "))")
                Toggle("I compared these words with the Serve host", isOn: $confirmedFingerprint)
                    .toggleStyle(.checkbox)
                    .accessibilityHint("Confirm the displayed fingerprint before pairing")
                HStack {
                    TextField("One-time pairing ticket", text: $ticket)
                        .textFieldStyle(.roundedBorder)
                        .accessibilityLabel("One-time pairing ticket")
                    Button("Pair") {
                        model.submitPairing(ticket: ticket, fingerprintWords: info.fingerprintWords)
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(!confirmedFingerprint || ticket.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .padding(12)
            .background(.quaternary.opacity(0.35), in: RoundedRectangle(cornerRadius: 8))
        case .checking(let host):
            Label("Checking \(host.name)…", systemImage: "hourglass")
        case .waiting:
            HStack {
                ProgressView()
                Text("Waiting for host approval…")
                Spacer()
                Button("Cancel") { model.cancelPairing() }
            }
        case .approved(let deviceName):
            Label("Paired as \(deviceName). Connecting…", systemImage: "checkmark.seal.fill")
                .foregroundStyle(.green)
        case .denied(let reason):
            Label(reason, systemImage: "xmark.octagon")
                .foregroundStyle(.red)
        case .failed(let message):
            Label(message, systemImage: "exclamationmark.triangle")
                .foregroundStyle(.red)
        case .discovering:
            Label("Searching the local network…", systemImage: "dot.radiowaves.left.and.right")
                .foregroundStyle(.secondary)
        case .idle:
            EmptyView()
        }
    }
}
