import Foundation

public enum AgentServerEvent: Decodable, Equatable {
    case statusChanged(sessionID: UInt64?, status: String?)
    case textDelta(sessionID: UInt64?, delta: String?)
    case messageCompleted(sessionID: UInt64?, role: String?, text: String?)
    case toolCallStarted(
        sessionID: UInt64?,
        toolCallID: String?,
        toolName: String?,
        toolArguments: String?
    )
    case toolCallCompleted(
        sessionID: UInt64?,
        toolCallID: String?,
        toolName: String?,
        toolArguments: String?,
        success: Bool?
    )
    case cacheHealth(sessionID: UInt64?, cache: CacheHealthPayload?)
    case error(sessionID: UInt64?, message: String?)
    case turnCompleted(sessionID: UInt64?)
    case unknown(type: String, sessionID: UInt64?)

    public var isAssistantOutputEvent: Bool {
        switch self {
        case .textDelta:
            return true
        case let .messageCompleted(_, role, _):
            return role == "assistant"
        default:
            return false
        }
    }

    public init(from decoder: Decoder) throws {
        let payload = try AgentServerEventPayload(from: decoder)

        switch payload.type {
        case "status_changed":
            self = .statusChanged(sessionID: payload.sessionID, status: payload.status)
        case "text_delta":
            self = .textDelta(sessionID: payload.sessionID, delta: payload.delta)
        case "message_completed":
            self = .messageCompleted(
                sessionID: payload.sessionID,
                role: payload.role,
                text: payload.text
            )
        case "tool_call_started":
            self = .toolCallStarted(
                sessionID: payload.sessionID,
                toolCallID: payload.toolCallID,
                toolName: payload.toolName,
                toolArguments: payload.toolArguments
            )
        case "tool_call_completed":
            self = .toolCallCompleted(
                sessionID: payload.sessionID,
                toolCallID: payload.toolCallID,
                toolName: payload.toolName,
                toolArguments: payload.toolArguments,
                success: payload.success
            )
        case "cache_health":
            self = .cacheHealth(sessionID: payload.sessionID, cache: payload.cache)
        case "error":
            self = .error(sessionID: payload.sessionID, message: payload.message)
        case "turn_completed":
            self = .turnCompleted(sessionID: payload.sessionID)
        default:
            self = .unknown(type: payload.type, sessionID: payload.sessionID)
        }
    }
}

public struct CacheHealthPayload: Decodable, Equatable {
    public let usage: TokenUsagePayload?
}

public struct TokenUsagePayload: Decodable, Equatable {
    public let inputTokens: UInt64?
    public let outputTokens: UInt64?
    public let totalTokens: UInt64?

    enum CodingKeys: String, CodingKey {
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case totalTokens = "total_tokens"
    }
}

private struct AgentServerEventPayload: Decodable {
    let type: String
    let sessionID: UInt64?
    let status: String?
    let delta: String?
    let role: String?
    let text: String?
    let message: String?
    let toolCallID: String?
    let toolName: String?
    let toolArguments: String?
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
        case toolCallID = "tool_call_id"
        case toolName = "tool_name"
        case toolArguments = "tool_arguments"
        case success
        case cache
    }
}
