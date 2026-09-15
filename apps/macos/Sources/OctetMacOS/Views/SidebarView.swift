import SwiftUI
import OctetServe

struct SidebarView: View {
    @EnvironmentObject private var model: MacOSAppModel

    var body: some View {
        VStack(spacing: 0) {
            List(selection: Binding(
                get: { model.selectedSessionID },
                set: { model.selectSession($0) }
            )) {
                Section {
                    ForEach(model.filteredSessions) { session in
                        SessionRow(session: session)
                            .tag(session.id as String?)
                            .contextMenu {
                                Button("Open Session") { model.selectSession(session.id) }
                            }
                    }
                } header: {
                    HStack {
                        Text("Sessions")
                        Spacer()
                        Text("\(model.sessions.count)")
                            .foregroundStyle(.secondary)
                            .accessibilityLabel("\(model.sessions.count) sessions")
                    }
                }

                if !model.pendingMutations.isEmpty {
                    Section("Needs reconciliation") {
                        ForEach(model.pendingMutations) { mutation in
                            Button {
                                model.retryReconciliation(mutation)
                            } label: {
                                Label(mutation.label, systemImage: "questionmark.circle")
                                    .help("Check the host result without sending the action again")
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("Reconcile \(mutation.label), command \(mutation.id)")
                        }
                    }
                }
            }
            .listStyle(.sidebar)
            .searchable(text: $model.searchText, placement: .sidebar, prompt: "Search sessions")

            Divider()
            HStack {
                Button {
                    model.openCreateSession()
                } label: {
                    Label("New session", systemImage: "plus")
                }
                .buttonStyle(.borderless)
                .keyboardShortcut("n", modifiers: [.command])
                .disabled(model.bootstrap == nil)
                Spacer()
            }
            .padding(10)
        }
        .navigationTitle("Octet Serve")
        .toolbar {
            ToolbarItem {
                Button {
                    Task { await model.connect() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .help("Reconnect")
                .accessibilityLabel("Reconnect to Serve host")
            }
        }
    }
}

private struct SessionRow: View {
    let session: SessionSummary

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: iconName)
                .foregroundStyle(iconColor)
                .frame(width: 18)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 5) {
                    Text(session.title)
                        .lineLimit(1)
                    if session.pinned {
                        Image(systemName: "pin.fill")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .accessibilityLabel("Pinned")
                    }
                    if session.attentionCount > 0 {
                        Text("\(session.attentionCount)")
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(.white)
                            .padding(.horizontal, 5)
                            .padding(.vertical, 2)
                            .background(.orange, in: Capsule())
                            .accessibilityLabel("\(session.attentionCount) items need attention")
                    }
                    Spacer(minLength: 0)
                    if let pullRequest = session.pullRequest {
                        PullRequestBadge(status: pullRequest)
                    }
                }
                Text(session.preview)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                HStack(spacing: 5) {
                    Text(session.projectID)
                    Text("·")
                    Text(session.updatedAt, style: .relative)
                }
                .font(.caption2)
                .foregroundStyle(.tertiary)
            }
        }
        .padding(.vertical, 3)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityText)
    }

    private var statusLabel: String {
        switch session.status {
        case .idle: return "Idle"
        case .running: return "Running"
        case .waitingForApproval: return "Waiting for approval"
        case .waitingForInput: return "Waiting for input"
        case .completed: return "Completed"
        case .failed: return "Failed"
        case .cancelled: return "Cancelled"
        case .archived: return "Archived"
        }
    }

    private var iconName: String {
        switch session.status {
        case .running: return "arrow.triangle.2.circlepath"
        case .waitingForApproval, .waitingForInput: return "pause.circle"
        case .failed: return "xmark.circle"
        case .cancelled: return "stop.circle"
        case .completed: return "checkmark.circle"
        case .idle, .archived: return "circle"
        }
    }

    private var iconColor: Color {
        switch session.status {
        case .running: return .blue
        case .waitingForApproval, .waitingForInput: return .orange
        case .failed: return .red
        case .completed: return .green
        case .idle, .cancelled, .archived: return .secondary
        }
    }

    private var accessibilityText: String {
        var value = "\(session.title), \(statusLabel), project \(session.projectID)"
        if session.attentionCount > 0 { value += ", needs attention" }
        if session.pullRequest != nil { value += ", pull request available" }
        return value
    }
}

struct PullRequestBadge: View {
    let status: PullRequestStatus

    var body: some View {
        Label(label, systemImage: symbol)
            .font(.caption2)
            .foregroundStyle(color)
            .labelStyle(.iconOnly)
            .help(label)
            .accessibilityLabel("Pull request: \(label)")
    }

    private var label: String {
        switch status.state {
        case .unknown: return "Status unavailable"
        case .open: return "Open pull request"
        case .ready: return "Ready pull request"
        case .failed: return "Check failed"
        case .merged: return "Merged"
        case .closed: return "Closed"
        }
    }

    private var symbol: String {
        switch status.state {
        case .merged: return "arrow.merge"
        case .failed: return "exclamationmark.triangle"
        case .unknown, .open, .ready, .closed: return "arrow.up.right.square"
        }
    }

    private var color: Color {
        switch status.state {
        case .merged: return .purple
        case .failed: return .red
        case .unknown: return .secondary
        case .open, .ready, .closed: return .blue
        }
    }
}
