import Darwin
import Foundation

final class RustAgentServer: AgentServerProtocol {
    let token = "zeus-swift-dev-token"

    private let candidates = (0..<20).map {
        ServerCandidate(httpPort: 4196 + $0, h3Port: 4533 + $0)
    }
    private var process: Process?
    private let outputCapture = ProcessOutputCapture()
    private let readyPolls = 600
    private let readyPollNanoseconds: UInt64 = 100_000_000

    deinit {
        stop()
    }

    func start(workspaceURL: URL) async throws -> any AgentClientProtocol {
        let expectedWorkspacePath = try canonicalWorkspacePath(workspaceURL)
        var failures: [String] = []
        var reusableClient: AgentAPIClient?

        for candidate in candidates {
            let client = AgentAPIClient(baseURL: candidate.baseURL, token: token)
            if await client.healthz() {
                do {
                    try await validateCompatibility(
                        client,
                        expectedWorkspacePath: expectedWorkspacePath
                    )
                    reusableClient = reusableClient ?? client
                    failures.append("\(candidate.httpAddress) already has a compatible server; trying to start a fresh server first")
                } catch {
                    failures.append("\(candidate.httpAddress) is occupied by an incompatible server: \(error.localizedDescription)")
                }
                continue
            }

            do {
                try launch(candidate, workspacePath: expectedWorkspacePath)
                try await waitForReady(client, expectedWorkspacePath: expectedWorkspacePath)
                return client
            } catch {
                stop()
                failures.append("\(candidate.httpAddress): \(error.localizedDescription)")
            }
        }

        if let reusableClient {
            return reusableClient
        }

        throw RustAgentServerError.noAvailableServer(failures)
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

    private func launch(_ candidate: ServerCandidate, workspacePath: String) throws {
        let rustAgentRoot = RustAgentLocator.rootURL()
        let workingDirectory = RustAgentLocator.launchDirectoryURL()
        let process = Process()
        RustAgentLocator.configure(
            process,
            rootURL: rustAgentRoot,
            arguments: ["serve"],
            workingDirectoryURL: workingDirectory
        )
        process.environment = serverEnvironment(candidate, workspacePath: workspacePath)

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        capture(stdout)
        capture(stderr)

        try process.run()
        self.process = process
    }

    private func waitForReady(
        _ client: AgentAPIClient,
        expectedWorkspacePath: String
    ) async throws {
        for _ in 0..<readyPolls {
            if await client.healthz() {
                try await validateCompatibility(
                    client,
                    expectedWorkspacePath: expectedWorkspacePath
                )
                return
            }

            if let process, !process.isRunning {
                throw RustAgentServerError.exitedEarly(outputCapture.snapshot())
            }

            try await Task.sleep(nanoseconds: readyPollNanoseconds)
        }

        throw RustAgentServerError.timedOut(outputCapture.snapshot())
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

    private func serverEnvironment(
        _ candidate: ServerCandidate,
        workspacePath: String
    ) -> [String: String] {
        var environment = ProcessInfo.processInfo.environment
        environment["RUST_AGENT_SERVER_TOKEN"] = token
        environment["RUST_AGENT_SERVER_HTTP_ADDR"] = candidate.httpAddress
        environment["RUST_AGENT_SERVER_H3_ADDR"] = candidate.h3Address
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

    private func capture(_ pipe: Pipe) {
        pipe.fileHandleForReading.readabilityHandler = { [outputCapture] handle in
            outputCapture.append(handle.availableData)
        }
    }
}

private struct ServerCandidate {
    let httpPort: Int
    let h3Port: Int

    var httpAddress: String {
        "127.0.0.1:\(httpPort)"
    }

    var h3Address: String {
        "127.0.0.1:\(h3Port)"
    }

    var baseURL: URL {
        URL(string: "http://\(httpAddress)")!
    }
}

enum RustAgentServerError: LocalizedError {
    case workspaceUnavailable(String)
    case invalidIdentity(String)
    case workspaceMismatch(expected: String, actual: String)
    case exitedEarly(String)
    case timedOut(String)
    case noAvailableServer([String])

    var errorDescription: String? {
        switch self {
        case let .workspaceUnavailable(path):
            return "Workspace does not exist or is not a directory: \(path)"
        case let .invalidIdentity(name):
            return "Expected rust-agent, but server identified as \(name)."
        case let .workspaceMismatch(expected, actual):
            return "rust-agent workspace mismatch. expected \(expected), got \(actual)."
        case let .exitedEarly(output):
            return "rust-agent exited before the server was ready.\(suffix(output))"
        case let .timedOut(output):
            return "Timed out waiting for rust-agent to start.\(suffix(output))"
        case let .noAvailableServer(failures):
            let details = failures.isEmpty ? "" : "\n" + failures.joined(separator: "\n")
            return "Could not start or connect to a local rust-agent server.\(details)"
        }
    }

    private func suffix(_ output: String) -> String {
        guard !output.isEmpty else { return "" }
        return "\n\(output)"
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
