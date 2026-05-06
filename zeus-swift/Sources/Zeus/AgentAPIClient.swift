import Foundation
import ZeusCore

struct AgentAPIClient: AgentClientProtocol {
    let baseURL: URL
    let token: String

    func healthz() async -> Bool {
        do {
            var request = URLRequest(url: baseURL.appendingPathComponent("healthz"))
            request.timeoutInterval = 1
            let (_, response) = try await URLSession.shared.data(for: request)
            return (response as? HTTPURLResponse)?.statusCode == 200
        } catch {
            return false
        }
    }

    func identity() async throws -> ServerIdentityResponse {
        var request = URLRequest(url: baseURL)
        request.httpMethod = "GET"
        request.timeoutInterval = 2
        let data = try await data(for: request)
        return try JSONDecoder().decode(ServerIdentityResponse.self, from: data)
    }

    func models() async throws -> ModelsResponse {
        var request = authenticatedRequest(path: "models")
        request.httpMethod = "GET"
        let data = try await data(for: request)
        return try JSONDecoder().decode(ModelsResponse.self, from: data)
    }

    func permissions() async throws -> PermissionsResponse {
        var request = authenticatedRequest(path: "permissions")
        request.httpMethod = "GET"
        let data = try await data(for: request)
        return try JSONDecoder().decode(PermissionsResponse.self, from: data)
    }

    func workspace() async throws -> WorkspaceResponse {
        var request = authenticatedRequest(path: "workspace")
        request.httpMethod = "GET"
        let data = try await data(for: request)
        return try JSONDecoder().decode(WorkspaceResponse.self, from: data)
    }

    func switchWorkspaceBranch(branch: String) async throws -> SwitchWorkspaceBranchResponse {
        var request = authenticatedRequest(path: "workspace/branch")
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        request.httpBody = try JSONEncoder().encode(SwitchWorkspaceBranchRequest(branch: branch))
        let data = try await data(for: request)
        return try JSONDecoder().decode(SwitchWorkspaceBranchResponse.self, from: data)
    }

    func createSession() async throws -> CreateSessionResponse {
        var request = authenticatedRequest(path: "sessions")
        request.httpMethod = "POST"
        let data = try await data(for: request)
        return try JSONDecoder().decode(CreateSessionResponse.self, from: data)
    }

    func restoreSession(sessionID: UInt64) async throws -> RestoreSessionResponse {
        var request = authenticatedRequest(path: "sessions:restore")
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        request.httpBody = try JSONEncoder().encode(RestoreSessionRequest(sessionID: sessionID))
        let data = try await data(for: request)
        return try JSONDecoder().decode(RestoreSessionResponse.self, from: data)
    }

    func setSessionModel(sessionID: UInt64, model: String) async throws -> SessionModelResponse {
        var request = authenticatedRequest(path: "sessions/\(sessionID)/model")
        request.httpMethod = "PUT"
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        request.httpBody = try JSONEncoder().encode(SetModelRequest(model: model))
        let data = try await data(for: request)
        return try JSONDecoder().decode(SessionModelResponse.self, from: data)
    }

    func setSessionPermissions(
        sessionID: UInt64,
        toolPolicy: String
    ) async throws -> SessionPermissionsResponse {
        var request = authenticatedRequest(path: "sessions/\(sessionID)/permissions")
        request.httpMethod = "PUT"
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        request.httpBody = try JSONEncoder().encode(SetPermissionsRequest(toolPolicy: toolPolicy))
        let data = try await data(for: request)
        return try JSONDecoder().decode(SessionPermissionsResponse.self, from: data)
    }

    func streamTurn(
        sessionID: UInt64,
        message: String,
        reasoningEffort: String,
        onEvent: @escaping (AgentServerEvent) async -> Void
    ) async throws {
        var request = authenticatedRequest(path: "sessions/\(sessionID)/turns:stream")
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        request.setValue("text/event-stream", forHTTPHeaderField: "accept")
        request.httpBody = try JSONEncoder().encode(TurnRequest(
            message: message,
            reasoningEffort: reasoningEffort
        ))

        let (bytes, response) = try await URLSession.shared.bytes(for: request)
        try validate(response: response)

        var parser = SSEStreamParser()
        var receivedEvent = false

        for try await byte in bytes {
            if let frame = parser.append(byte),
               let event = try decodeEvent(fromFrame: frame) {
                receivedEvent = true
                await onEvent(event)
            }
        }

        if let frame = parser.finish(),
           let event = try decodeEvent(fromFrame: frame) {
            receivedEvent = true
            await onEvent(event)
        }

        if !receivedEvent {
            throw AgentClientError.noStreamEvents(parser.preview)
        }
    }

    private func authenticatedRequest(path: String) -> URLRequest {
        var request = URLRequest(url: baseURL.appendingPathComponent(path))
        request.setValue("Bearer \(token)", forHTTPHeaderField: "authorization")
        request.timeoutInterval = 120
        return request
    }

    private func data(for request: URLRequest) async throws -> Data {
        let (data, response) = try await URLSession.shared.data(for: request)
        try validate(response: response, data: data)
        return data
    }

    private func validate(response: URLResponse, data: Data = Data()) throws {
        guard let http = response as? HTTPURLResponse else {
            throw AgentClientError.invalidResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            let body = String(data: data, encoding: .utf8)
            throw AgentClientError.httpStatus(http.statusCode, body)
        }
    }

    private func decodeEvent(fromFrame data: Data) throws -> AgentServerEvent? {
        guard let text = String(data: data, encoding: .utf8), !text.isEmpty else {
            return nil
        }

        let normalized = text
            .replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
        let dataLines = normalized
            .split(separator: "\n", omittingEmptySubsequences: false)
            .compactMap { line -> String? in
                guard line.hasPrefix("data:") else { return nil }
                let value = line.dropFirst(5)
                return value.first == " " ? String(value.dropFirst()) : String(value)
            }

        guard !dataLines.isEmpty else { return nil }
        let eventData = Data(dataLines.joined(separator: "\n").utf8)
        return try JSONDecoder().decode(AgentServerEvent.self, from: eventData)
    }
}

private struct SSEStreamParser {
    private var buffer = Data()
    private var previewData = Data()

    var preview: String {
        String(data: previewData, encoding: .utf8) ?? ""
    }

    mutating func append(_ byte: UInt8) -> Data? {
        if previewData.count < 1_000 {
            previewData.append(byte)
        }

        buffer.append(byte)
        if buffer.hasSuffixBytes([13, 10, 13, 10]) {
            return frame(dropping: 4)
        }
        if buffer.hasSuffixBytes([10, 10]) {
            return frame(dropping: 2)
        }
        return nil
    }

    mutating func finish() -> Data? {
        guard !buffer.isEmpty else { return nil }
        defer { buffer.removeAll(keepingCapacity: true) }
        return buffer
    }

    private mutating func frame(dropping terminatorLength: Int) -> Data {
        let frame = buffer.dropLast(terminatorLength)
        buffer.removeAll(keepingCapacity: true)
        return Data(frame)
    }
}

private extension Data {
    func hasSuffixBytes(_ bytes: [UInt8]) -> Bool {
        guard count >= bytes.count else { return false }
        return suffix(bytes.count).elementsEqual(bytes)
    }
}

enum AgentClientError: LocalizedError {
    case invalidResponse
    case httpStatus(Int, String?)
    case noStreamEvents(String)

    var errorDescription: String? {
        switch self {
        case .invalidResponse:
            return "rust-agent returned an invalid response."
        case let .httpStatus(status, body):
            if let body, !body.isEmpty {
                return "rust-agent returned HTTP \(status): \(body)"
            }
            return "rust-agent returned HTTP \(status)."
        case let .noStreamEvents(preview):
            if preview.isEmpty {
                return "rust-agent returned an empty stream."
            }
            return "rust-agent returned no parseable stream events: \(preview)"
        }
    }
}

struct ServerIdentityResponse: Decodable {
    let name: String
    let protocolDescription: String
    let workspaceRoot: String

    enum CodingKeys: String, CodingKey {
        case name
        case protocolDescription = "protocol"
        case workspaceRoot = "workspace_root"
    }
}

struct ModelsResponse: Decodable {
    let defaultModel: String
    let allowedModels: [String]
    let defaultReasoningEffort: String
    let reasoningEfforts: [String]

    enum CodingKeys: String, CodingKey {
        case defaultModel = "default_model"
        case allowedModels = "allowed_models"
        case defaultReasoningEffort = "default_reasoning_effort"
        case reasoningEfforts = "reasoning_efforts"
    }
}

struct PermissionsResponse: Decodable {
    let defaultToolPolicy: String
    let allowedToolPolicies: [String]

    enum CodingKeys: String, CodingKey {
        case defaultToolPolicy = "default_tool_policy"
        case allowedToolPolicies = "allowed_tool_policies"
    }
}

struct CreateSessionResponse: Decodable {
    let sessionID: UInt64
    let model: String
    let toolPolicy: String?

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case model
        case toolPolicy = "tool_policy"
    }
}

struct RestoreSessionResponse: Decodable {
    let sessionID: UInt64
    let model: String
    let toolPolicy: String
    let messages: [TranscriptRecord]

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case model
        case toolPolicy = "tool_policy"
        case messages
    }
}

struct TranscriptRecord: Decodable, Equatable {
    let messageID: UInt64
    let kind: String
    let role: String?
    let text: String?
    let toolCallID: String?
    let toolName: String?
    let toolArguments: String?
    let success: Bool?

    enum CodingKeys: String, CodingKey {
        case messageID = "message_id"
        case kind
        case role
        case text
        case toolCallID = "tool_call_id"
        case toolName = "tool_name"
        case toolArguments = "tool_arguments"
        case success
    }
}

struct SessionModelResponse: Decodable {
    let model: String
}

struct SessionPermissionsResponse: Decodable {
    let toolPolicy: String

    enum CodingKeys: String, CodingKey {
        case toolPolicy = "tool_policy"
    }
}

struct WorkspaceResponse: Decodable {
    let workspaceRoot: String
    let branch: String?
    let branches: [String]
    let git: Bool

    enum CodingKeys: String, CodingKey {
        case workspaceRoot = "workspace_root"
        case branch
        case branches
        case git
    }
}

struct SwitchWorkspaceBranchResponse: Decodable {
    let previousBranch: String?
    let branch: String
    let stashedChanges: Bool
    let workspace: WorkspaceResponse

    enum CodingKeys: String, CodingKey {
        case previousBranch = "previous_branch"
        case branch
        case stashedChanges = "stashed_changes"
        case workspace
    }
}

private struct SetModelRequest: Encodable {
    let model: String
}

private struct SetPermissionsRequest: Encodable {
    let toolPolicy: String

    enum CodingKeys: String, CodingKey {
        case toolPolicy = "tool_policy"
    }
}

private struct SwitchWorkspaceBranchRequest: Encodable {
    let branch: String
}

private struct RestoreSessionRequest: Encodable {
    let sessionID: UInt64

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
    }
}

struct TurnRequest: Encodable {
    let message: String
    let reasoningEffort: String

    enum CodingKeys: String, CodingKey {
        case message
        case reasoningEffort = "reasoning_effort"
    }
}
