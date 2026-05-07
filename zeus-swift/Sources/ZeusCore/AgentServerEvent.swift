import Foundation

public enum AgentServerEvent: Decodable, Equatable {
    case serverConnected(sessionID: UInt64?)
    case serverHeartbeat(sessionID: UInt64?)
    case eventsLagged(sessionID: UInt64?, skipped: UInt64?)
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
    case turnCancelled(sessionID: UInt64?)
    case unknown(type: String, sessionID: UInt64?)

    public var sessionID: UInt64? {
        switch self {
        case let .serverConnected(sessionID),
             let .serverHeartbeat(sessionID),
             let .statusChanged(sessionID, _),
             let .textDelta(sessionID, _),
             let .messageCompleted(sessionID, _, _),
             let .toolCallStarted(sessionID, _, _, _),
             let .toolCallCompleted(sessionID, _, _, _, _),
             let .cacheHealth(sessionID, _),
             let .error(sessionID, _),
             let .turnCompleted(sessionID),
             let .turnCancelled(sessionID),
             let .unknown(_, sessionID):
            return sessionID
        case let .eventsLagged(sessionID, _):
            return sessionID
        }
    }

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
        case "server_connected":
            self = .serverConnected(sessionID: payload.sessionID)
        case "server_heartbeat":
            self = .serverHeartbeat(sessionID: payload.sessionID)
        case "events_lagged":
            self = .eventsLagged(sessionID: payload.sessionID, skipped: payload.skipped)
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
        case "turn_cancelled":
            self = .turnCancelled(sessionID: payload.sessionID)
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
    let skipped: UInt64?
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
        case args
        case success
        case skipped
        case cache
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        type = try container.decode(String.self, forKey: .type)
        sessionID = try container.decodeIfPresent(UInt64.self, forKey: .sessionID)
        status = try container.decodeIfPresent(String.self, forKey: .status)
        delta = try container.decodeIfPresent(String.self, forKey: .delta)
        role = try container.decodeIfPresent(String.self, forKey: .role)
        text = try container.decodeIfPresent(String.self, forKey: .text)
        message = try container.decodeIfPresent(String.self, forKey: .message)
        toolCallID = try container.decodeIfPresent(String.self, forKey: .toolCallID)
        toolName = try container.decodeIfPresent(String.self, forKey: .toolName)
        toolArguments = try container.decodeIfPresent(String.self, forKey: .toolArguments)
            ?? container.decodeIfPresent(String.self, forKey: .args)
        success = try container.decodeIfPresent(Bool.self, forKey: .success)
        skipped = try container.decodeIfPresent(UInt64.self, forKey: .skipped)
        cache = try container.decodeIfPresent(CacheHealthPayload.self, forKey: .cache)
    }
}
