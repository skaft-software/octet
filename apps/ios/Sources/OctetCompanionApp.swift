import SwiftUI

/// The iOS companion application entry point.
///
/// The companion is a controller for an authoritative Serve host: it renders
/// host-reported state and sends host-validated commands. The default
/// composition is deliberately fail-closed — `CompanionComposition` composes the
/// unavailable client factory and the refusing pairing adapter — so a build that
/// has not yet bound an `OctetServe` transport can pair nothing, connect to
/// nothing and send nothing. A shipping build supplies its adapter in
/// `CompanionComposition.makeService`.
@main
struct OctetCompanionApp: App {
    @StateObject private var model: CompanionAppModel

    init() {
        _model = StateObject(wrappedValue: CompanionAppModel(service: CompanionComposition.makeService()))
    }

    var body: some Scene {
        WindowGroup {
            CompanionRootView(model: model)
        }
    }
}
