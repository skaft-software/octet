import SwiftUI

/// Root shell of the companion.
///
/// It routes on host-reported connection state and bridges the app scene phase
/// into the host-owned lifecycle: backgrounding closes the transport and marks
/// the connection `backgrounded`; returning to the foreground reconnects with a
/// fresh `bootstrap`, so a resumed UI can never be stale.
public struct CompanionRootView: View {
    @ObservedObject private var model: CompanionAppModel
    @Environment(\.scenePhase) private var scenePhase

    public init(model: CompanionAppModel) {
        self._model = ObservedObject(wrappedValue: model)
    }

    public var body: some View {
        Group {
            if model.snapshot.connection == .connected {
                if model.snapshot.selectedSession != nil {
                    SessionDetailView(model: model)
                } else {
                    SessionListView(model: model)
                }
            } else {
                PairingGuidanceView(model: model)
            }
        }
        .task { model.startObserving() }
        .onChange(of: scenePhase) { phase in
            switch phase {
            case .active:
                Task { await model.enterForeground() }
            case .background:
                Task { await model.enterBackground() }
            default:
                break
            }
        }
    }
}

/// The single status line every screen shares. It renders only bounded, local
/// text: a host error description or a local refusal.
struct CompanionStatusView: View {
    @ObservedObject var model: CompanionAppModel

    var body: some View {
        if model.isWorking {
            Text("Working…")
                .font(.footnote)
                .accessibilityLabel("Working")
        } else if let failure = model.failure {
            Text(failure)
                .font(.footnote)
                .accessibilityLabel(failure)
        }
    }
}
