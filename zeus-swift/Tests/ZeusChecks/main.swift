import Foundation
import ZeusCore

@main
struct ZeusChecks {
    static func main() {
        var checks = CheckRunner()
        checks.run("TerminalMarkdownParser parses common blocks", testCommonMarkdownBlocks)
        checks.run("TerminalMarkdownParser stops paragraphs at block starts", testParagraphBoundaries)
        checks.run("ToolMetadata maps known tools", testToolMetadata)
        checks.run("ToolMetadata summarizes arguments", testToolTargets)
        checks.finish()
    }
}

private struct CheckRunner {
    private var failures: [String] = []

    mutating func run(_ name: String, _ body: () throws -> Void) {
        do {
            try body()
            print("PASS \(name)")
        } catch {
            failures.append("\(name): \(error.localizedDescription)")
            print("FAIL \(name): \(error.localizedDescription)")
        }
    }

    func finish() -> Never {
        if failures.isEmpty {
            print("All checks passed.")
            exit(0)
        }

        for failure in failures {
            fputs("\(failure)\n", stderr)
        }
        exit(1)
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

private func testCommonMarkdownBlocks() throws {
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

private func testParagraphBoundaries() throws {
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

private func testToolMetadata() throws {
    let metadata = ToolMetadata.forName("git_commit")

    try require(metadata.name == "git_commit", "unexpected tool name")
    try require(metadata.action == "committing", "unexpected action")
    try require(metadata.iconName == "arrow.trianglehead.branch", "unexpected icon")
}

private func testToolTargets() throws {
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
