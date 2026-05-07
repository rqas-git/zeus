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

public struct TerminalCommandResponse: Decodable {
    public let output: String
    public let success: Bool
}

public struct CancelTurnResponse: Decodable {
    public let cancelled: Bool
}
