import SwiftUI

/// One transcript entry. Provisional (streaming) entries are labelled so a
/// person never mistakes host-streamed text for a committed record.
struct TranscriptEntryRow: View {
    let entry: TranscriptEntry

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(entry.title ?? Self.label(for: entry.kind))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                if let status = entry.status {
                    Text(status)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                if entry.isProvisional {
                    Text("streaming")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }
            if !entry.text.isEmpty {
                Text(entry.text)
                    .textSelection(.enabled)
            }
            if let summary = entry.summary {
                Text(summary)
                    .foregroundStyle(.secondary)
            }
            if let target = entry.target {
                Text(target)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.vertical, 2)
    }

    private static func label(for kind: TranscriptKind) -> String {
        switch kind {
        case .user: return "You"
        case .assistant: return "Assistant"
        case .reasoning: return "Reasoning"
        case .tool: return "Tool"
        case .system: return "Host"
        }
    }
}

/// Selected-session screen: transcript, host-requested decisions, composer and
/// the explicit stop control. Prompt delivery is always host-authoritative, so
/// every mutation goes through the service and its acknowledgement inspection.
public struct SessionDetailView: View {
    @ObservedObject var model: CompanionAppModel

    public var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    if let session = model.selectedSession {
                        sessionHeader(session)
                        ForEach(session.items) { entry in
                            TranscriptEntryRow(entry: entry)
                        }
                        ForEach(session.pendingRequests.filter(\.isPending)) { request in
                            PendingRequestCard(model: model, request: request)
                        }
                    } else {
                        Text("No session is selected.")
                            .foregroundStyle(.secondary)
                    }
                }
                .padding()
            }
            Divider()
            composer
        }
        .navigationTitle(model.selectedSession?.title ?? "Session")
    }

    @ViewBuilder
    private func sessionHeader(_ session: SessionProjection) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(session.liveState)
                .font(.headline)
            if !session.modelLabel.isEmpty {
                Text(session.modelLabel)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            if session.activeRunID != nil {
                Button("Stop run") {
                    Task { await model.stopRun() }
                }
                .disabled(model.isWorking)
            }
        }
    }

    private var composer: some View {
        VStack(alignment: .leading, spacing: 8) {
            CompanionStatusView(model: model)
            TextField("Message the host", text: $model.composer, axis: .vertical)
                .lineLimit(1...6)
            HStack(spacing: 8) {
                Button("Send") { Task { await model.send(delivery: .submit) } }
                Button("Steer") { Task { await model.send(delivery: .steer) } }
                Button("Follow up") { Task { await model.send(delivery: .followUp) } }
            }
            .disabled(model.isWorking)
        }
        .padding()
    }
}

/// A host-requested decision. The app answers the request the host named with
/// the actor generation the host reported; it never invents one.
struct PendingRequestCard: View {
    @ObservedObject var model: CompanionAppModel
    let request: PendingRequestView

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(request.title)
                .font(.subheadline)
            if request.kind == "input" {
                TextField("Answer", text: $model.answerInput, axis: .vertical)
                    .lineLimit(1...4)
                Button("Send answer") {
                    Task { await model.answerText(requestID: request.id) }
                }
                .disabled(model.isWorking)
            } else {
                HStack(spacing: 8) {
                    Button("Approve") {
                        Task { await model.answerApproval(requestID: request.id, allowed: true) }
                    }
                    Button("Decline") {
                        Task { await model.answerApproval(requestID: request.id, allowed: false) }
                    }
                }
                .disabled(model.isWorking)
            }
        }
        .padding(8)
        .background(.thinMaterial)
    }
}
