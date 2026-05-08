import Foundation
import ZeusCore

extension ZeusCoreChecks {
    public static func testRustAgentAPIContractFixtures() throws {
        let fixture = try APIContractFixture.load()
        try contractRequire(fixture.version == 1, "unexpected contract fixture version")

        let ready = try fixture.decodeResponse("server_ready", as: ServerReadyMessage.self)
        try contractRequire(ready.name == "rust-agent", "unexpected readiness name")
        try contractRequire(ready.protocolVersion == 1, "unexpected readiness protocol")
        try contractRequire(ready.token == "contract-token", "unexpected readiness token")

        let root = try fixture.decodeResponse("root", as: ServerIdentityResponse.self)
        try contractRequire(root.name == "rust-agent", "unexpected root identity")
        try contractRequire(root.workspaceRoot == "/workspace", "unexpected root workspace")

        let capabilities = try fixture.decodeResponse(
            "capabilities",
            as: ServerCapabilitiesResponse.self
        )
        try contractRequire(capabilities.name == "rust-agent", "unexpected capabilities identity")
        try contractRequire(capabilities.protocolVersion == 1, "unexpected capabilities protocol")
        try contractRequire(!capabilities.schemaHash.isEmpty, "missing contract schema hash")
        try contractRequire(
            capabilities.features.contains("turn_streaming"),
            "missing turn streaming feature"
        )

        let models = try fixture.decodeResponse("models", as: ModelsResponse.self)
        try contractRequire(models.defaultModel == "gpt-5.5", "unexpected default model")
        try contractRequire(models.reasoningEfforts.contains("high"), "missing reasoning effort")

        let permissions = try fixture.decodeResponse("permissions", as: PermissionsResponse.self)
        try contractRequire(
            permissions.allowedToolPolicies.contains("workspace-write"),
            "missing workspace-write permission"
        )

        let workspace = try fixture.decodeResponse("workspace", as: WorkspaceResponse.self)
        try contractRequire(workspace.branch == "main", "unexpected workspace branch")
        try contractRequire(workspace.git, "workspace should be git-backed")

        let branch = try fixture.decodeResponse(
            "switch_workspace_branch",
            as: SwitchWorkspaceBranchResponse.self
        )
        try contractRequire(branch.previousBranch == "main", "unexpected previous branch")
        try contractRequire(branch.workspace.branch == "feature", "unexpected switched branch")

        let created = try fixture.decodeResponse("create_session", as: CreateSessionResponse.self)
        try contractRequire(created.sessionID == 42, "unexpected created session id")
        try contractRequire(created.toolPolicy == "read-only", "unexpected created tool policy")

        let restored = try fixture.decodeResponse("restore_session", as: RestoreSessionResponse.self)
        try contractRequire(restored.messages.count == 4, "unexpected restored transcript count")
        try contractRequire(
            restored.messages[1].toolArguments == #"{"path":"Cargo.toml"}"#,
            "unexpected restored tool arguments"
        )

        let model = try fixture.decodeResponse("session_model", as: SessionModelResponse.self)
        try contractRequire(model.model == "gpt-5.5", "unexpected session model")

        let sessionPermissions = try fixture.decodeResponse(
            "session_permissions",
            as: SessionPermissionsResponse.self
        )
        try contractRequire(
            sessionPermissions.toolPolicy == "workspace-write",
            "unexpected session permission"
        )

        let cancel = try fixture.decodeResponse("cancel_turn", as: CancelTurnResponse.self)
        try contractRequire(cancel.cancelled, "cancel response should be true")

        let terminal = try fixture.decodeResponse("terminal_command", as: TerminalCommandResponse.self)
        try contractRequire(terminal.success, "terminal response should succeed")

        let compaction = try fixture.decodeResponse(
            "compact_session",
            as: CompactSessionResponse.self
        )
        try contractRequire(compaction.summary == "checkpoint", "unexpected compaction summary")
        try contractRequire(
            compaction.details.modifiedFiles == ["src/main.rs"],
            "unexpected compaction modified files"
        )

        let error = try fixture.decodeResponse("error", as: ContractErrorResponse.self)
        try contractRequire(error.error == "session not found", "unexpected error response")

        try contractRequire(
            Set(fixture.requestNames) == [
                "switch_workspace_branch",
                "set_session_model",
                "set_session_permissions",
                "restore_session",
                "list_sessions",
                "turn",
                "terminal_command",
                "compact_session"
            ],
            "contract request names changed"
        )
        let setModel = try fixture.decodeRequest("set_session_model", as: SetModelRequest.self)
        try contractRequire(
            setModel == SetModelRequest(model: "gpt-5.5"),
            "unexpected set model request"
        )
        let setPermissions = try fixture.decodeRequest(
            "set_session_permissions",
            as: SetPermissionsRequest.self
        )
        try contractRequire(
            setPermissions == SetPermissionsRequest(toolPolicy: "workspace-write"),
            "unexpected set permissions request"
        )
        let branchRequest = try fixture.decodeRequest(
            "switch_workspace_branch",
            as: SwitchWorkspaceBranchRequest.self
        )
        try contractRequire(
            branchRequest == SwitchWorkspaceBranchRequest(branch: "feature"),
            "unexpected branch request"
        )
        let restoreRequest = try fixture.decodeRequest(
            "restore_session",
            as: RestoreSessionRequest.self
        )
        try contractRequire(
            restoreRequest == RestoreSessionRequest(sessionID: 42),
            "unexpected restore request"
        )
        let turnRequest = try fixture.decodeRequest("turn", as: TurnRequest.self)
        try contractRequire(
            turnRequest == TurnRequest(message: "hello", reasoningEffort: "medium"),
            "unexpected turn request"
        )
        let terminalRequest = try fixture.decodeRequest(
            "terminal_command",
            as: TerminalCommandRequest.self
        )
        try contractRequire(
            terminalRequest == TerminalCommandRequest(command: "printf ok"),
            "unexpected terminal request"
        )
        let compactRequest = try fixture.decodeRequest(
            "compact_session",
            as: CompactSessionRequest.self
        )
        try contractRequire(
            compactRequest == CompactSessionRequest(
                instructions: "focus on open files",
                reasoningEffort: "medium"
            ),
            "unexpected compact session request"
        )

        let expectedEvents: [String: AgentServerEvent] = [
            "server.connected": .serverConnected(sessionID: 42),
            "server.heartbeat": .serverHeartbeat(sessionID: 42),
            "server.events_lagged": .eventsLagged(sessionID: 42, skipped: 3),
            "session.status_changed": .statusChanged(sessionID: 42, status: "running"),
            "message.text_delta": .textDelta(sessionID: 42, delta: "hello"),
            "message.completed": .messageCompleted(
                sessionID: 42,
                role: "assistant",
                text: "hello"
            ),
            "tool_call.started": .toolCallStarted(
                sessionID: 42,
                toolCallID: "call_read",
                toolName: "read_file",
                toolArguments: #"{"path":"Cargo.toml"}"#
            ),
            "tool_call.completed": .toolCallCompleted(
                sessionID: 42,
                toolCallID: "call_read",
                toolName: "read_file",
                toolArguments: nil,
                success: true
            ),
            "session.error": .error(sessionID: 42, message: "not logged in"),
            "turn.completed": .turnCompleted(sessionID: 42),
            "turn.cancelled": .turnCancelled(sessionID: 42),
            "compaction.started": .compactionStarted(sessionID: 42, reason: "manual"),
            "compaction.completed": .compactionCompleted(
                sessionID: 42,
                reason: "manual",
                summary: "checkpoint",
                firstKeptMessageID: 4,
                tokensBefore: 12345,
                details: CompactionDetails(
                    readFiles: ["Cargo.toml"],
                    modifiedFiles: ["src/main.rs"]
                )
            )
        ]

        try contractRequire(
            Set(fixture.eventNames) == Set(expectedEvents.keys).union(["cache.health"]),
            "contract event names changed"
        )

        for (name, expected) in expectedEvents {
            let event = try fixture.decodeEvent(name)
            try contractRequire(event == expected, "unexpected \(name) event")
        }

        guard case let .cacheHealth(_, cache) = try fixture.decodeEvent("cache.health") else {
            throw ContractCheckFailure.message("expected cache health event")
        }
        try contractRequire(cache?.usage?.totalTokens == 112, "unexpected cache token total")
    }
}

private struct APIContractFixture {
    let root: [String: Any]

    var version: Int {
        root["version"] as? Int ?? 0
    }

    var eventNames: [String] {
        Array(((root["events"] as? [String: Any]) ?? [:]).keys)
    }

    var requestNames: [String] {
        Array(((root["requests"] as? [String: Any]) ?? [:]).keys)
    }

    static func load() throws -> APIContractFixture {
        let url = try contractFixtureURL()
        guard FileManager.default.isReadableFile(atPath: url.path) else {
            throw ContractCheckFailure.message(
                "missing rust-agent contract fixture: \(url.path)"
            )
        }
        let data = try Data(contentsOf: url)
        let object = try JSONSerialization.jsonObject(with: data)
        guard let root = object as? [String: Any] else {
            throw ContractCheckFailure.message("contract fixture root must be an object")
        }
        return APIContractFixture(root: root)
    }

    private static func contractFixtureURL() throws -> URL {
        let environment = ProcessInfo.processInfo.environment
        if let path = environment["RUST_AGENT_CONTRACT_FIXTURE"], !path.isEmpty {
            return URL(fileURLWithPath: path)
        }

        if let root = environment["RUST_AGENT_ROOT"], !root.isEmpty {
            return URL(fileURLWithPath: root)
                .appendingPathComponent("docs/contracts/zeus-api-contract.json")
        }

        let swiftRoot = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        return swiftRoot
            .deletingLastPathComponent()
            .appendingPathComponent("rust-agent/docs/contracts/zeus-api-contract.json")
    }

    func decodeResponse<T: Decodable>(_ name: String, as type: T.Type) throws -> T {
        try JSONDecoder().decode(type, from: objectData(section: "responses", name: name))
    }

    func decodeRequest<T: Decodable>(_ name: String, as type: T.Type) throws -> T {
        try JSONDecoder().decode(type, from: objectData(section: "requests", name: name))
    }

    func decodeEvent(_ name: String) throws -> AgentServerEvent {
        try JSONDecoder().decode(AgentServerEvent.self, from: objectData(section: "events", name: name))
    }

    private func objectData(section: String, name: String) throws -> Data {
        guard let values = root[section] as? [String: Any],
              let value = values[name] else {
            throw ContractCheckFailure.message("missing \(section).\(name) fixture")
        }
        return try JSONSerialization.data(withJSONObject: value)
    }
}

private struct ContractErrorResponse: Decodable {
    let error: String
}

private enum ContractCheckFailure: LocalizedError {
    case message(String)

    var errorDescription: String? {
        switch self {
        case let .message(message):
            return message
        }
    }
}

private func contractRequire(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() {
        throw ContractCheckFailure.message(message)
    }
}
