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
