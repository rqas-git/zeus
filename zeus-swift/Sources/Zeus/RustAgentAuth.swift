import Foundation
import ZeusCore

enum RustAgentAuthState: Equatable {
    case loggedIn(String)
    case loggedOut
    case unknown(String)
}

final class RustAgentAuth: AgentAuthProtocol {
    private let lock = NSLock()
    private var loginProcess: Process?

    var authFileDisplayPath: String {
        let environment = ProcessInfo.processInfo.environment
        let path: String
        if let home = environment["RUST_AGENT_HOME"], !home.isEmpty {
            path = URL(fileURLWithPath: home)
                .appendingPathComponent("auth.json")
                .standardizedFileURL
                .path
        } else {
            path = FileManager.default.homeDirectoryForCurrentUser
                .appendingPathComponent(".rust-agent/auth.json")
                .standardizedFileURL
                .path
        }
        return PathDisplay.abbreviatingHome(in: path)
    }

    func status() async -> RustAgentAuthState {
        do {
            let output = try await runCollectingOutput(arguments: ["login", "status"])
            if output.contains("Logged in.") {
                return .loggedIn(output)
            }
            if output.contains("Not logged in.") {
                return .loggedOut
            }
            return .unknown(output)
        } catch {
            return .unknown(error.localizedDescription)
        }
    }

    func runDeviceLogin(onLine: @escaping @MainActor (String) -> Void) async throws {
        let rootURL = RustAgentLocator.rootURL()
        let process = Process()
        RustAgentLocator.configure(process, rootURL: rootURL, arguments: ["login", "--device-code"])
        process.environment = ProcessInfo.processInfo.environment

        let stdout = Pipe()
        let stderr = Pipe()
        let capture = ProcessOutputCapture()
        let emitter = ProcessLineEmitter { line in
            Task { @MainActor in
                onLine(line)
            }
        }
        process.standardOutput = stdout
        process.standardError = stderr

        stdout.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            capture.append(data)
            emitter.append(data)
        }
        stderr.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            capture.append(data)
            emitter.append(data)
        }

        try beginLogin(process)
        defer {
            stdout.fileHandleForReading.readabilityHandler = nil
            stderr.fileHandleForReading.readabilityHandler = nil
            emitter.finish()
            endLogin(process)
        }

        let status = try await runToTermination(process)
        guard status == 0 else {
            throw RustAgentAuthError.loginFailed(status, capture.snapshot())
        }
    }

    func cancelLogin() {
        lock.lock()
        let process = loginProcess
        loginProcess = nil
        lock.unlock()

        if process?.isRunning == true {
            process?.terminate()
        }
    }

    private func runCollectingOutput(arguments: [String]) async throws -> String {
        let rootURL = RustAgentLocator.rootURL()
        let process = Process()
        RustAgentLocator.configure(process, rootURL: rootURL, arguments: arguments)
        process.environment = ProcessInfo.processInfo.environment

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr

        let status = try await runToTermination(process)
        let output = stdout.fileHandleForReading.readDataToEndOfFile()
        let error = stderr.fileHandleForReading.readDataToEndOfFile()
        let text = (String(data: output, encoding: .utf8) ?? "")
            + (String(data: error, encoding: .utf8) ?? "")

        guard status == 0 else {
            throw RustAgentAuthError.commandFailed(status, text)
        }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func beginLogin(_ process: Process) throws {
        lock.lock()
        defer { lock.unlock() }

        if loginProcess?.isRunning == true {
            throw RustAgentAuthError.loginAlreadyRunning
        }
        loginProcess = process
    }

    private func endLogin(_ process: Process) {
        lock.lock()
        if loginProcess === process {
            loginProcess = nil
        }
        lock.unlock()
    }

    private func runToTermination(_ process: Process) async throws -> Int32 {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                process.terminationHandler = { completedProcess in
                    continuation.resume(returning: completedProcess.terminationStatus)
                }

                do {
                    try process.run()
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        } onCancel: {
            if process.isRunning {
                process.terminate()
            }
        }
    }
}

enum RustAgentAuthError: LocalizedError {
    case loginAlreadyRunning
    case loginFailed(Int32, String)
    case commandFailed(Int32, String)

    var errorDescription: String? {
        switch self {
        case .loginAlreadyRunning:
            return "A rust-agent login is already running."
        case let .loginFailed(status, output):
            return "rust-agent login failed with exit code \(status).\(suffix(output))"
        case let .commandFailed(status, output):
            return "rust-agent auth command failed with exit code \(status).\(suffix(output))"
        }
    }

    private func suffix(_ output: String) -> String {
        let output = output.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !output.isEmpty else { return "" }
        return "\n\(output)"
    }
}

final class ProcessLineEmitter {
    private let lock = NSLock()
    private var pending = ""
    private let onLine: (String) -> Void

    init(onLine: @escaping (String) -> Void) {
        self.onLine = onLine
    }

    func append(_ data: Data) {
        guard !data.isEmpty, let chunk = String(data: data, encoding: .utf8) else { return }

        var lines: [String] = []
        lock.lock()
        pending += chunk
        while let range = pending.range(of: "\n") {
            let line = String(pending[..<range.lowerBound])
                .trimmingCharacters(in: CharacterSet(charactersIn: "\r"))
            pending.removeSubrange(...range.lowerBound)
            lines.append(line)
        }
        lock.unlock()

        emit(lines)
    }

    func finish() {
        lock.lock()
        let line = pending.trimmingCharacters(in: .whitespacesAndNewlines)
        pending.removeAll()
        lock.unlock()

        if !line.isEmpty {
            onLine(line)
        }
    }

    private func emit(_ lines: [String]) {
        for line in lines {
            let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                onLine(trimmed)
            }
        }
    }
}
