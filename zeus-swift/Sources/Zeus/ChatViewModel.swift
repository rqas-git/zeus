import Foundation
import ZeusCore

@MainActor
final class ChatViewModel: ObservableObject {
    @Published var lines: [TranscriptLine] = []
    @Published var draft = ""
    @Published private(set) var model = "gpt 5.5"
    @Published private(set) var selectedModel = "gpt-5.5"
    @Published private(set) var modelOptions = ["gpt-5.5"]
    @Published private(set) var effort = "medium"
    @Published private(set) var permissions = "allow"
    @Published private(set) var tokenUsage = "0 / 272k tokens"
    @Published private(set) var isReady = false
    @Published private(set) var isSending = false
    @Published private(set) var isLoggingIn = false
    @Published private(set) var isSelectingModel = false

    let workspace: WorkspaceMetadata

    var canChangeModel: Bool {
        isReady && !isLoggingIn
    }

    private let pendingStateDwellNanoseconds: UInt64 = 180_000_000
    private let server: any AgentServerProtocol
    private let auth: any AgentAuthProtocol
    private var client: (any AgentClientProtocol)?
    private var sessionID: UInt64?
    private var started = false
    private var currentAssistantLineID: UUID?
    private var assistantPlaceholderLineID: UUID?
    private var toolLineIDsByCallID: [String: UUID] = [:]
    private var toolDisplaysByCallID: [String: ToolCallTranscript] = [:]
    private var streamTask: Task<Void, Never>?
    private var loginTask: Task<Void, Never>?
    private var sessionModel = "gpt-5.5"

    init(
        server: any AgentServerProtocol = RustAgentServer(),
        auth: any AgentAuthProtocol = RustAgentAuth(),
        workspace: WorkspaceMetadata = WorkspaceMetadata.current()
    ) {
        self.server = server
        self.auth = auth
        self.workspace = workspace
    }

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
                applyModels(models)
            }

            let session = try await client.createSession()
            sessionID = session.sessionID
            applySessionModel(session.model)
            isReady = true
            append(kind: .status, text: "ready")
            await refreshAuthStatus()
        } catch {
            started = false
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
        assistantPlaceholderLineID = nil
        toolLineIDsByCallID.removeAll(keepingCapacity: true)
        toolDisplaysByCallID.removeAll(keepingCapacity: true)
        append(kind: .user, text: message)
        showAssistantPlaceholder("sending...")

        streamTask = Task {
            var receivedEvent = false
            var receivedAssistantOutput = false
            do {
                try await Task.sleep(nanoseconds: pendingStateDwellNanoseconds)
                try await applySelectedModelForNextTurn(client: client, sessionID: sessionID)
                try await client.streamTurn(sessionID: sessionID, message: message) { event in
                    if case let .statusChanged(_, status) = event, status == "running" {
                        try? await Task.sleep(nanoseconds: self.pendingStateDwellNanoseconds)
                    }
                    await MainActor.run {
                        receivedEvent = true
                        if event.isAssistantOutputEvent {
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

    func selectModel(_ rawModel: String) {
        guard rawModel != selectedModel else { return }
        guard canChangeModel else { return }
        applySelectedModel(rawModel)
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
        switch event {
        case let .statusChanged(_, status):
            if status == "running" {
                showAssistantPlaceholder("thinking...")
            }
        case let .textDelta(_, delta):
            appendAssistantDelta(delta ?? "")
        case let .messageCompleted(_, role, text):
            guard role == "assistant" else { return }
            replaceAssistantText(text ?? "")
            currentAssistantLineID = nil
        case let .toolCallStarted(_, toolCallID, toolName, toolArguments):
            upsertToolLine(
                callID: toolCallID,
                display: toolDisplay(
                    name: toolName,
                    arguments: toolArguments,
                    status: .running
                )
            )
        case let .toolCallCompleted(_, toolCallID, toolName, toolArguments, success):
            let callID = toolCallID ?? ""
            var display = toolDisplaysByCallID[callID]
                ?? toolDisplay(
                    name: toolName,
                    arguments: toolArguments,
                    status: .completed
                )
            display.status = success == false ? .failed : .completed
            upsertToolLine(
                callID: toolCallID,
                display: display
            )
        case let .cacheHealth(_, cache):
            updateTokenUsage(cache?.usage)
        case let .error(_, eventMessage):
            let message = eventMessage ?? "rust-agent reported an error."
            append(kind: .error, text: message)
            if message.contains("not logged in") {
                append(kind: .status, text: "type /login to authorize rust-agent")
            }
        case .turnCompleted:
            break
        case .unknown:
            break
        }
    }

    private func finishTurn() {
        isSending = false
        streamTask = nil
        currentAssistantLineID = nil
        assistantPlaceholderLineID = nil
    }

    private func failTurn(_ error: Error) {
        isSending = false
        streamTask = nil
        removeAssistantPlaceholder()
        currentAssistantLineID = nil
        assistantPlaceholderLineID = nil
        append(kind: .error, text: error.localizedDescription)
    }

    private func appendAssistantDelta(_ delta: String) {
        guard !delta.isEmpty else { return }
        let id = ensureAssistantLine()
        guard let index = lines.firstIndex(where: { $0.id == id }) else { return }
        if assistantPlaceholderLineID == id {
            lines[index].text = ""
            assistantPlaceholderLineID = nil
        }
        lines[index].text += delta
    }

    private func replaceAssistantText(_ text: String) {
        let id = ensureAssistantLine()
        guard let index = lines.firstIndex(where: { $0.id == id }) else { return }
        lines[index].text = text
        assistantPlaceholderLineID = nil
    }

    private func showAssistantPlaceholder(_ text: String) {
        let id = ensureAssistantLine()
        guard let index = lines.firstIndex(where: { $0.id == id }) else { return }
        guard assistantPlaceholderLineID == id || lines[index].text.isEmpty else { return }
        lines[index].text = text
        assistantPlaceholderLineID = id
    }

    private func removeAssistantPlaceholder() {
        guard let assistantPlaceholderLineID,
              let index = lines.firstIndex(where: { $0.id == assistantPlaceholderLineID }) else {
            return
        }
        lines.remove(at: index)
        self.assistantPlaceholderLineID = nil
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

    private func upsertToolLine(
        callID: String?,
        display: ToolCallTranscript
    ) {
        let text = toolFallbackText(display)

        guard let callID, !callID.isEmpty else {
            insertToolLine(text, display: display)
            return
        }

        toolDisplaysByCallID[callID] = display
        if let lineID = toolLineIDsByCallID[callID],
           let index = lines.firstIndex(where: { $0.id == lineID }) {
            lines[index].text = text
            lines[index].toolCall = display
            return
        }

        let lineID = insertToolLine(text, display: display)
        toolLineIDsByCallID[callID] = lineID
    }

    @discardableResult
    private func insertToolLine(_ text: String, display: ToolCallTranscript? = nil) -> UUID {
        let line = TranscriptLine(kind: .tool, text: text, toolCall: display)
        let lineID = line.id

        if let currentAssistantLineID,
           let index = lines.firstIndex(where: { $0.id == currentAssistantLineID }) {
            lines.insert(line, at: index)
            return lineID
        }

        if let index = lines.lastIndex(where: { $0.kind == .assistant }) {
            lines.insert(line, at: index)
            return lineID
        }

        lines.append(line)
        return lineID
    }

    private func toolDisplay(
        name: String?,
        arguments: String?,
        status: ToolCallStatus
    ) -> ToolCallTranscript {
        let metadata = ToolMetadata.forName(name)
        return ToolCallTranscript(
            name: metadata.displayName,
            action: metadata.action,
            iconName: metadata.iconName,
            target: metadata.target(fromArgumentsJSON: arguments),
            status: status
        )
    }

    private func toolFallbackText(_ display: ToolCallTranscript) -> String {
        let target = display.target.map { " \($0)" } ?? ""
        switch display.status {
        case .running:
            return "\(display.action) \(display.name)\(target)..."
        case .completed:
            return "\(display.name)\(target) completed"
        case .failed:
            return "\(display.name)\(target) failed"
        }
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
        applySessionModel(session.model)
        append(kind: .status, text: "new session ready")
    }

    private func applyModels(_ models: ModelsResponse) {
        modelOptions = models.allowedModels
        applySelectedModel(models.defaultModel)
    }

    private func applySelectedModel(_ rawModel: String) {
        selectedModel = rawModel
        model = displayModel(rawModel)
        if !modelOptions.contains(rawModel) {
            modelOptions.append(rawModel)
        }
    }

    private func applySessionModel(_ rawModel: String, selectedTarget: String? = nil) {
        sessionModel = rawModel
        if selectedTarget == nil || selectedModel == selectedTarget {
            applySelectedModel(rawModel)
        } else if !modelOptions.contains(rawModel) {
            modelOptions.append(rawModel)
        }
    }

    private func applySelectedModelForNextTurn(
        client: any AgentClientProtocol,
        sessionID: UInt64
    ) async throws {
        guard selectedModel != sessionModel else { return }

        isSelectingModel = true
        defer { isSelectingModel = false }

        while selectedModel != sessionModel {
            let target = selectedModel
            let response = try await client.setSessionModel(sessionID: sessionID, model: target)
            applySessionModel(response.model, selectedTarget: target)
        }
    }

    private func updateTokenUsage(_ usage: TokenUsagePayload?) {
        guard let usage else { return }
        if let total = usage.totalTokens {
            tokenUsage = "\(compactNumber(total)) / 272k tokens"
        } else if let input = usage.inputTokens, let output = usage.outputTokens {
            tokenUsage = "\(compactNumber(input + output)) / 272k tokens"
        }
    }

    func displayModel(_ raw: String) -> String {
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
