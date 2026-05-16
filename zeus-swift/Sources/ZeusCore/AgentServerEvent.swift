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
    case turnTokenUsage(sessionID: UInt64?, usage: TokenUsagePayload?)
    case compactionStarted(sessionID: UInt64?, reason: String?)
    case compactionCompleted(
        sessionID: UInt64?,
        reason: String?,
        summary: String?,
        firstKeptMessageID: UInt64?,
        tokensBefore: UInt64?,
        details: CompactionDetails?
    )
    case error(sessionID: UInt64?, message: String?)
    case turnCompleted(sessionID: UInt64?, durationMS: UInt64)
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
             let .turnTokenUsage(sessionID, _),
             let .compactionStarted(sessionID, _),
             let .compactionCompleted(sessionID, _, _, _, _, _),
             let .error(sessionID, _),
             let .turnCompleted(sessionID, _),
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
        case "turn_token_usage":
            self = .turnTokenUsage(sessionID: payload.sessionID, usage: payload.usage)
        case "compaction_started":
            self = .compactionStarted(sessionID: payload.sessionID, reason: payload.reason)
        case "compaction_completed":
            self = .compactionCompleted(
                sessionID: payload.sessionID,
                reason: payload.reason,
                summary: payload.summary,
                firstKeptMessageID: payload.firstKeptMessageID,
                tokensBefore: payload.tokensBefore,
                details: payload.details
            )
        case "error":
            self = .error(sessionID: payload.sessionID, message: payload.message)
        case "turn_completed":
            self = .turnCompleted(
                sessionID: payload.sessionID,
                durationMS: try payload.requiredDurationMS()
            )
        case "turn_cancelled":
            self = .turnCancelled(sessionID: payload.sessionID)
        default:
            self = .unknown(type: payload.type, sessionID: payload.sessionID)
        }
    }
}

public struct CacheHealthPayload: Decodable, Equatable {
    public let model: String?
    public let responseID: String?
    public let cacheStatus: String?
    public let usage: TokenUsagePayload?

    enum CodingKeys: String, CodingKey {
        case model
        case responseID = "response_id"
        case cacheStatus = "cache_status"
        case usage
    }
}

public struct TokenUsagePayload: Decodable, Equatable {
    public let inputTokens: UInt64?
    public let cachedInputTokens: UInt64?
    public let outputTokens: UInt64?
    public let reasoningOutputTokens: UInt64?
    public let totalTokens: UInt64?

    enum CodingKeys: String, CodingKey {
        case inputTokens = "input_tokens"
        case cachedInputTokens = "cached_input_tokens"
        case outputTokens = "output_tokens"
        case reasoningOutputTokens = "reasoning_output_tokens"
        case totalTokens = "total_tokens"
    }
}

public struct ResponseCacheStats: Equatable {
    public let model: String?
    public let responseID: String?
    public let cacheStatus: String?
    public let inputTokens: UInt64?
    public let cachedInputTokens: UInt64?
    public let outputTokens: UInt64?
    public let reasoningOutputTokens: UInt64?
    public let totalTokens: UInt64?

    public init(cache: CacheHealthPayload) {
        model = cache.model
        responseID = cache.responseID
        cacheStatus = cache.cacheStatus
        inputTokens = cache.usage?.inputTokens
        cachedInputTokens = cache.usage?.cachedInputTokens
        outputTokens = cache.usage?.outputTokens
        reasoningOutputTokens = cache.usage?.reasoningOutputTokens
        totalTokens = cache.usage?.totalTokens
    }

    public var displayText: String {
        var parts: [String] = ["tokens"]
        append("input", inputTokens, to: &parts)
        append("cached", cachedInputTokens, to: &parts)
        if let cacheHit = cacheHitRatioText {
            parts.append("hit=\(cacheHit)")
        }
        append("output", outputTokens, to: &parts)
        append("reasoning", reasoningOutputTokens, to: &parts)
        append("total", totalTokens, to: &parts)
        append("cache", cacheStatus, to: &parts)
        append("model", model, to: &parts)
        append("response", responseID, to: &parts)
        return parts.joined(separator: " ")
    }

    private var cacheHitRatioText: String? {
        guard let inputTokens, inputTokens > 0, let cachedInputTokens else { return nil }
        let ratio = Double(cachedInputTokens) / Double(inputTokens) * 100
        return String(format: "%.0f%%", ratio)
    }

    private func append(_ label: String, _ value: UInt64?, to parts: inout [String]) {
        guard let value else { return }
        parts.append("\(label)=\(value)")
    }

    private func append(_ label: String, _ value: String?, to parts: inout [String]) {
        guard let value, !value.isEmpty else { return }
        parts.append("\(label)=\(value)")
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
    let usage: TokenUsagePayload?
    let reason: String?
    let summary: String?
    let firstKeptMessageID: UInt64?
    let tokensBefore: UInt64?
    let details: CompactionDetails?
    let durationMS: UInt64?

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
        case usage
        case reason
        case summary
        case firstKeptMessageID = "first_kept_message_id"
        case tokensBefore = "tokens_before"
        case details
        case durationMS = "duration_ms"
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
        usage = try container.decodeIfPresent(TokenUsagePayload.self, forKey: .usage)
        reason = try container.decodeIfPresent(String.self, forKey: .reason)
        summary = try container.decodeIfPresent(String.self, forKey: .summary)
        firstKeptMessageID = try container.decodeIfPresent(UInt64.self, forKey: .firstKeptMessageID)
        tokensBefore = try container.decodeIfPresent(UInt64.self, forKey: .tokensBefore)
        details = try container.decodeIfPresent(CompactionDetails.self, forKey: .details)
        durationMS = try container.decodeIfPresent(UInt64.self, forKey: .durationMS)
    }

    func requiredDurationMS() throws -> UInt64 {
        guard let durationMS else {
            throw DecodingError.keyNotFound(
                CodingKeys.durationMS,
                DecodingError.Context(
                    codingPath: [],
                    debugDescription: "turn_completed event requires duration_ms"
                )
            )
        }
        return durationMS
    }
}
