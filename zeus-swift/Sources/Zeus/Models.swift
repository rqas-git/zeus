import Foundation
import ZeusCore

enum TranscriptKind: Equatable {
    case user
    case assistant
    case status
    case tool
    case error
}

struct TranscriptLine: Identifiable, Equatable {
    let id: UUID
    var kind: TranscriptKind
    var text: String
    var toolCall: ToolCallTranscript?
    var cacheStats: [ResponseCacheStats]
    var isStreaming: Bool
    var markdownBlocks: [TerminalMarkdownBlock]?

    init(
        id: UUID = UUID(),
        kind: TranscriptKind,
        text: String,
        toolCall: ToolCallTranscript? = nil,
        cacheStats: [ResponseCacheStats] = [],
        isStreaming: Bool = false,
        markdownBlocks: [TerminalMarkdownBlock]? = nil
    ) {
        self.id = id
        self.kind = kind
        self.text = text
        self.toolCall = toolCall
        self.cacheStats = cacheStats
        self.isStreaming = isStreaming
        self.markdownBlocks = markdownBlocks
    }

    static func == (lhs: TranscriptLine, rhs: TranscriptLine) -> Bool {
        lhs.id == rhs.id
            && lhs.kind == rhs.kind
            && lhs.text == rhs.text
            && lhs.toolCall == rhs.toolCall
            && lhs.cacheStats == rhs.cacheStats
            && lhs.isStreaming == rhs.isStreaming
    }
}

struct TranscriptScrollTarget: Equatable {
    let lineID: UUID
    let revision: Int
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
