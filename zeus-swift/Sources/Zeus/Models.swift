import Foundation
import ZeusCore

enum TranscriptKind: Equatable {
    case user
    case assistant
    case status
    case tool
    case error
}

struct RenderedTerminalMarkdown: Equatable, Sendable {
    let blocks: [RenderedTerminalMarkdownBlock]

    init(text: String) {
        self.init(blocks: TerminalMarkdownParser.parse(text))
    }

    init(blocks: [TerminalMarkdownBlock]) {
        self.blocks = blocks.map(Self.render)
    }

    private static func render(_ block: TerminalMarkdownBlock) -> RenderedTerminalMarkdownBlock {
        switch block {
        case let .paragraph(text):
            return .paragraph(renderInline(text))
        case let .heading(level, text):
            return .heading(level: level, text: renderInline(text))
        case let .unorderedList(items):
            return .unorderedList(items.map(renderInline))
        case let .orderedList(items):
            return .orderedList(items.map { item in
                RenderedOrderedListItem(number: item.number, text: renderInline(item.text))
            })
        case let .quote(lines):
            return .quote(lines.map(renderInline))
        case let .codeBlock(language, code):
            return .codeBlock(language: language, code: code)
        case .rule:
            return .rule
        }
    }

    private static func renderInline(_ source: String) -> RenderedInlineText {
        let options = AttributedString.MarkdownParsingOptions(
            interpretedSyntax: .inlineOnlyPreservingWhitespace,
            failurePolicy: .returnPartiallyParsedIfPossible
        )
        if let attributed = try? AttributedString(markdown: source, options: options) {
            return .attributed(attributed)
        }
        return .plain(source)
    }
}

enum RenderedTerminalMarkdownBlock: Equatable, Sendable {
    case paragraph(RenderedInlineText)
    case heading(level: Int, text: RenderedInlineText)
    case unorderedList([RenderedInlineText])
    case orderedList([RenderedOrderedListItem])
    case quote([RenderedInlineText])
    case codeBlock(language: String?, code: String)
    case rule
}

struct RenderedOrderedListItem: Equatable, Sendable {
    let number: Int
    let text: RenderedInlineText
}

enum RenderedInlineText: Equatable, Sendable {
    case plain(String)
    case attributed(AttributedString)
}

struct TranscriptLine: Identifiable, Equatable {
    let id: UUID
    var kind: TranscriptKind
    var text: String
    var toolCall: ToolCallTranscript?
    var cacheStats: [ResponseCacheStats]
    var isStreaming: Bool
    var renderedMarkdown: RenderedTerminalMarkdown?
    var responseDurationMS: UInt64?

    init(
        id: UUID = UUID(),
        kind: TranscriptKind,
        text: String,
        toolCall: ToolCallTranscript? = nil,
        cacheStats: [ResponseCacheStats] = [],
        isStreaming: Bool = false,
        renderedMarkdown: RenderedTerminalMarkdown? = nil,
        responseDurationMS: UInt64? = nil
    ) {
        self.id = id
        self.kind = kind
        self.text = text
        self.toolCall = toolCall
        self.cacheStats = cacheStats
        self.isStreaming = isStreaming
        self.renderedMarkdown = renderedMarkdown
        self.responseDurationMS = responseDurationMS
    }

    static func == (lhs: TranscriptLine, rhs: TranscriptLine) -> Bool {
        lhs.id == rhs.id
            && lhs.kind == rhs.kind
            && lhs.text == rhs.text
            && lhs.toolCall == rhs.toolCall
            && lhs.cacheStats == rhs.cacheStats
            && lhs.isStreaming == rhs.isStreaming
            && lhs.renderedMarkdown == rhs.renderedMarkdown
            && lhs.responseDurationMS == rhs.responseDurationMS
    }
}

struct TranscriptScrollTarget: Equatable {
    let lineID: UUID
    let revision: Int
}

struct PromptPathCompletionState: Equatable {
    var context: PromptPathCompletionContext
    var suggestions: [PathCompletionSuggestion]
    var selectedIndex: Int

    var selectedSuggestion: PathCompletionSuggestion? {
        guard suggestions.indices.contains(selectedIndex) else { return nil }
        return suggestions[selectedIndex]
    }
}

struct ActiveAssistantStream: Equatable {
    let lineID: UUID
    let chunks: [StreamingTextChunk]
    let tail: String

    var text: String {
        let completedLines = chunks.map { "\($0.text)\n" }.joined()
        return completedLines + tail
    }
}

struct StreamingTextChunk: Identifiable, Equatable {
    let id: Int
    let text: String
}

struct ToolCallTranscript: Equatable {
    var name: String
    var action: String
    var iconName: String
    var target: String?
    var status: ToolCallStatus
}

enum ToolCallStatus: Equatable {
    case running
    case completed
    case failed
}

struct WorkspaceMetadata {
    let name: String
    let branch: String
    let displayPath: String
    let url: URL
    let isGit: Bool

    static func current() -> WorkspaceMetadata {
        let environment = ProcessInfo.processInfo.environment
        let url = workspaceURL(environment: environment)
        return current(at: url)
    }

    static func current(at url: URL) -> WorkspaceMetadata {
        return WorkspaceMetadata(
            name: url.lastPathComponent,
            branch: "...",
            displayPath: PathDisplay.abbreviatingHome(in: url.path),
            url: url,
            isGit: false
        )
    }

    func applying(_ workspace: WorkspaceResponse) -> WorkspaceMetadata {
        let branch = workspace.branch ?? (workspace.git ? "detached" : "no git")
        return WorkspaceMetadata(
            name: name,
            branch: branch.isEmpty ? "detached" : branch,
            displayPath: PathDisplay.abbreviatingHome(in: workspace.workspaceRoot),
            url: URL(fileURLWithPath: workspace.workspaceRoot).standardizedFileURL,
            isGit: workspace.git
        )
    }

    private static func workspaceURL(environment: [String: String]) -> URL {
        if let configured = environment["ZEUS_WORKSPACE"], !configured.isEmpty {
            return URL(fileURLWithPath: configured).standardizedFileURL
        }

        let current = RustAgentLocator.launchDirectoryURL()
        if isSwiftPackageRoot(current) {
            return current
        }

        if let executable = Bundle.main.executableURL?.standardizedFileURL {
            var directory = executable.deletingLastPathComponent()
            for _ in 0..<8 {
                if isSwiftPackageRoot(directory) {
                    return directory
                }
                directory.deleteLastPathComponent()
            }
        }

        return current
    }

    private static func isSwiftPackageRoot(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.appendingPathComponent("Package.swift").path)
    }
}
