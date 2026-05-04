import Foundation
import ZeusCore

protocol AgentClientProtocol {
    func models() async throws -> ModelsResponse
    func createSession() async throws -> CreateSessionResponse
    func streamTurn(
        sessionID: UInt64,
        message: String,
        onEvent: @escaping (AgentServerEvent) async -> Void
    ) async throws
}

protocol AgentServerProtocol: AnyObject {
    func start() async throws -> any AgentClientProtocol
    func stop()
}

protocol AgentAuthProtocol: AnyObject {
    var authFileDisplayPath: String { get }

    func status() async -> RustAgentAuthState
    func runDeviceLogin(onLine: @escaping @MainActor (String) -> Void) async throws
    func cancelLogin()
}
