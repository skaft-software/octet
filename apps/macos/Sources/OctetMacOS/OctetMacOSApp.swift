import SwiftUI

@main
struct OctetMacOSApp: App {
    @StateObject private var model: MacOSAppModel

    init() {
        let model = MacOSAppModel()
        NotificationCoordinator.shared.setRouteHandler { [weak model] route in
            Task { @MainActor in
                model?.openNotification(route)
            }
        }
        _model = StateObject(wrappedValue: model)
    }

    var body: some Scene {
        WindowGroup("Octet Serve") {
            RootView()
                .environmentObject(model)
                .task { model.start() }
        }
        .defaultSize(width: 1_120, height: 760)
        .commands {
            ServeCommands()
        }

        Settings {
            SettingsView()
                .environmentObject(model)
        }
    }
}

private struct ServeCommands: Commands {
    @EnvironmentObject private var model: MacOSAppModel

    var body: some Commands {
        CommandGroup(after: .newItem) {
            Button("New Session") {
                model.openCreateSession()
            }
            .keyboardShortcut("n", modifiers: [.command])
            .disabled(model.bootstrap == nil)
        }

        CommandMenu("Session") {
            Button("Stop Current Run") {
                model.stopCurrentRun()
            }
            .keyboardShortcut(".", modifiers: [.command])
            .disabled(model.currentSession == nil)

            Button("Reconnect") {
                Task { await model.connect() }
            }
            .keyboardShortcut("r", modifiers: [.command, .shift])

            Divider()

            Button("Dismiss Error") {
                model.dismissError()
            }
            .disabled(model.errorBanner == nil)
        }
    }
}

private struct SettingsView: View {
    @EnvironmentObject private var model: MacOSAppModel

    var body: some View {
        Form {
            Section("Serve connection") {
                LabeledContent("Status", value: connectionLabel)
                Button("Reconnect") {
                    Task { await model.connect() }
                }
                .accessibilityHint("Reconnects to the paired authoritative Serve host")
            }
            Section("Privacy") {
                Text("Notifications contain only a session and event route. Octet revalidates the route with the host before opening it.")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .frame(width: 430)
        .padding()
    }

    private var connectionLabel: String {
        switch model.connection {
        case .unpaired: return "Not paired"
        case .discovering: return "Discovering Serve hosts…"
        case .connecting: return "Connecting…"
        case .connected: return "Connected"
        case .reconnecting(let attempt): return "Reconnecting (attempt \(attempt))"
        case .sleeping: return "Sleeping"
        case .disconnected(let reason): return reason.isEmpty ? "Disconnected" : "Disconnected: \(reason)"
        case .revoked: return "Pairing revoked"
        case .hostIdentityChanged: return "Host identity changed"
        }
    }
}
