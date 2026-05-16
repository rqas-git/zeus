public struct ServerReadyMessage: Decodable {
    public let event: String
    public let name: String
    public let protocolVersion: Int
    public let httpAddr: String
    public let h3Addr: String
    public let token: String
    public let workspaceRoot: String
    public let pid: UInt32

    enum CodingKeys: String, CodingKey {
        case event
        case name
        case protocolVersion = "protocol_version"
        case httpAddr = "http_addr"
        case h3Addr = "h3_addr"
        case token
        case workspaceRoot = "workspace_root"
        case pid
    }
}

public struct ServerIdentityResponse: Decodable {
    public let name: String
    public let protocolDescription: String
    public let workspaceRoot: String

    enum CodingKeys: String, CodingKey {
        case name
        case protocolDescription = "protocol"
        case workspaceRoot = "workspace_root"
    }
}

public struct ServerCapabilitiesResponse: Decodable {
    public let name: String
    public let protocolVersion: Int
    public let schemaHash: String
    public let transports: [String]
    public let features: [String]
    public let routeGroups: [String]

    enum CodingKeys: String, CodingKey {
        case name
        case protocolVersion = "protocol_version"
        case schemaHash = "schema_hash"
        case transports
        case features
        case routeGroups = "route_groups"
    }
}

public struct ModelsResponse: Decodable {
    public let defaultModel: String
    public let allowedModels: [String]
    public let defaultReasoningEffort: String
    public let reasoningEfforts: [String]

    enum CodingKeys: String, CodingKey {
        case defaultModel = "default_model"
        case allowedModels = "allowed_models"
        case defaultReasoningEffort = "default_reasoning_effort"
        case reasoningEfforts = "reasoning_efforts"
    }
}

public struct PermissionsResponse: Decodable {
    public let defaultToolPolicy: String
    public let allowedToolPolicies: [String]

    enum CodingKeys: String, CodingKey {
        case defaultToolPolicy = "default_tool_policy"
        case allowedToolPolicies = "allowed_tool_policies"
    }
}

public struct CreateSessionResponse: Decodable {
    public let sessionID: UInt64
    public let model: String
    public let toolPolicy: String?

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case model
        case toolPolicy = "tool_policy"
    }
}

public struct RestoreSessionResponse: Decodable {
    public let sessionID: UInt64
    public let model: String
    public let toolPolicy: String
    public let messages: [TranscriptRecord]

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case model
        case toolPolicy = "tool_policy"
        case messages
    }
}

public struct TranscriptRecord: Decodable, Equatable {
    public let messageID: UInt64
    public let kind: String
    public let role: String?
    public let text: String?
    public let toolCallID: String?
    public let toolName: String?
    public let toolArguments: String?
    public let success: Bool?

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

public struct SessionModelResponse: Decodable {
    public let model: String
}

public struct SessionPermissionsResponse: Decodable {
    public let toolPolicy: String

    enum CodingKeys: String, CodingKey {
        case toolPolicy = "tool_policy"
    }
}

public struct WorkspaceResponse: Decodable {
    public let workspaceRoot: String
    public let branch: String?
    public let branches: [String]
    public let git: Bool

    enum CodingKeys: String, CodingKey {
        case workspaceRoot = "workspace_root"
        case branch
        case branches
        case git
    }
}

public struct SwitchWorkspaceBranchResponse: Decodable {
    public let previousBranch: String?
    public let branch: String
    public let stashedChanges: Bool
    public let workspace: WorkspaceResponse

    enum CodingKeys: String, CodingKey {
        case previousBranch = "previous_branch"
        case branch
        case stashedChanges = "stashed_changes"
        case workspace
    }
}

public struct PathCompletionRequest: Codable, Equatable {
    public let prefix: String
    public let kind: String
    public let limit: Int?

    public init(prefix: String, kind: String, limit: Int? = nil) {
        self.prefix = prefix
        self.kind = kind
        self.limit = limit
    }
}

public struct PathCompletionResponse: Decodable, Equatable {
    public let suggestions: [PathCompletionSuggestion]

    public init(suggestions: [PathCompletionSuggestion]) {
        self.suggestions = suggestions
    }
}

public struct PathCompletionSuggestion: Decodable, Equatable, Identifiable {
    public let value: String
    public let label: String
    public let detail: String
    public let isDirectory: Bool
    public let isExternal: Bool

    public var id: String { "\(value)|\(detail)" }

    public init(
        value: String,
        label: String,
        detail: String,
        isDirectory: Bool,
        isExternal: Bool
    ) {
        self.value = value
        self.label = label
        self.detail = detail
        self.isDirectory = isDirectory
        self.isExternal = isExternal
    }

    enum CodingKeys: String, CodingKey {
        case value
        case label
        case detail
        case isDirectory = "is_directory"
        case isExternal = "is_external"
    }
}

public struct TerminalCommandResponse: Decodable {
    public let output: String
    public let success: Bool
}

public struct CancelTurnResponse: Decodable {
    public let cancelled: Bool
}

public struct CompactionDetails: Decodable, Equatable {
    public let readFiles: [String]
    public let modifiedFiles: [String]

    public init(readFiles: [String], modifiedFiles: [String]) {
        self.readFiles = readFiles
        self.modifiedFiles = modifiedFiles
    }

    enum CodingKeys: String, CodingKey {
        case readFiles = "read_files"
        case modifiedFiles = "modified_files"
    }
}

public struct CompactSessionResponse: Decodable, Equatable {
    public let summary: String
    public let firstKeptMessageID: UInt64
    public let tokensBefore: UInt64
    public let details: CompactionDetails

    enum CodingKeys: String, CodingKey {
        case summary
        case firstKeptMessageID = "first_kept_message_id"
        case tokensBefore = "tokens_before"
        case details
    }
}

public struct SetModelRequest: Codable, Equatable {
    public let model: String

    public init(model: String) {
        self.model = model
    }
}

public struct SetPermissionsRequest: Codable, Equatable {
    public let toolPolicy: String

    public init(toolPolicy: String) {
        self.toolPolicy = toolPolicy
    }

    enum CodingKeys: String, CodingKey {
        case toolPolicy = "tool_policy"
    }
}

public struct SwitchWorkspaceBranchRequest: Codable, Equatable {
    public let branch: String

    public init(branch: String) {
        self.branch = branch
    }
}

public struct RestoreSessionRequest: Codable, Equatable {
    public let sessionID: UInt64

    public init(sessionID: UInt64) {
        self.sessionID = sessionID
    }

    enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
    }
}

public struct TurnRequest: Codable, Equatable {
    public let message: String
    public let reasoningEffort: String

    public init(message: String, reasoningEffort: String) {
        self.message = message
        self.reasoningEffort = reasoningEffort
    }

    enum CodingKeys: String, CodingKey {
        case message
        case reasoningEffort = "reasoning_effort"
    }
}

public struct TerminalCommandRequest: Codable, Equatable {
    public let command: String

    public init(command: String) {
        self.command = command
    }
}

public struct CompactSessionRequest: Codable, Equatable {
    public let instructions: String
    public let reasoningEffort: String

    public init(instructions: String, reasoningEffort: String) {
        self.instructions = instructions
        self.reasoningEffort = reasoningEffort
    }

    enum CodingKeys: String, CodingKey {
        case instructions
        case reasoningEffort = "reasoning_effort"
    }
}
