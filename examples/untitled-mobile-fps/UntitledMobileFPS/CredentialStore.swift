import Foundation
import Security

protocol CredentialStoring {
    func credential(for serverId: String) -> String?
    func setCredential(_ credential: String, for serverId: String) throws
    func removeCredential(for serverId: String)
}

struct KeychainCredentialStore: CredentialStoring {
    private let service = "com.example.UntitledMobileFPS.server-credentials"

    func credential(for serverId: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: serverId,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }

    func setCredential(_ credential: String, for serverId: String) throws {
        let key: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: serverId
        ]
        let attributes: [String: Any] = [
            kSecValueData as String: Data(credential.utf8),
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        ]
        let status: OSStatus
        if SecItemCopyMatching(key as CFDictionary, nil) == errSecSuccess {
            status = SecItemUpdate(key as CFDictionary, attributes as CFDictionary)
        } else {
            status = SecItemAdd(key.merging(attributes) { _, new in new } as CFDictionary, nil)
        }
        guard status == errSecSuccess else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status))
        }
    }

    func removeCredential(for serverId: String) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: serverId
        ]
        SecItemDelete(query as CFDictionary)
    }
}
