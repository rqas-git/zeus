import Foundation

@MainActor
final class ChatViewModel: ObservableObject {
    @Published var lines: [TranscriptLine] = []
    @Published var draft = ""
    @Published private(set) var model = "gpt 5.5"
    @Published private(set) var effort = "medium"
    @Published private(set) var permissions = "allow"
    @Published private(set) var tokenUsage = "0 / 272k tokens"
    @Published private(set) var isReady = false
    @Published private(set) var isSending = false
    @Published private(set) var isLoggingIn = false

    let workspace = WorkspaceMetadata.current()

    private let server = RustAgentServer()
    private let auth = RustAgentAuth()
    private var client: AgentAPIClient?
    private var sessionID: UInt64?
    private var started = false
    private var currentAssistantLineID: UUID?
    private var streamTask: Task<Void, Never>?
    private var loginTask: Task<Void, Never>?

    deinit {
        streamTask?.cancel()
        loginTask?.cancel()
        auth.cancelLogin()
        server.stop()
    }

    func start() async {
        guard !started else { return }
        started = true

        append(kind: .status, text: "starting server...")

        do {
            let client = try await server.start()
            self.client = client
            append(kind: .status, text: "creating session...")

            if let models = try? await client.models() {
                model = displayModel(models.defaultModel)
            }

            let session = try await client.createSession()
            sessionID = session.sessionID
            model = displayModel(session.model)
            isReady = true
            append(kind: .status, text: "ready")
            await refreshAuthStatus()
        } catch {
            isReady = false
            append(kind: .error, text: error.localizedDescription)
        }
    }

    func sendDraft() {
        let message = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !message.isEmpty else { return }

        if message == "/login" {
            draft = ""
            append(kind: .user, text: message)
            startLogin()
            return
        }

        guard !isSending, !isLoggingIn else { return }
        guard isReady else {
            append(kind: .status, text: "rust-agent is still starting")
            return
        }
        guard let client, let sessionID else {
            append(kind: .error, text: "No rust-agent session is available.")
            return
        }

        draft = ""
        isSending = true
        currentAssistantLineID = nil
        append(kind: .user, text: message)
        replaceAssistantText("sending...")

        streamTask = Task {
            var receivedEvent = false
            var receivedAssistantOutput = false
            do {
                try await client.streamTurn(sessionID: sessionID, message: message) { event in
                    await MainActor.run {
                        receivedEvent = true
                        if event.type == "text_delta" || (event.type == "message_completed" && event.role == "assistant") {
                            receivedAssistantOutput = true
                        }
                        self.handle(event)
                    }
                }
                if !receivedEvent {
                    append(kind: .error, text: "rust-agent returned no stream events")
                } else if !receivedAssistantOutput {
                    append(kind: .status, text: "turn finished without assistant output")
                }
                finishTurn()
            } catch {
                failTurn(error)
            }
        }
    }

    func shutdown() {
        streamTask?.cancel()
        streamTask = nil
        loginTask?.cancel()
        loginTask = nil
        auth.cancelLogin()
        server.stop()
    }

    func showLoginStatus() {
        append(kind: .status, text: "checking login status...")
        Task {
            let location = auth.authFileDisplayPath
            switch await auth.status() {
            case let .loggedIn(output):
                append(kind: .status, text: "\(output). auth: \(location)")
            case .loggedOut:
                append(kind: .status, text: "Logged out. auth: \(location)")
            case let .unknown(message):
                append(kind: .error, text: "Login status unavailable: \(message)")
            }
        }
    }

    private func startLogin() {
        guard !isLoggingIn else {
            append(kind: .status, text: "login already running")
            return
        }

        isLoggingIn = true
        append(kind: .status, text: "starting rust-agent login...")

        loginTask = Task {
            do {
                try await auth.runDeviceLogin { [weak self] line in
                    self?.append(kind: .status, text: line)
                }
                append(kind: .status, text: "login complete")
                try await createFreshSession()
            } catch {
                append(kind: .error, text: error.localizedDescription)
            }
            isLoggingIn = false
        }
    }

    private func handle(_ event: AgentServerEvent) {
        switch event.type {
        case "status_changed":
            if event.status == "running" {
                replaceAssistantText("thinking...")
            }
        case "text_delta":
            appendAssistantDelta(event.delta ?? "")
        case "message_completed":
            guard event.role == "assistant" else { return }
            replaceAssistantText(event.text ?? "")
            currentAssistantLineID = nil
        case "tool_call_started":
            append(kind: .tool, text: "running \(event.toolName ?? "tool")...")
        case "tool_call_completed":
            let name = event.toolName ?? "tool"
            append(kind: .tool, text: "\(name) \(event.success == false ? "failed" : "completed")")
        case "cache_health":
            updateTokenUsage(event.cache?.usage)
        case "error":
            let message = event.message ?? "rust-agent reported an error."
            append(kind: .error, text: message)
            if message.contains("not logged in") {
                append(kind: .status, text: "type /login to authorize rust-agent")
            }
        case "turn_completed":
            isSending = false
        default:
            break
        }
    }

    private func finishTurn() {
        isSending = false
        currentAssistantLineID = nil
    }

    private func failTurn(_ error: Error) {
        isSending = false
        currentAssistantLineID = nil
        append(kind: .error, text: error.localizedDescription)
    }

    private func appendAssistantDelta(_ delta: String) {
        guard !delta.isEmpty else { return }
        let id = ensureAssistantLine()
        guard let index = lines.firstIndex(where: { $0.id == id }) else { return }
        lines[index].text += delta
    }

    private func replaceAssistantText(_ text: String) {
        let id = ensureAssistantLine()
        guard let index = lines.firstIndex(where: { $0.id == id }) else { return }
        lines[index].text = text
    }

    private func ensureAssistantLine() -> UUID {
        if let currentAssistantLineID {
            return currentAssistantLineID
        }

        let line = TranscriptLine(kind: .assistant, text: "")
        lines.append(line)
        currentAssistantLineID = line.id
        return line.id
    }

    private func append(kind: TranscriptKind, text: String) {
        lines.append(TranscriptLine(kind: kind, text: text))
    }

    private func refreshAuthStatus() async {
        switch await auth.status() {
        case .loggedIn:
            break
        case .loggedOut:
            append(kind: .status, text: "not logged in. type /login to authorize rust-agent")
        case let .unknown(message):
            append(kind: .error, text: message)
        }
    }

    private func createFreshSession() async throws {
        guard let client else { return }
        let session = try await client.createSession()
        sessionID = session.sessionID
        model = displayModel(session.model)
        append(kind: .status, text: "new session ready")
    }

    private func updateTokenUsage(_ usage: TokenUsagePayload?) {
        guard let usage else { return }
        if let total = usage.totalTokens {
            tokenUsage = "\(compactNumber(total)) / 272k tokens"
        } else if let input = usage.inputTokens, let output = usage.outputTokens {
            tokenUsage = "\(compactNumber(input + output)) / 272k tokens"
        }
    }

    private func displayModel(_ raw: String) -> String {
        raw.replacingOccurrences(of: "-", with: " ")
    }

    private func compactNumber(_ value: UInt64) -> String {
        if value >= 1_000 {
            let rounded = Double(value) / 1_000
            return String(format: "%.1fk", rounded)
        }
        return "\(value)"
    }
}
