import Foundation

struct AgentAPIClient {
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

    func models() async throws -> ModelsResponse {
        var request = authenticatedRequest(path: "models")
        request.httpMethod = "GET"
        let data = try await data(for: request)
        return try JSONDecoder().decode(ModelsResponse.self, from: data)
    }

    func createSession() async throws -> CreateSessionResponse {
        var request = authenticatedRequest(path: "sessions")
        request.httpMethod = "POST"
        let data = try await data(for: request)
        return try JSONDecoder().decode(CreateSessionResponse.self, from: data)
    }

    func streamTurn(
        sessionID: UInt64,
        message: String,
        onEvent: @escaping (AgentServerEvent) async -> Void
    ) async throws {
        var request = authenticatedRequest(path: "sessions/\(sessionID)/turns:stream")
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "content-type")
        request.setValue("text/event-stream", forHTTPHeaderField: "accept")
        request.httpBody = try JSONEncoder().encode(TurnRequest(message: message))

        let (data, response) = try await URLSession.shared.data(for: request)
        try validate(response: response, data: data)

        let events = try decodeEvents(from: data)
        for event in events {
            await onEvent(event)
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

    private func decodeEvents(from data: Data) throws -> [AgentServerEvent] {
        guard let text = String(data: data, encoding: .utf8), !text.isEmpty else {
            throw AgentClientError.noStreamEvents("")
        }

        let normalized = text
            .replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
        let frames = normalized.components(separatedBy: "\n\n")
        var events: [AgentServerEvent] = []

        for frame in frames {
            let dataLines = frame
                .split(separator: "\n", omittingEmptySubsequences: false)
                .compactMap { line -> String? in
                    guard line.hasPrefix("data:") else { return nil }
                    let value = line.dropFirst(5)
                    return value.first == " " ? String(value.dropFirst()) : String(value)
                }

            guard !dataLines.isEmpty else { continue }
            let eventData = Data(dataLines.joined(separator: "\n").utf8)
            events.append(try JSONDecoder().decode(AgentServerEvent.self, from: eventData))
        }

        if events.isEmpty {
            throw AgentClientError.noStreamEvents(String(normalized.prefix(1_000)))
        }
        return events
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

struct ModelsResponse: Decodable {
    let defaultModel: String
    let allowedModels: [String]

    enum CodingKeys: String, CodingKey {
        case defaultModel = "default_model"
        case allowedModels = "allowed_models"
    }
}

struct CreateSessionResponse: Decodable {
    let sessionID: UInt64
    let model: String

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case model
    }
}

struct TurnRequest: Encodable {
    let message: String
}

struct AgentServerEvent: Decodable {
    let type: String
    let sessionID: UInt64
    let status: String?
    let delta: String?
    let role: String?
    let text: String?
    let message: String?
    let toolName: String?
    let success: Bool?
    let cache: CacheHealthPayload?

    enum CodingKeys: String, CodingKey {
        case type
        case sessionID = "session_id"
        case status
        case delta
        case role
        case text
        case message
        case toolName = "tool_name"
        case success
        case cache
    }
}

struct CacheHealthPayload: Decodable {
    let usage: TokenUsagePayload?
}

struct TokenUsagePayload: Decodable {
    let inputTokens: UInt64?
    let outputTokens: UInt64?
    let totalTokens: UInt64?

    enum CodingKeys: String, CodingKey {
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case totalTokens = "total_tokens"
    }
}
