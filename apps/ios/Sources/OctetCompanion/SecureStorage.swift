import Foundation
import Security

public protocol CompanionCredentialStore: Sendable {
    func save(_ credential: Data, reference: String) async throws
    func load(reference: String) async throws -> Data
    func delete(reference: String) async throws
    func deviceID() async throws -> String
}

public actor KeychainCredentialStore: CompanionCredentialStore {
    private let service: String
    private let accessGroup: String?

    public init(service: String = "org.octet.companion", accessGroup: String? = nil) {
        self.service = service
        self.accessGroup = accessGroup
    }

    public func save(_ credential: Data, reference: String) async throws {
        guard !credential.isEmpty, validReference(reference) else { throw CompanionError.invalidInput("The credential reference is invalid.") }
        let query = baseQuery(reference: reference)
        let attributes: [String: Any] = [kSecValueData as String: credential]
        let updateStatus = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if updateStatus == errSecSuccess { return }
        guard updateStatus == errSecItemNotFound else { throw keychainError(updateStatus) }
        var item = query
        item[kSecValueData as String] = credential
        item[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let status = SecItemAdd(item as CFDictionary, nil)
        guard status == errSecSuccess else { throw keychainError(status) }
    }

    public func load(reference: String) async throws -> Data {
        guard validReference(reference) else { throw CompanionError.credentialRejected }
        var query = baseQuery(reference: reference)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess, let data = result as? Data, !data.isEmpty else {
            if status == errSecItemNotFound { throw CompanionError.credentialRejected }
            throw keychainError(status)
        }
        return data
    }

    public func delete(reference: String) async throws {
        guard validReference(reference) else { return }
        let status = SecItemDelete(baseQuery(reference: reference) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else { throw keychainError(status) }
    }

    public func deviceID() async throws -> String {
        let reference = "device-id"
        do {
            let data = try await load(reference: reference)
            guard let value = String(data: data, encoding: .utf8), isValidDeviceID(value) else {
                throw CompanionError.protocolViolation("stored device identity is invalid")
            }
            return value
        } catch CompanionError.credentialRejected {
            let value = UUID().uuidString.lowercased()
            try await save(Data(value.utf8), reference: reference)
            return value
        }
    }

    private func baseQuery(reference: String) -> [String: Any] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: reference
        ]
        if let accessGroup { query[kSecAttrAccessGroup as String] = accessGroup }
        return query
    }

    private func validReference(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.count <= 256 && value.allSatisfy { $0.isLetter || $0.isNumber || $0 == "." || $0 == "-" || $0 == "_" }
    }

    private func isValidDeviceID(_ value: String) -> Bool {
        value.count == 36 && UUID(uuidString: value) != nil
    }

    private func keychainError(_ status: OSStatus) -> CompanionError {
        if status == errSecAuthFailed || status == errSecInteractionNotAllowed {
            return .credentialRejected
        }
        return .protocolViolation("secure storage is unavailable")
    }
}

public protocol PairedHostStore: Sendable {
    func all() async throws -> [PairedHost]
    func save(_ host: PairedHost) async throws
    func remove(hostID: String) async throws
}

public actor UserDefaultsPairedHostStore: PairedHostStore {
    private let defaults: UserDefaults
    private let key: String

    public init(defaults: UserDefaults = .standard, key: String = "octet.companion.pairedHosts") {
        self.defaults = defaults
        self.key = key
    }

    public func all() async throws -> [PairedHost] {
        guard let data = defaults.data(forKey: key) else { return [] }
        let hosts = try JSONDecoder().decode([PairedHost].self, from: data)
        return hosts.filter { Self.isSecure($0.endpoint) }
    }

    public func save(_ host: PairedHost) async throws {
        guard Self.isSecure(host.endpoint), !host.hostID.isEmpty, !host.fingerprint.isEmpty,
              !host.credentialReference.isEmpty else {
            throw CompanionError.invalidInput("The paired host record is invalid.")
        }
        var hosts = try await all()
        hosts.removeAll { $0.hostID == host.hostID }
        hosts.append(host)
        let data = try JSONEncoder().encode(hosts)
        defaults.set(data, forKey: key)
    }

    public func remove(hostID: String) async throws {
        var hosts = try await all()
        hosts.removeAll { $0.hostID == hostID }
        defaults.set(try JSONEncoder().encode(hosts), forKey: key)
    }

    private static func isSecure(_ endpoint: URL) -> Bool {
        endpoint.scheme?.lowercased() == "https" || endpoint.scheme?.lowercased() == "wss"
    }
}

public actor PairingCoordinator {
    private let credentialStore: any CompanionCredentialStore
    private let hostStore: any PairedHostStore
    private let adapter: any PairingAdapter

    public init(
        credentialStore: any CompanionCredentialStore = KeychainCredentialStore(),
        hostStore: any PairedHostStore = UserDefaultsPairedHostStore(),
        adapter: any PairingAdapter = NoPairingAdapter()
    ) {
        self.credentialStore = credentialStore
        self.hostStore = hostStore
        self.adapter = adapter
    }

    public func pairedHosts() async throws -> [PairedHost] {
        try await hostStore.all()
    }

    public func pair(candidate: PairingCandidate, confirmed fingerprint: FingerprintConfirmation) async throws -> PairedHost {
        guard !candidate.isExpired else { throw CompanionError.pairingExpired }
        guard fingerprint.hostID == candidate.hostID,
              normalizedFingerprint(fingerprint.fingerprint) == normalizedFingerprint(candidate.fingerprint) else {
            throw CompanionError.hostIdentityChanged
        }
        guard candidate.endpoint.scheme?.lowercased() == "https" || candidate.endpoint.scheme?.lowercased() == "wss" else {
            throw CompanionError.invalidPairingInput
        }
        try await adapter.verifyPeerIdentity(candidate: candidate)
        let deviceID = try await credentialStore.deviceID()
        let receipt = try await adapter.pair(candidate: candidate, confirmation: fingerprint, deviceID: deviceID)
        guard receipt.hostID == candidate.hostID,
              normalizedFingerprint(receipt.fingerprint) == normalizedFingerprint(candidate.fingerprint),
              receipt.deviceID == deviceID,
              !receipt.credential.isEmpty,
              receipt.endpoint.scheme?.lowercased() == "https" || receipt.endpoint.scheme?.lowercased() == "wss" else {
            throw CompanionError.protocolViolation("pairing response did not match the verified host")
        }
        let reference = "credential." + UUID().uuidString.lowercased()
        try await credentialStore.save(receipt.credential, reference: reference)
        let host = PairedHost(
            hostID: receipt.hostID,
            name: SafeText.value(receipt.name, limit: 256) ?? candidate.name,
            endpoint: receipt.endpoint,
            fingerprint: normalizedFingerprint(receipt.fingerprint),
            deviceID: receipt.deviceID,
            credentialReference: reference,
            pairedAtMs: UInt64(max(0, Date().timeIntervalSince1970 * 1_000))
        )
        do {
            try await hostStore.save(host)
        } catch {
            try? await credentialStore.delete(reference: reference)
            throw error
        }
        return host
    }

    public func unpair(_ host: PairedHost) async throws {
        try await credentialStore.delete(reference: host.credentialReference)
        try await hostStore.remove(hostID: host.hostID)
    }

    public func configuration(for host: PairedHost) async throws -> ServeClientConfiguration {
        let credential = try await credentialStore.load(reference: host.credentialReference)
        return try ServeClientConfiguration(
            endpoint: host.endpoint,
            hostID: host.hostID,
            fingerprint: host.fingerprint,
            deviceID: host.deviceID,
            credential: credential
        )
    }

    private func normalizedFingerprint(_ value: String) -> String {
        value.lowercased().filter { $0.isNumber || ($0 >= "a" && $0 <= "f") }
    }
}
