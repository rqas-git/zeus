import Darwin
import Foundation
import Security
import ZeusCore

final class RustAgentServer: AgentServerProtocol {
    private let requiredProtocolVersion = 1
    private var process: Process?
    private let outputCapture = ProcessOutputCapture()
    private let readyPolls = 600
    private let readyPollNanoseconds: UInt64 = 100_000_000

    deinit {
        stop()
    }

    func start(workspaceURL: URL) async throws -> any AgentClientProtocol {
        let expectedWorkspacePath = try canonicalWorkspacePath(workspaceURL)
        let token = try Self.generateToken()
        let readiness = ServerReadinessCapture()

        do {
            try launch(
                token: token,
                workspacePath: expectedWorkspacePath,
                readiness: readiness
            )
            return try await waitForReady(
                readiness,
                expectedWorkspacePath: expectedWorkspacePath,
                expectedToken: token
            )
        } catch {
            stop()
            throw error
        }
    }

    func stop() {
        guard let process else { return }
        self.process = nil
        if process.isRunning {
            process.terminate()
            waitForExit(process, timeout: 0.75)
            if process.isRunning {
                kill(process.processIdentifier, SIGKILL)
            }
        }
    }

    private func launch(
        token: String,
        workspacePath: String,
        readiness: ServerReadinessCapture
    ) throws {
        let rustAgentRoot = RustAgentLocator.rootURL()
        let workingDirectory = RustAgentLocator.launchDirectoryURL()
        let process = Process()
        RustAgentLocator.configure(
            process,
            rootURL: rustAgentRoot,
            arguments: ["serve"],
            workingDirectoryURL: workingDirectory
        )
        process.environment = serverEnvironment(token: token, workspacePath: workspacePath)

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        capture(stdout, readiness: readiness)
        capture(stderr, readiness: readiness)

        try process.run()
        self.process = process
    }

    private func waitForReady(
        _ readiness: ServerReadinessCapture,
        expectedWorkspacePath: String,
        expectedToken: String
    ) async throws -> AgentAPIClient {
        var client: AgentAPIClient?

        for _ in 0..<readyPolls {
            if client == nil, let message = try readiness.snapshot() {
                try validateReadiness(
                    message,
                    expectedWorkspacePath: expectedWorkspacePath,
                    expectedToken: expectedToken
                )
                client = AgentAPIClient(baseURL: try message.baseURL, token: message.token)
            }

            if let client, await client.healthz() {
                try await validateCompatibility(
                    client,
                    expectedWorkspacePath: expectedWorkspacePath
                )
                return client
            }

            if let process, !process.isRunning {
                throw RustAgentServerError.exitedEarly(outputCapture.snapshot())
            }

            try await Task.sleep(nanoseconds: readyPollNanoseconds)
        }

        throw RustAgentServerError.timedOut(outputCapture.snapshot())
    }

    private func validateReadiness(
        _ readiness: ServerReadyMessage,
        expectedWorkspacePath: String,
        expectedToken: String
    ) throws {
        guard readiness.name == "rust-agent" else {
            throw RustAgentServerError.invalidIdentity(readiness.name)
        }
        guard readiness.protocolVersion == requiredProtocolVersion else {
            throw RustAgentServerError.unsupportedProtocolVersion(readiness.protocolVersion)
        }
        guard readiness.workspaceRoot == expectedWorkspacePath else {
            throw RustAgentServerError.workspaceMismatch(
                expected: expectedWorkspacePath,
                actual: readiness.workspaceRoot
            )
        }
        guard readiness.token == expectedToken else {
            throw RustAgentServerError.tokenMismatch
        }
    }

    private func validateCompatibility(
        _ client: AgentAPIClient,
        expectedWorkspacePath: String
    ) async throws {
        let identity = try await client.identity()
        guard identity.name == "rust-agent" else {
            throw RustAgentServerError.invalidIdentity(identity.name)
        }
        guard identity.workspaceRoot == expectedWorkspacePath else {
            throw RustAgentServerError.workspaceMismatch(
                expected: expectedWorkspacePath,
                actual: identity.workspaceRoot
            )
        }
        _ = try await client.models()
        _ = try await client.permissions()
    }

    private func serverEnvironment(token: String, workspacePath: String) -> [String: String] {
        var environment = ProcessInfo.processInfo.environment
        environment["RUST_AGENT_SERVER_TOKEN"] = token
        environment["RUST_AGENT_SERVER_HTTP_ADDR"] = "127.0.0.1:0"
        environment["RUST_AGENT_SERVER_H3_ADDR"] = "127.0.0.1:0"
        environment["RUST_AGENT_CACHE_HEALTH"] = "1"
        environment["RUST_AGENT_PARENT_PID"] = "\(ProcessInfo.processInfo.processIdentifier)"
        environment["RUST_AGENT_WORKSPACE"] = workspacePath
        return environment
    }

    private func canonicalWorkspacePath(_ workspaceURL: URL) throws -> String {
        let url = workspaceURL.resolvingSymlinksInPath().standardizedFileURL
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory),
              isDirectory.boolValue else {
            throw RustAgentServerError.workspaceUnavailable(url.path)
        }
        return url.path
    }

    private func waitForExit(_ process: Process, timeout: TimeInterval) {
        let deadline = Date().addingTimeInterval(timeout)
        while process.isRunning, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.05)
        }
    }

    private func capture(_ pipe: Pipe, readiness: ServerReadinessCapture) {
        let emitter = ProcessLineEmitter { line in
            readiness.observe(line)
        }
        pipe.fileHandleForReading.readabilityHandler = { [outputCapture] handle in
            let data = handle.availableData
            outputCapture.append(data)
            if data.isEmpty {
                emitter.finish()
            } else {
                emitter.append(data)
            }
        }
    }

    private static func generateToken() throws -> String {
        var bytes = [UInt8](repeating: 0, count: 32)
        let status = bytes.withUnsafeMutableBytes { buffer in
            SecRandomCopyBytes(kSecRandomDefault, buffer.count, buffer.baseAddress!)
        }
        guard status == errSecSuccess else {
            throw RustAgentServerError.tokenGenerationFailed(status)
        }
        return Data(bytes)
            .base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}

enum RustAgentServerError: LocalizedError {
    case workspaceUnavailable(String)
    case invalidIdentity(String)
    case unsupportedProtocolVersion(Int)
    case workspaceMismatch(expected: String, actual: String)
    case tokenMismatch
    case invalidReadinessAddress(String)
    case invalidReadiness(String)
    case tokenGenerationFailed(OSStatus)
    case exitedEarly(String)
    case timedOut(String)

    var errorDescription: String? {
        switch self {
        case let .workspaceUnavailable(path):
            return "Workspace does not exist or is not a directory: \(path)"
        case let .invalidIdentity(name):
            return "Expected rust-agent, but server identified as \(name)."
        case let .unsupportedProtocolVersion(version):
            return "Unsupported rust-agent protocol version \(version)."
        case let .workspaceMismatch(expected, actual):
            return "rust-agent workspace mismatch. expected \(expected), got \(actual)."
        case .tokenMismatch:
            return "rust-agent readiness token did not match the launched token."
        case let .invalidReadinessAddress(address):
            return "rust-agent readiness returned an invalid HTTP address: \(address)"
        case let .invalidReadiness(message):
            return "rust-agent returned invalid readiness metadata: \(message)"
        case let .tokenGenerationFailed(status):
            return "Failed to generate rust-agent server token: \(status)"
        case let .exitedEarly(output):
            return "rust-agent exited before the server was ready.\(suffix(output))"
        case let .timedOut(output):
            return "Timed out waiting for rust-agent to start.\(suffix(output))"
        }
    }

    private func suffix(_ output: String) -> String {
        guard !output.isEmpty else { return "" }
        return "\n\(output)"
    }
}

extension ServerReadyMessage {
    var baseURL: URL {
        get throws {
            guard let url = URL(string: "http://\(httpAddr)") else {
                throw RustAgentServerError.invalidReadinessAddress(httpAddr)
            }
            return url
        }
    }
}

private struct ServerReadyProbe: Decodable {
    let event: String?
}

private final class ServerReadinessCapture {
    private let lock = NSLock()
    private var message: ServerReadyMessage?
    private var error: Error?

    func observe(_ line: String) {
        do {
            guard let readiness = try Self.readiness(from: line) else { return }
            lock.lock()
            if message == nil, error == nil {
                message = readiness
            }
            lock.unlock()
        } catch {
            lock.lock()
            if self.error == nil {
                self.error = error
            }
            lock.unlock()
        }
    }

    func snapshot() throws -> ServerReadyMessage? {
        lock.lock()
        let message = message
        let error = error
        lock.unlock()

        if let error {
            throw error
        }
        return message
    }

    private static func readiness(from line: String) throws -> ServerReadyMessage? {
        let trimmed = line.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.first == "{" else { return nil }
        let data = Data(trimmed.utf8)
        guard let probe = try? JSONDecoder().decode(ServerReadyProbe.self, from: data) else {
            return nil
        }
        guard probe.event == "server_ready" else { return nil }
        do {
            return try JSONDecoder().decode(ServerReadyMessage.self, from: data)
        } catch {
            throw RustAgentServerError.invalidReadiness(error.localizedDescription)
        }
    }
}

final class ProcessOutputCapture {
    private let lock = NSLock()
    private var text = ""

    func append(_ data: Data) {
        guard !data.isEmpty, let chunk = String(data: data, encoding: .utf8) else { return }
        lock.lock()
        text += chunk
        if text.count > 8_000 {
            text = String(text.suffix(8_000))
        }
        lock.unlock()
    }

    func snapshot() -> String {
        lock.lock()
        let snapshot = text.trimmingCharacters(in: .whitespacesAndNewlines)
        lock.unlock()
        return snapshot
    }
}
