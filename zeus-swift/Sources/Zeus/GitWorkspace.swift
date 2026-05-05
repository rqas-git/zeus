import Foundation

struct GitBranchSwitchResult {
    let previousBranch: String
    let branch: String
    let stashedChanges: Bool
}

enum GitWorkspace {
    static func currentBranch(at url: URL) throws -> String {
        let branch = try runGit(["branch", "--show-current"], at: url)
        if !branch.isEmpty {
            return branch
        }

        let commit = try runGit(["rev-parse", "--short", "HEAD"], at: url)
        return commit.isEmpty ? "detached" : "detached@\(commit)"
    }

    static func branches(at url: URL) throws -> [String] {
        let output = try runGit(
            ["for-each-ref", "--format=%(refname:short)", "refs/heads"],
            at: url
        )
        let branches = output
            .split(separator: "\n")
            .map(String.init)
            .filter { !$0.isEmpty }
        guard !branches.isEmpty else {
            throw GitWorkspaceError.noBranches
        }
        return branches
    }

    static func switchBranch(
        to branch: String,
        at url: URL,
        currentBranch previousBranch: String
    ) throws -> GitBranchSwitchResult {
        let branches = try branches(at: url)
        guard branches.contains(branch) else {
            throw GitWorkspaceError.unknownBranch(branch)
        }
        guard branch != previousBranch else {
            return GitBranchSwitchResult(
                previousBranch: previousBranch,
                branch: branch,
                stashedChanges: false
            )
        }

        let stashedChanges = try hasChanges(at: url)
        if stashedChanges {
            try runGit(
                [
                    "stash",
                    "push",
                    "--include-untracked",
                    "-m",
                    "zeus: auto-stash before switching from \(previousBranch) to \(branch)"
                ],
                at: url
            )
        }

        try runGit(["switch", branch], at: url)
        return GitBranchSwitchResult(
            previousBranch: previousBranch,
            branch: try currentBranch(at: url),
            stashedChanges: stashedChanges
        )
    }

    private static func hasChanges(at url: URL) throws -> Bool {
        try !runGit(["status", "--porcelain"], at: url).isEmpty
    }

    @discardableResult
    private static func runGit(_ arguments: [String], at url: URL) throws -> String {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
        process.arguments = ["git"] + arguments
        process.currentDirectoryURL = url

        let output = Pipe()
        let error = Pipe()
        process.standardOutput = output
        process.standardError = error

        do {
            try process.run()
            process.waitUntilExit()
        } catch {
            throw GitWorkspaceError.launchFailed(error)
        }

        let outputText = output.fileHandleForReading.readDataToEndOfFile().utf8Text
        let errorText = error.fileHandleForReading.readDataToEndOfFile().utf8Text
        guard process.terminationStatus == 0 else {
            throw GitWorkspaceError.commandFailed(
                command: (["git"] + arguments).joined(separator: " "),
                status: process.terminationStatus,
                output: [outputText, errorText]
                    .filter { !$0.isEmpty }
                    .joined(separator: "\n")
            )
        }

        return outputText.trimmingCharacters(in: .whitespacesAndNewlines)
    }
}

enum GitWorkspaceError: LocalizedError {
    case noBranches
    case unknownBranch(String)
    case launchFailed(Error)
    case commandFailed(command: String, status: Int32, output: String)

    var errorDescription: String? {
        switch self {
        case .noBranches:
            return "No local Git branches were found."
        case let .unknownBranch(branch):
            return "Git branch \(branch) is not available in this repository."
        case let .launchFailed(error):
            return "Failed to run git: \(error.localizedDescription)"
        case let .commandFailed(command, status, output):
            let details = output.isEmpty ? "" : ": \(output)"
            return "\(command) failed with status \(status)\(details)"
        }
    }
}

private extension Data {
    var utf8Text: String {
        String(data: self, encoding: .utf8)?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }
}
