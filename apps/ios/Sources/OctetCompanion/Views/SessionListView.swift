import SwiftUI

/// Host-authoritative session inventory. Every row renders only host-reported
/// summary text that the wire layer already truncated.
public struct SessionListView: View {
    @ObservedObject var model: CompanionAppModel

    public var body: some View {
        List {
            if let host = model.snapshot.host {
                Section("Host") {
                    Text(host.name)
                    Text(host.fingerprint)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            Section("Sessions") {
                if model.snapshot.sessions.isEmpty {
                    Text("This host has no sessions yet.")
                        .foregroundStyle(.secondary)
                }
                ForEach(model.snapshot.sessions) { summary in
                    Button {
                        Task { await model.select(sessionID: summary.id) }
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(summary.title)
                            Text("\(summary.liveState) · \(summary.attention)")
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .disabled(model.isWorking)
                }
            }
            Section {
                CompanionStatusView(model: model)
            }
        }
        .navigationTitle("Sessions")
        .toolbar {
            ToolbarItem {
                Button("New session") {
                    Task { await model.createSession() }
                }
                .disabled(model.isWorking)
            }
        }
    }
}
