import Foundation
import ZeusCore

protocol AgentClientProtocol {
    func models() async throws -> ModelsResponse
    func permissions() async throws -> PermissionsResponse
    func createSession() async throws -> CreateSessionResponse
    func restoreSession(sessionID: UInt64) async throws -> RestoreSessionResponse
    func setSessionModel(sessionID: UInt64, model: String) async throws -> SessionModelResponse
    func setSessionPermissions(
        sessionID: UInt64,
        toolPolicy: String
    ) async throws -> SessionPermissionsResponse
    func streamTurn(
        sessionID: UInt64,
        message: String,
        reasoningEffort: String,
        onEvent: @escaping (AgentServerEvent) async -> Void
    ) async throws
}

protocol AgentServerProtocol: AnyObject {
    func start(workspaceURL: URL) async throws -> any AgentClientProtocol
    func stop()
}

protocol AgentAuthProtocol: AnyObject {
    var authFileDisplayPath: String { get }

    func status() async -> RustAgentAuthState
    func runDeviceLogin(onLine: @escaping @MainActor (String) -> Void) async throws
    func cancelLogin()
}
