import SwiftUI

/// Shown whenever the app is not attached to a connected host. Pairing is
/// host-driven: the app can never mint a ticket, trust an unverified
/// fingerprint, or reach an unencrypted endpoint.
public struct PairingGuidanceView: View {
    @ObservedObject var model: CompanionAppModel

    public var body: some View {
        VStack(spacing: 16) {
            Text("Octet Companion")
                .font(.title2)
            Text(model.connectionSummary)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            if let host = model.snapshot.host {
                VStack(spacing: 4) {
                    Text(host.name)
                        .font(.headline)
                    Text("Fingerprint: \(host.fingerprint)")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }
            }
            CompanionStatusView(model: model)
            VStack(spacing: 8) {
                Button("Connect to paired host") {
                    Task { await model.connect() }
                }
                .disabled(model.isWorking)
                Button("Reconnect") {
                    Task { await model.reconnect() }
                }
                .disabled(model.isWorking)
            }
        }
        .padding()
        .navigationTitle("Companion")
    }
}
