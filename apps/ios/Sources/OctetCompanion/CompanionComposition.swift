import Foundation

/// The single composition root for the iOS companion.
///
/// It is deliberately fail-closed: the defaults compose the unavailable client
/// factory and the refusing pairing adapter, so a build that has not yet bound a
/// concrete `OctetServe` transport can pair nothing, connect to nothing and send
/// nothing. A build that owns a real transport supplies both boundaries here,
/// keeping the service actor free of transport and credential knowledge.
public enum CompanionComposition {
    public static func makeService(
        factory: any ServeClientFactory = UnconfiguredServeClientFactory(),
        adapter: any PairingAdapter = NoPairingAdapter(),
        credentialStore: any CompanionCredentialStore = KeychainCredentialStore(),
        hostStore: any PairedHostStore = UserDefaultsPairedHostStore()
    ) -> CompanionSessionService {
        CompanionSessionService(
            factory: factory,
            pairing: PairingCoordinator(
                credentialStore: credentialStore,
                hostStore: hostStore,
                adapter: adapter
            )
        )
    }

    /// Whether this build can reach a host at all. The UI uses it to explain an
    /// unconfigured build instead of pretending the connection is merely offline.
    public static func isHostBound(factory: any ServeClientFactory) -> Bool {
        !(factory is UnconfiguredServeClientFactory)
    }
}
