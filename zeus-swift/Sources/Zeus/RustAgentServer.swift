import Foundation

final class RustAgentServer {
    let token = "zeus-swift-dev-token"

    private let candidates = [
        ServerCandidate(httpPort: 4196, h3Port: 4533),
        ServerCandidate(httpPort: 4197, h3Port: 4534),
        ServerCandidate(httpPort: 4198, h3Port: 4535)
    ]
    private var process: Process?
    private let outputCapture = ProcessOutputCapture()

    deinit {
        stop()
    }

    func start() async throws -> AgentAPIClient {
        var failures: [String] = []

        for candidate in candidates {
            let client = AgentAPIClient(baseURL: candidate.baseURL, token: token)
            if await client.healthz() {
                do {
                    _ = try await client.models()
                    return client
                } catch {
                    failures.append("\(candidate.httpAddress) is occupied but rejected Zeus auth: \(error.localizedDescription)")
                    continue
                }
            }

            do {
                try launch(candidate)
                try await waitForReady(client)
                return client
            } catch {
                stop()
                failures.append("\(candidate.httpAddress): \(error.localizedDescription)")
            }
        }

        throw RustAgentServerError.noAvailableServer(failures)
    }

    func stop() {
        guard let process else { return }
        self.process = nil
        if process.isRunning {
            process.terminate()
        }
    }

    private func launch(_ candidate: ServerCandidate) throws {
        let rustAgentRoot = RustAgentLocator.rootURL()
        let process = Process()
        RustAgentLocator.configure(process, rootURL: rustAgentRoot, arguments: ["serve"])
        process.environment = serverEnvironment(candidate)

        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        capture(stdout)
        capture(stderr)

        try process.run()
        self.process = process
    }

    private func waitForReady(_ client: AgentAPIClient) async throws {
        for _ in 0..<80 {
            if await client.healthz(), (try? await client.models()) != nil {
                return
            }

            if let process, !process.isRunning {
                throw RustAgentServerError.exitedEarly(outputCapture.snapshot())
            }

            try await Task.sleep(nanoseconds: 100_000_000)
        }

        throw RustAgentServerError.timedOut(outputCapture.snapshot())
    }

    private func serverEnvironment(_ candidate: ServerCandidate) -> [String: String] {
        var environment = ProcessInfo.processInfo.environment
        environment["RUST_AGENT_SERVER_TOKEN"] = token
        environment["RUST_AGENT_SERVER_HTTP_ADDR"] = candidate.httpAddress
        environment["RUST_AGENT_SERVER_H3_ADDR"] = candidate.h3Address
        environment["RUST_AGENT_CACHE_HEALTH"] = "1"
        return environment
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
    case exitedEarly(String)
    case timedOut(String)
    case noAvailableServer([String])

    var errorDescription: String? {
        switch self {
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
