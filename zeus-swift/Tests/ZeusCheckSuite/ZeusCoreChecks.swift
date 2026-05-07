import Foundation
import ZeusCore

public enum ZeusCoreChecks {
    public static func testCommonMarkdownBlocks() throws {
        let blocks = TerminalMarkdownParser.parse(
            """
            # Title

            Paragraph line one
            paragraph line two

            - one
            - two

            1. first
            2. second

            > quote

            ```swift
            let value = 1
            ```

            ---
            """
        )

        try require(blocks.count == 7, "expected 7 markdown blocks, got \(blocks.count)")

        guard case let .heading(level, text) = blocks[0] else {
            throw CheckFailure.message("expected heading block")
        }
        try require(level == 1, "expected heading level 1")
        try require(text == "Title", "expected heading text")

        guard case let .paragraph(paragraph) = blocks[1] else {
            throw CheckFailure.message("expected paragraph block")
        }
        try require(paragraph == "Paragraph line one\nparagraph line two", "unexpected paragraph text")

        guard case let .unorderedList(unorderedItems) = blocks[2] else {
            throw CheckFailure.message("expected unordered list block")
        }
        try require(unorderedItems == ["one", "two"], "unexpected unordered list")

        guard case let .orderedList(orderedItems) = blocks[3] else {
            throw CheckFailure.message("expected ordered list block")
        }
        try require(orderedItems.map(\.number) == [1, 2], "unexpected ordered numbers")
        try require(orderedItems.map(\.text) == ["first", "second"], "unexpected ordered text")

        guard case let .quote(quoteLines) = blocks[4] else {
            throw CheckFailure.message("expected quote block")
        }
        try require(quoteLines == ["quote"], "unexpected quote")

        guard case let .codeBlock(language, code) = blocks[5] else {
            throw CheckFailure.message("expected code block")
        }
        try require(language == "swift", "unexpected code language")
        try require(code == "let value = 1", "unexpected code body")

        guard case .rule = blocks[6] else {
            throw CheckFailure.message("expected rule block")
        }
    }

    public static func testParagraphBoundaries() throws {
        let blocks = TerminalMarkdownParser.parse(
            """
            alpha
            beta
            ## Next
            """
        )

        try require(blocks.count == 2, "expected 2 markdown blocks, got \(blocks.count)")

        guard case let .paragraph(text) = blocks[0] else {
            throw CheckFailure.message("expected paragraph block")
        }
        try require(text == "alpha\nbeta", "unexpected paragraph text")

        guard case let .heading(level, text) = blocks[1] else {
            throw CheckFailure.message("expected heading block")
        }
        try require(level == 2, "expected heading level 2")
        try require(text == "Next", "expected heading text")
    }

    public static func testToolMetadata() throws {
        let metadata = ToolMetadata.forName("git_commit")

        try require(metadata.name == "git_commit", "unexpected tool name")
        try require(metadata.displayName == "git_commit", "unexpected display name")
        try require(metadata.action == "committing", "unexpected action")
        try require(metadata.iconName == "arrow.trianglehead.branch", "unexpected icon")
    }

    public static func testToolDisplayNames() throws {
        let displayNames = [
            "read_file": "read",
            "read_file_range": "read",
            "list_dir": "list",
            "search_files": "find",
            "search_text": "find",
            "apply_patch": "edit",
            "exec_command": "bash"
        ]

        for (tool, displayName) in displayNames {
            try require(
                ToolMetadata.forName(tool).displayName == displayName,
                "unexpected display name for \(tool)"
            )
        }
    }

    public static func testToolTargets() throws {
        let read = ToolMetadata.forName("read_file")
        try require(
            read.target(fromArgumentsJSON: #"{"path":"Sources/Zeus/ChatWindow.swift"}"#)
                == "Sources/Zeus/ChatWindow.swift",
            "unexpected read_file target"
        )

        let list = ToolMetadata.forName("list_dir")
        try require(
            list.target(fromArgumentsJSON: #"{"path":"Sources"}"#) == "Sources",
            "unexpected list_dir target"
        )

        let exec = ToolMetadata.forName("exec_command")
        try require(
            exec.target(fromArgumentsJSON: #"{"command":"swift test"}"#) == #""swift test""#,
            "unexpected exec target"
        )

        let add = ToolMetadata.forName("git_add")
        try require(
            add.target(fromArgumentsJSON: #"{"paths":["a.swift","b.swift"]}"#) == "a.swift +1",
            "unexpected paths summary"
        )

        let patch = ToolMetadata.forName("apply_patch")
        try require(
            patch.target(
                fromArgumentsJSON: #"{"patch":"*** Begin Patch\n*** Update File: Sources/App.swift\n*** End Patch"}"#
            ) == "Sources/App.swift",
            "unexpected patch summary"
        )
    }

    public static func testAgentServerEvents() throws {
        let textDelta = try decodeEvent(
            #"{"type":"text_delta","session_id":42,"delta":"hello"}"#
        )
        try require(textDelta == .textDelta(sessionID: 42, delta: "hello"), "unexpected text delta")
        try require(textDelta.isAssistantOutputEvent, "text delta should be assistant output")
        try require(textDelta.sessionID == 42, "text delta session id should be available")

        let connected = try decodeEvent(
            #"{"type":"server_connected","session_id":42}"#
        )
        try require(
            connected == .serverConnected(sessionID: 42),
            "unexpected server connected event"
        )

        let lagged = try decodeEvent(
            #"{"type":"events_lagged","session_id":42,"skipped":3}"#
        )
        try require(
            lagged == .eventsLagged(sessionID: 42, skipped: 3),
            "unexpected lagged event"
        )

        let toolStarted = try decodeEvent(
            #"{"type":"tool_call_started","session_id":7,"tool_call_id":"call_read","tool_name":"read_file","args":"{\"path\":\"Cargo.toml\"}"}"#
        )
        try require(
            toolStarted == .toolCallStarted(
                sessionID: 7,
                toolCallID: "call_read",
                toolName: "read_file",
                toolArguments: #"{"path":"Cargo.toml"}"#
            ),
            "unexpected tool call started event"
        )

        let unknown = try decodeEvent(
            #"{"type":"new_event","session_id":7,"message":"later"}"#
        )
        try require(unknown == .unknown(type: "new_event", sessionID: 7), "unexpected unknown event")

        let turnCancelled = try decodeEvent(
            #"{"type":"turn_cancelled","session_id":7}"#
        )
        try require(
            turnCancelled == .turnCancelled(sessionID: 7),
            "unexpected turn cancelled event"
        )

        let cacheHealth = try decodeEvent(
            #"{"type":"cache_health","session_id":1,"cache":{"usage":{"input_tokens":2,"output_tokens":3,"total_tokens":5}}}"#
        )
        guard case let .cacheHealth(_, cache) = cacheHealth else {
            throw CheckFailure.message("expected cache health event")
        }
        try require(cache?.usage?.inputTokens == 2, "unexpected input tokens")
        try require(cache?.usage?.outputTokens == 3, "unexpected output tokens")
        try require(cache?.usage?.totalTokens == 5, "unexpected total tokens")
    }

    public static func testPathDisplay() throws {
        try require(
            PathDisplay.abbreviatingHome(
                in: "/Users/example/project",
                homeDirectory: "/Users/example"
            ) == "~/project",
            "expected home path abbreviation"
        )
        try require(
            PathDisplay.abbreviatingHome(
                in: "/Volumes/work/project",
                homeDirectory: "/Users/example"
            ) == "/Volumes/work/project",
            "expected external path to remain unchanged"
        )
        try require(
            PathDisplay.abbreviatingHome(
                in: "/Users/example-other/project",
                homeDirectory: "/Users/example"
            ) == "/Users/example-other/project",
            "expected sibling path to remain unchanged"
        )
    }

    public static func testPromptHistoryNavigation() throws {
        var history = PromptHistory()
        try require(history.previous(currentDraft: "draft") == nil, "empty history should not navigate up")
        try require(history.next() == nil, "empty history should not navigate down")

        history.record("first")
        history.record("second")

        try require(
            history.previous(currentDraft: "draft") == "second",
            "up should recall newest entry"
        )
        try require(
            history.previous(currentDraft: "ignored") == "first",
            "second up should recall older entry"
        )
        try require(
            history.previous(currentDraft: "ignored") == "first",
            "up at oldest entry should remain there"
        )
        try require(
            history.next() == "second",
            "down should move toward newer entries"
        )
        try require(
            history.next() == "draft",
            "down after newest entry should restore original draft"
        )
        try require(
            history.next() == nil,
            "down after leaving history should not handle the key"
        )
    }

    public static func testPromptHistoryResetAndReplace() throws {
        var history = PromptHistory(entries: ["old"])
        try require(history.previous(currentDraft: "draft") == "old", "expected initial entry")

        history.record("new")
        try require(history.next() == nil, "record should reset active navigation")
        try require(history.previous(currentDraft: "draft") == "new", "record should append new entry")

        history.replace(with: ["restored"])
        try require(history.next() == nil, "replace should reset active navigation")
        try require(
            history.previous(currentDraft: "draft") == "restored",
            "replace should use restored entries"
        )
    }

    private static func decodeEvent(_ json: String) throws -> AgentServerEvent {
        try JSONDecoder().decode(AgentServerEvent.self, from: Data(json.utf8))
    }
}

private enum CheckFailure: LocalizedError {
    case message(String)

    var errorDescription: String? {
        switch self {
        case let .message(message):
            return message
        }
    }
}

private func require(_ condition: @autoclosure () -> Bool, _ message: String) throws {
    if !condition() {
        throw CheckFailure.message(message)
    }
}
