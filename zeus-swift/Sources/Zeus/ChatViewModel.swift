import Foundation
import ZeusCore

@MainActor
final class ChatViewModel: ObservableObject {
    @Published var lines: [TranscriptLine] = []
    @Published var draft = "" {
        didSet {
            guard !isApplyingDraftFromHistory else { return }
            resetDraftHistoryNavigation()
        }
    }
    @Published var searchQuery = ""
    @Published private(set) var isSearchVisible = false
    @Published private(set) var searchResultSummary = ""
    @Published private(set) var searchMatchLineIDs: Set<UUID> = []
    @Published private(set) var selectedSearchLineID: UUID?
    @Published private(set) var isTerminalPassthroughEnabled = false
    @Published private(set) var workspace: WorkspaceMetadata
    @Published private(set) var branchOptions: [String]
    @Published private(set) var model = "gpt 5.5"
    @Published private(set) var selectedModel = "gpt-5.5"
    @Published private(set) var modelOptions = ["gpt-5.5"]
    @Published private(set) var effort = "medium"
    @Published private(set) var effortOptions = ["medium"]
    @Published private(set) var permissions = "read"
    @Published private(set) var selectedPermission = "read-only"
    @Published private(set) var permissionOptions = [
        "read-only",
        "workspace-write",
        "workspace-exec"
    ]
    @Published private(set) var tokenUsage = "0 / 272k tokens"
    @Published private(set) var isReady = false
    @Published private(set) var isSending = false
    @Published private(set) var isLoggingIn = false
    @Published private(set) var isSelectingModel = false
    @Published private(set) var isSelectingPermissions = false
    @Published private(set) var isSwitchingBranch = false
    @Published private(set) var isRunningTerminalCommand = false

    var canChangeModel: Bool {
        isReady && !isLoggingIn
    }

    var canChangeEffort: Bool {
        isReady && !isLoggingIn
    }

    var canChangePermissions: Bool {
        isReady && !isLoggingIn
    }

    var canChangeBranch: Bool {
        isReady
            && workspace.isGit
            && !branchOptions.isEmpty
            && !isSending
            && !isLoggingIn
            && !isSwitchingBranch
            && !isRunningTerminalCommand
    }

    var inputPrompt: String {
        isTerminalPassthroughEnabled ? "$" : ">"
    }

    var inputPlaceholder: String {
        isTerminalPassthroughEnabled ? "bash command..." : "type a command or ask anything..."
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
    private var branchSwitchTask: Task<Void, Never>?
    private var terminalTask: Task<Void, Never>?
    private var sessionModel = "gpt-5.5"
    private var sessionPermission = "read-only"
    private var searchMatchedLineIDsInOrder: [UUID] = []
    private var submittedMessageHistory = PromptHistory()
    private var isApplyingDraftFromHistory = false

    init(
        server: any AgentServerProtocol = RustAgentServer(),
        auth: any AgentAuthProtocol = RustAgentAuth(),
        workspace: WorkspaceMetadata = WorkspaceMetadata.current()
    ) {
        self.server = server
        self.auth = auth
        self.workspace = workspace
        self.branchOptions = [workspace.branch]
    }

    deinit {
        streamTask?.cancel()
        loginTask?.cancel()
        branchSwitchTask?.cancel()
        terminalTask?.cancel()
        auth.cancelLogin()
        server.stop()
    }

    func start() async {
        guard !started else { return }
        started = true

        append(kind: .status, text: "starting server...")

        do {
            let client = try await server.start(workspaceURL: workspace.url)
            self.client = client
            append(kind: .status, text: "creating session...")

            if let models = try? await client.models() {
                applyModels(models)
            }
            if let permissions = try? await client.permissions() {
                applyPermissions(permissions)
            }
            try? await refreshWorkspace(client: client)

            let session = try await client.createSession()
            sessionID = session.sessionID
            applySessionModel(session.model)
            applySessionPermissions(session.toolPolicy ?? selectedPermission)
            isReady = true
            append(kind: .status, text: "ready. session \(session.sessionID)")
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

        if isTerminalPassthroughEnabled {
            runTerminalCommand(message)
            return
        }

        if message == "/login" {
            recordSubmittedMessage(message)
            draft = ""
            append(kind: .user, text: message)
            startLogin()
            return
        }

        if message.split(separator: " ").first == "/restore" {
            recordSubmittedMessage(message)
            draft = ""
            append(kind: .user, text: message)
            guard let restoreSessionID = Self.restoreSessionID(from: message) else {
                append(kind: .error, text: "usage: /restore <session id>")
                return
            }
            restoreSession(restoreSessionID)
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

        recordSubmittedMessage(message)
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
                try await applySelectedPermissionsForNextTurn(client: client, sessionID: sessionID)
                let reasoningEffort = effort
                try await client.streamTurn(
                    sessionID: sessionID,
                    message: message,
                    reasoningEffort: reasoningEffort
                ) { event in
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
        branchSwitchTask?.cancel()
        branchSwitchTask = nil
        terminalTask?.cancel()
        terminalTask = nil
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

    func selectEffort(_ rawEffort: String) {
        guard rawEffort != effort else { return }
        guard canChangeEffort else { return }
        guard effortOptions.contains(rawEffort) else { return }
        effort = rawEffort
    }

    func selectPermissions(_ rawPermission: String) {
        guard rawPermission != selectedPermission else { return }
        guard canChangePermissions else { return }
        guard permissionOptions.contains(rawPermission) else { return }
        applySelectedPermissions(rawPermission)
    }

    func clearDraft() {
        draft = ""
    }

    func selectPreviousSubmittedMessage() -> Bool {
        guard let message = submittedMessageHistory.previous(currentDraft: draft) else {
            return false
        }
        applyDraftFromHistory(message)
        return true
    }

    func selectNextSubmittedMessage() -> Bool {
        guard let message = submittedMessageHistory.next() else {
            return false
        }
        applyDraftFromHistory(message)
        return true
    }

    func toggleTerminalPassthrough() {
        isTerminalPassthroughEnabled.toggle()
        append(
            kind: .status,
            text: "terminal passthrough \(isTerminalPassthroughEnabled ? "on" : "off")"
        )
    }

    func showSearch() {
        isSearchVisible = true
        refreshSearchMatches()
    }

    func closeSearch() {
        isSearchVisible = false
        searchQuery = ""
        searchMatchedLineIDsInOrder.removeAll(keepingCapacity: true)
        searchMatchLineIDs.removeAll(keepingCapacity: true)
        selectedSearchLineID = nil
        searchResultSummary = ""
    }

    func setSearchQuery(_ query: String) {
        searchQuery = query
        refreshSearchMatches()
    }

    func selectNextSearchMatch() {
        moveSearchSelection(by: 1)
    }

    func selectPreviousSearchMatch() {
        moveSearchSelection(by: -1)
    }

    func selectBranch(_ rawBranch: String) {
        guard rawBranch != workspace.branch else { return }
        guard canChangeBranch else { return }
        guard branchOptions.contains(rawBranch) else { return }
        guard let client else {
            append(kind: .error, text: "No rust-agent client is available.")
            return
        }

        isSwitchingBranch = true
        append(kind: .status, text: "switching branch to \(rawBranch)...")

        let previousBranch = workspace.branch
        branchSwitchTask = Task {
            do {
                let result = try await client.switchWorkspaceBranch(branch: rawBranch)
                applyWorkspace(result.workspace)
                let previous = result.previousBranch ?? previousBranch
                let stashStatus = result.stashedChanges
                    ? "stashed changes on \(previous); "
                    : ""
                append(kind: .status, text: "\(stashStatus)switched to \(result.branch)")
            } catch {
                try? await refreshWorkspace(client: client)
                append(kind: .error, text: error.localizedDescription)
            }
            isSwitchingBranch = false
            branchSwitchTask = nil
        }
    }

    private func runTerminalCommand(_ command: String) {
        guard !isRunningTerminalCommand else {
            append(kind: .status, text: "terminal command already running")
            return
        }

        recordSubmittedMessage(command)
        draft = ""
        isRunningTerminalCommand = true
        append(kind: .user, text: "$ \(command)")
        let rootURL = workspace.url
        terminalTask = Task {
            do {
                let result = try await Task.detached {
                    try BashPassthrough.run(command, at: rootURL)
                }.value
                appendTerminalResult(result)
                workspace = WorkspaceMetadata.current(at: rootURL)
                refreshBranchOptions()
            } catch {
                append(kind: .error, text: error.localizedDescription)
            }
            isRunningTerminalCommand = false
            terminalTask = nil
        }
    }

    private func restoreSession(_ targetSessionID: UInt64) {
        guard !isSending else {
            append(kind: .status, text: "turn already running")
            return
        }
        guard !isLoggingIn else {
            append(kind: .status, text: "login already running")
            return
        }
        guard isReady else {
            append(kind: .status, text: "rust-agent is still starting")
            return
        }
        guard let client else {
            append(kind: .error, text: "No rust-agent client is available.")
            return
        }

        isSending = true
        append(kind: .status, text: "restoring session \(targetSessionID)...")
        streamTask = Task {
            do {
                let restored = try await client.restoreSession(sessionID: targetSessionID)
                applyRestoredSession(restored)
                append(kind: .status, text: "restored session \(restored.sessionID)")
            } catch {
                append(kind: .error, text: error.localizedDescription)
            }
            isSending = false
            streamTask = nil
        }
    }

    private func appendTerminalResult(_ result: BashCommandResult) {
        let output = result.output.isEmpty ? "exit \(result.exitCode)" : result.output
        if result.exitCode == 0 {
            append(kind: .status, text: output)
        } else {
            append(kind: .error, text: "\(output)\nexit \(result.exitCode)")
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
        refreshSearchMatches()
    }

    private func replaceAssistantText(_ text: String) {
        let id = ensureAssistantLine()
        guard let index = lines.firstIndex(where: { $0.id == id }) else { return }
        lines[index].text = text
        assistantPlaceholderLineID = nil
        refreshSearchMatches()
    }

    private func showAssistantPlaceholder(_ text: String) {
        let id = ensureAssistantLine()
        guard let index = lines.firstIndex(where: { $0.id == id }) else { return }
        guard assistantPlaceholderLineID == id || lines[index].text.isEmpty else { return }
        lines[index].text = text
        assistantPlaceholderLineID = id
        refreshSearchMatches()
    }

    private func removeAssistantPlaceholder() {
        guard let assistantPlaceholderLineID,
              let index = lines.firstIndex(where: { $0.id == assistantPlaceholderLineID }) else {
            return
        }
        lines.remove(at: index)
        self.assistantPlaceholderLineID = nil
        refreshSearchMatches()
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
        refreshSearchMatches()
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
            refreshSearchMatches()
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
            refreshSearchMatches()
            return lineID
        }

        if let index = lines.lastIndex(where: { $0.kind == .assistant }) {
            lines.insert(line, at: index)
            refreshSearchMatches()
            return lineID
        }

        lines.append(line)
        refreshSearchMatches()
        return lineID
    }

    private func applyRestoredSession(_ restored: RestoreSessionResponse) {
        sessionID = restored.sessionID
        applySessionModel(restored.model)
        applySessionPermissions(restored.toolPolicy)
        currentAssistantLineID = nil
        assistantPlaceholderLineID = nil
        toolLineIDsByCallID.removeAll(keepingCapacity: true)
        toolDisplaysByCallID.removeAll(keepingCapacity: true)
        lines = transcriptLines(from: restored.messages)
        replaceSubmittedMessages(with: restored.messages)
        refreshSearchMatches()
    }

    private func transcriptLines(from records: [TranscriptRecord]) -> [TranscriptLine] {
        var restoredLines: [TranscriptLine] = []
        var lineIndexesByCallID: [String: Int] = [:]
        var displaysByCallID: [String: ToolCallTranscript] = [:]

        for record in records {
            switch record.kind {
            case "message":
                guard let kind = transcriptKind(forRole: record.role) else { continue }
                restoredLines.append(TranscriptLine(kind: kind, text: record.text ?? ""))
            case "function_call":
                let display = toolDisplay(
                    name: record.toolName,
                    arguments: record.toolArguments,
                    status: .running
                )
                let line = TranscriptLine(
                    kind: .tool,
                    text: toolFallbackText(display),
                    toolCall: display
                )
                restoredLines.append(line)
                if let callID = record.toolCallID, !callID.isEmpty {
                    lineIndexesByCallID[callID] = restoredLines.count - 1
                    displaysByCallID[callID] = display
                }
            case "function_output":
                guard let callID = record.toolCallID,
                      let index = lineIndexesByCallID[callID],
                      var display = displaysByCallID[callID] else {
                    continue
                }
                display.status = record.success == false ? .failed : .completed
                displaysByCallID[callID] = display
                restoredLines[index].text = toolFallbackText(display)
                restoredLines[index].toolCall = display
            default:
                continue
            }
        }

        return restoredLines
    }

    private func transcriptKind(forRole role: String?) -> TranscriptKind? {
        switch role {
        case "user":
            return .user
        case "assistant":
            return .assistant
        default:
            return nil
        }
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

    private func refreshSearchMatches() {
        let query = searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else {
            searchMatchedLineIDsInOrder.removeAll(keepingCapacity: true)
            searchMatchLineIDs.removeAll(keepingCapacity: true)
            selectedSearchLineID = nil
            searchResultSummary = ""
            return
        }

        let preferredLineID = selectedSearchLineID
        let matches = lines.compactMap { line -> UUID? in
            line.text.range(
                of: query,
                options: [.caseInsensitive, .diacriticInsensitive]
            ) == nil ? nil : line.id
        }
        searchMatchedLineIDsInOrder = matches
        searchMatchLineIDs = Set(matches)

        guard !matches.isEmpty else {
            selectedSearchLineID = nil
            searchResultSummary = "no matches"
            return
        }

        let selectedIndex = preferredLineID
            .flatMap { matches.firstIndex(of: $0) }
            ?? 0
        selectedSearchLineID = matches[selectedIndex]
        searchResultSummary = "\(selectedIndex + 1) / \(matches.count) lines"
    }

    private func moveSearchSelection(by offset: Int) {
        guard !searchMatchedLineIDsInOrder.isEmpty else { return }
        let currentIndex = selectedSearchLineID
            .flatMap { searchMatchedLineIDsInOrder.firstIndex(of: $0) }
            ?? 0
        let nextIndex = (
            currentIndex + offset + searchMatchedLineIDsInOrder.count
        ) % searchMatchedLineIDsInOrder.count
        selectedSearchLineID = searchMatchedLineIDsInOrder[nextIndex]
        searchResultSummary = "\(nextIndex + 1) / \(searchMatchedLineIDsInOrder.count) lines"
    }

    private func createFreshSession() async throws {
        guard let client else { return }
        let session = try await client.createSession()
        sessionID = session.sessionID
        applySessionModel(session.model)
        applySessionPermissions(session.toolPolicy ?? selectedPermission)
        replaceSubmittedMessages(with: [])
        append(kind: .status, text: "new session ready. session \(session.sessionID)")
    }

    private func applyModels(_ models: ModelsResponse) {
        modelOptions = models.allowedModels
        applySelectedModel(models.defaultModel)
        applyReasoningEfforts(models.reasoningEfforts, defaultEffort: models.defaultReasoningEffort)
    }

    private func refreshWorkspace(
        client: any AgentClientProtocol,
        fallbackURL: URL? = nil
    ) async throws {
        do {
            applyWorkspace(try await client.workspace())
        } catch {
            if let fallbackURL {
                workspace = WorkspaceMetadata.current(at: fallbackURL)
                branchOptions = [workspace.branch]
            }
            throw error
        }
    }

    private func applyWorkspace(_ response: WorkspaceResponse) {
        workspace = workspace.applying(response)
        branchOptions = Self.branchOptions(from: response, currentBranch: workspace.branch)
    }

    private func refreshBranchOptions() {
        branchOptions = workspace.isGit ? [workspace.branch] : []
    }

    private static func branchOptions(
        from response: WorkspaceResponse,
        currentBranch: String
    ) -> [String] {
        guard response.git else { return [] }
        var branches = response.branches
        if !branches.contains(currentBranch) {
            branches.append(currentBranch)
        }
        return branches
    }

    private static func restoreSessionID(from message: String) -> UInt64? {
        let parts = message.split(separator: " ")
        guard parts.count == 2, parts[0] == "/restore" else { return nil }
        return UInt64(parts[1])
    }

    private func applyPermissions(_ permissions: PermissionsResponse) {
        permissionOptions = permissions.allowedToolPolicies.isEmpty
            ? [permissions.defaultToolPolicy]
            : permissions.allowedToolPolicies
        if !permissionOptions.contains(permissions.defaultToolPolicy) {
            permissionOptions.append(permissions.defaultToolPolicy)
        }
        if !permissionOptions.contains(selectedPermission) {
            applySelectedPermissions(permissions.defaultToolPolicy)
        }
    }

    private func applyReasoningEfforts(_ efforts: [String], defaultEffort: String) {
        effortOptions = efforts.isEmpty ? [defaultEffort] : efforts
        if !effortOptions.contains(defaultEffort) {
            effortOptions.append(defaultEffort)
        }
        if !effortOptions.contains(effort) {
            effort = defaultEffort
        }
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

    private func applySelectedPermissions(_ rawPermission: String) {
        selectedPermission = rawPermission
        permissions = displayPermission(rawPermission)
        if !permissionOptions.contains(rawPermission) {
            permissionOptions.append(rawPermission)
        }
    }

    private func applySessionPermissions(_ rawPermission: String, selectedTarget: String? = nil) {
        sessionPermission = rawPermission
        if selectedTarget == nil || selectedPermission == selectedTarget {
            applySelectedPermissions(rawPermission)
        } else if !permissionOptions.contains(rawPermission) {
            permissionOptions.append(rawPermission)
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

    private func applySelectedPermissionsForNextTurn(
        client: any AgentClientProtocol,
        sessionID: UInt64
    ) async throws {
        guard selectedPermission != sessionPermission else { return }

        isSelectingPermissions = true
        defer { isSelectingPermissions = false }

        while selectedPermission != sessionPermission {
            let target = selectedPermission
            let response = try await client.setSessionPermissions(
                sessionID: sessionID,
                toolPolicy: target
            )
            applySessionPermissions(response.toolPolicy, selectedTarget: target)
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

    func displayPermission(_ raw: String) -> String {
        switch raw {
        case "workspace-exec":
            return "bash"
        case "workspace-write":
            return "edit"
        case "read-only":
            return "read"
        default:
            return raw
        }
    }

    private func compactNumber(_ value: UInt64) -> String {
        if value >= 1_000 {
            let rounded = Double(value) / 1_000
            return String(format: "%.1fk", rounded)
        }
        return "\(value)"
    }

    private func recordSubmittedMessage(_ message: String) {
        submittedMessageHistory.record(message)
    }

    private func replaceSubmittedMessages(with records: [TranscriptRecord]) {
        let messages: [String] = records.compactMap { record in
            guard record.kind == "message",
                  record.role == "user",
                  let text = record.text,
                  !text.isEmpty else {
                return nil
            }
            return text
        }
        submittedMessageHistory.replace(with: messages)
    }

    private func applyDraftFromHistory(_ value: String) {
        isApplyingDraftFromHistory = true
        draft = value
        isApplyingDraftFromHistory = false
    }

    private func resetDraftHistoryNavigation() {
        submittedMessageHistory.reset()
    }
}
