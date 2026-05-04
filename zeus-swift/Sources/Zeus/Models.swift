import Foundation

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

    init(
        id: UUID = UUID(),
        kind: TranscriptKind,
        text: String,
        toolCall: ToolCallTranscript? = nil
    ) {
        self.id = id
        self.kind = kind
        self.text = text
        self.toolCall = toolCall
    }
}

struct ToolCallTranscript: Equatable {
    var name: String
    var action: String
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

    static func current() -> WorkspaceMetadata {
        let environment = ProcessInfo.processInfo.environment
        let url = workspaceURL(environment: environment)
        let branch = runGit(["branch", "--show-current"], at: url) ?? "main"

        return WorkspaceMetadata(
            name: url.lastPathComponent,
            branch: branch.isEmpty ? "main" : branch,
            displayPath: abbreviateHome(in: url.path)
        )
    }

    private static func workspaceURL(environment: [String: String]) -> URL {
        if let configured = environment["ZEUS_WORKSPACE"], !configured.isEmpty {
            return URL(fileURLWithPath: configured).standardizedFileURL
        }

        let current = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .standardizedFileURL
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

    private static func abbreviateHome(in path: String) -> String {
        let home = FileManager.default.homeDirectoryForCurrentUser.path
        guard path.hasPrefix(home) else { return path }
        return "~" + path.dropFirst(home.count)
    }

    private static func runGit(_ arguments: [String], at url: URL) -> String? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["git"] + arguments
        process.currentDirectoryURL = url

        let output = Pipe()
        process.standardOutput = output
        process.standardError = Pipe()

        do {
            try process.run()
            process.waitUntilExit()
        } catch {
            return nil
        }

        guard process.terminationStatus == 0 else { return nil }
        let data = output.fileHandleForReading.readDataToEndOfFile()
        return String(data: data, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
