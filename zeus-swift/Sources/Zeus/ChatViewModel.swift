import Foundation
import Observation
import ZeusCore

@MainActor
@Observable
final class ChatViewModel {
    var lines: [TranscriptLine] = []
    private(set) var transcriptScrollTarget: TranscriptScrollTarget?
    private(set) var activeAssistantStream: ActiveAssistantStream?
    var draft = "" {
        didSet {
            guard !isApplyingDraftFromHistory else { return }
            resetDraftHistoryNavigation()
        }
    }
    var draftSelectionLocation: Int?
    private(set) var pathCompletion: PromptPathCompletionState?
    var searchQuery = ""
    private(set) var isSearchVisible = false
    private(set) var searchResultSummary = ""
    private(set) var searchMatchLineIDs: Set<UUID> = []
    private(set) var selectedSearchLineID: UUID?
    private(set) var isTerminalPassthroughEnabled = false
    private(set) var workspace: WorkspaceMetadata
    private(set) var branchOptions: [String]
    private(set) var model = "gpt 5.5"
    private(set) var selectedModel = "gpt-5.5"
    private(set) var modelOptions = ["gpt-5.5"]
    private(set) var effort = "medium"
    private(set) var effortOptions = ["medium"]
    private(set) var permissions = "read"
    private(set) var selectedPermission = "read-only"
    private(set) var permissionOptions = [
        "read-only",
        "workspace-write",
        "workspace-exec"
    ]
    private(set) var showCacheStats = false
    private(set) var tokenUsage = "0 / 272k tokens"
    private(set) var startupInputPlaceholder: String?
    private(set) var isReady = false
    private(set) var isSending = false
    private(set) var canCancelTurn = false
    private(set) var isLoggedIn = false
    private(set) var isLoggingIn = false
    private(set) var isSelectingModel = false
    private(set) var isSelectingPermissions = false
    private(set) var isSwitchingBranch = false
    private(set) var isRunningTerminalCommand = false
    private(set) var isClearingContext = false
    private(set) var isCompactingContext = false

    var canChangeModel: Bool {
        isReady && !isLoggingIn && !isClearingContext && !isCompactingContext
    }

    var canChangeEffort: Bool {
        isReady && !isLoggingIn && !isClearingContext && !isCompactingContext
    }

    var canChangePermissions: Bool {
        isReady && !isLoggingIn && !isClearingContext && !isCompactingContext
    }

    var canChangeBranch: Bool {
        isReady
            && workspace.isGit
            && !branchOptions.isEmpty
            && !isSending
            && !isLoggingIn
            && !isSwitchingBranch
            && !isRunningTerminalCommand
            && !isClearingContext
            && !isCompactingContext
    }

    var canClearContext: Bool {
        isReady
            && !isSending
            && !isLoggingIn
            && !isSwitchingBranch
            && !isRunningTerminalCommand
            && !isClearingContext
            && !isCompactingContext
            && client != nil
            && sessionID != nil
    }

    var canCompactContext: Bool {
        isReady
            && !isSending
            && !isLoggingIn
            && !isSelectingModel
            && !isSwitchingBranch
            && !isRunningTerminalCommand
            && !isClearingContext
            && !isCompactingContext
            && client != nil
            && sessionID != nil
    }

    var inputPrompt: String {
        isTerminalPassthroughEnabled ? "$" : ">"
    }

    var inputPlaceholder: String {
        if let startupInputPlaceholder {
            return startupInputPlaceholder
        }
        guard !lines.contains(where: { $0.kind == .user }) else { return "" }
        return isTerminalPassthroughEnabled ? "bash command..." : "type a command or ask anything..."
    }

    private static let searchRefreshDebounceNanoseconds: UInt64 = 120_000_000
    private static let pathCompletionDebounceNanoseconds: UInt64 = 20_000_000
    private static let pathCompletionLimit = 20
    private static let assistantDisplayFlushNanoseconds: UInt64 = 33_000_000
    private static let assistantDisplayImmediateFlushCharacters = 2_000
    private static let assistantScrollThrottleSeconds: TimeInterval = 0.08
    @ObservationIgnored private let server: any AgentServerProtocol
    @ObservationIgnored private let auth: any AgentAuthProtocol
    @ObservationIgnored private var client: (any AgentClientProtocol)?
    @ObservationIgnored private var sessionID: UInt64?
    @ObservationIgnored private var started = false
    @ObservationIgnored private var currentAssistantLineID: UUID?
    @ObservationIgnored private var assistantPlaceholderLineID: UUID?
    @ObservationIgnored private var currentTurnFinalAssistantLineID: UUID?
    @ObservationIgnored private var currentTurnHadToolActivity = false
    @ObservationIgnored private var toolLineIDsByCallID: [String: UUID] = [:]
    @ObservationIgnored private var toolDisplaysByCallID: [String: ToolCallTranscript] = [:]
    @ObservationIgnored private var lastAssistantScrollRequest = Date.distantPast
    @ObservationIgnored private var streamTask: Task<Void, Never>?
    @ObservationIgnored private var sessionEventTask: Task<Void, Never>?
    @ObservationIgnored private var loginTask: Task<Void, Never>?
    @ObservationIgnored private var branchSwitchTask: Task<Void, Never>?
    @ObservationIgnored private var terminalTask: Task<Void, Never>?
    @ObservationIgnored private var contextClearTask: Task<Void, Never>?
    @ObservationIgnored private var contextCompactTask: Task<Void, Never>?
    @ObservationIgnored private var searchRefreshTask: Task<Void, Never>?
    @ObservationIgnored private var pathCompletionTask: Task<Void, Never>?
    @ObservationIgnored private var markdownRenderTask: Task<Void, Never>?
    @ObservationIgnored private var assistantDisplayFlushTask: Task<Void, Never>?
    @ObservationIgnored private var pendingAssistantDisplayFragment = ""
    @ObservationIgnored private var assistantStreamChunks: [StreamingTextChunk] = []
    @ObservationIgnored private var assistantStreamTail = ""
    @ObservationIgnored private var nextAssistantStreamChunkID = 0
    @ObservationIgnored private var pendingCacheStats: [ResponseCacheStats] = []
    @ObservationIgnored private var pendingMarkdownRenders: [UUID: MarkdownRenderSnapshot] = [:]
    @ObservationIgnored private var isSessionEventStreamConnected = false
    @ObservationIgnored private var sessionModel = "gpt-5.5"
    @ObservationIgnored private var sessionPermission = "read-only"
    @ObservationIgnored private var lineIndexesByID: [UUID: Int] = [:]
    @ObservationIgnored private var searchMatchedLineIDsInOrder: [UUID] = []
    @ObservationIgnored private var searchRefreshRevision = 0
    @ObservationIgnored private var pathCompletionRevision = 0
    @ObservationIgnored private var transcriptScrollRevision = 0
    @ObservationIgnored private var submittedMessageHistory = PromptHistory()
    @ObservationIgnored private var isApplyingDraftFromHistory = false

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
        sessionEventTask?.cancel()
        loginTask?.cancel()
        branchSwitchTask?.cancel()
        terminalTask?.cancel()
        contextClearTask?.cancel()
        contextCompactTask?.cancel()
        searchRefreshTask?.cancel()
        pathCompletionTask?.cancel()
        markdownRenderTask?.cancel()
        assistantDisplayFlushTask?.cancel()
        auth.cancelLogin()
        server.stop()
    }

    func start() async {
        guard !started else { return }
        started = true

        startupInputPlaceholder = "starting server..."

        do {
            let client = try await server.start(workspaceURL: workspace.url)
            self.client = client
            startupInputPlaceholder = "creating session..."

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
            startupInputPlaceholder = "connecting session events..."
            try await startSessionEventStream(sessionID: session.sessionID, client: client)
            isReady = true
            startupInputPlaceholder = nil
            append(kind: .status, text: "Session ID: \(session.sessionID)")
            await refreshAuthStatus()
        } catch {
            started = false
            isReady = false
            startupInputPlaceholder = nil
            append(kind: .error, text: error.localizedDescription)
        }
    }

    func sendDraft() {
        if acceptPathCompletion() {
            return
        }
        cancelPathCompletion()

        let message = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !message.isEmpty else { return }
        guard !isClearingContext else {
            append(kind: .status, text: "context clear already running")
            return
        }
        guard !isCompactingContext else {
            append(kind: .status, text: "context compaction already running")
            return
        }

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

        if message == "/show-cache" {
            recordSubmittedMessage(message)
            draft = ""
            showCacheStats.toggle()
            append(kind: .user, text: message)
            append(kind: .status, text: showCacheStats ? "cache stats shown" : "cache stats hidden")
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
        canCancelTurn = true
        currentAssistantLineID = nil
        assistantPlaceholderLineID = nil
        currentTurnFinalAssistantLineID = nil
        currentTurnHadToolActivity = false
        activeAssistantStream = nil
        resetAssistantStreamBuffers()
        lastAssistantScrollRequest = .distantPast
        pendingCacheStats.removeAll(keepingCapacity: true)
        toolLineIDsByCallID.removeAll(keepingCapacity: true)
        toolDisplaysByCallID.removeAll(keepingCapacity: true)
        append(kind: .user, text: message)
        showAssistantPlaceholder("sending...")

        streamTask = Task {
            do {
                try await withTaskCancellationHandler {
                    try await applySelectedModelForNextTurn(client: client, sessionID: sessionID)
                    try await applySelectedPermissionsForNextTurn(
                        client: client,
                        sessionID: sessionID
                    )
                    let reasoningEffort = effort
                    try await client.streamTurn(
                        sessionID: sessionID,
                        message: message,
                        reasoningEffort: reasoningEffort
                    ) { event in
                        await MainActor.run {
                            guard self.sessionID == sessionID else { return }
                            self.handle(event)
                        }
                    }
                } onCancel: {
                    Task {
                        _ = try? await client.cancelTurn(sessionID: sessionID)
                    }
                }
                if isSending {
                    finishTurn()
                }
            } catch {
                if Self.isCancellation(error) {
                    finishCancelledTurn()
                } else {
                    failTurn(error)
                }
            }
        }
    }

    func cancelCurrentTurn() {
        guard canCancelTurn else { return }
        streamTask?.cancel()
    }

    func shutdown() {
        streamTask?.cancel()
        streamTask = nil
        sessionEventTask?.cancel()
        sessionEventTask = nil
        isSessionEventStreamConnected = false
        loginTask?.cancel()
        loginTask = nil
        branchSwitchTask?.cancel()
        branchSwitchTask = nil
        terminalTask?.cancel()
        terminalTask = nil
        contextClearTask?.cancel()
        contextClearTask = nil
        contextCompactTask?.cancel()
        contextCompactTask = nil
        searchRefreshTask?.cancel()
        searchRefreshTask = nil
        pathCompletionTask?.cancel()
        pathCompletionTask = nil
        pathCompletion = nil
        markdownRenderTask?.cancel()
        markdownRenderTask = nil
        assistantDisplayFlushTask?.cancel()
        assistantDisplayFlushTask = nil
        pendingMarkdownRenders.removeAll(keepingCapacity: true)
        auth.cancelLogin()
        server.stop()
    }

    func showLoginStatus() {
        append(kind: .status, text: "checking login status...")
        Task {
            let location = auth.authFileDisplayPath
            switch await auth.status() {
            case let .loggedIn(output):
                isLoggedIn = true
                append(kind: .status, text: "\(output). auth: \(location)")
            case .loggedOut:
                isLoggedIn = false
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
        cancelPathCompletion()
        draft = ""
        draftSelectionLocation = nil
    }

    func updateDraftFromInput(_ value: String, cursor: Int) {
        if draft != value {
            draft = value
        }
        draftSelectionLocation = nil
        requestAutomaticPathCompletion(cursor: cursor)
    }

    func triggerPathCompletion(cursor: Int) -> Bool {
        guard isReady else {
            cancelPathCompletion()
            return true
        }
        guard let context = PromptPathCompletion.context(
            in: draft,
            cursor: cursor,
            explicitTab: true,
            terminalMode: isTerminalPassthroughEnabled
        ) else {
            cancelPathCompletion()
            return true
        }
        requestPathCompletion(context: context, debounce: false, acceptsSingleSuggestion: true)
        return true
    }

    func movePathCompletionSelection(by delta: Int) -> Bool {
        guard var completion = pathCompletion, !completion.suggestions.isEmpty else {
            return false
        }
        let count = completion.suggestions.count
        completion.selectedIndex = (completion.selectedIndex + delta + count) % count
        pathCompletion = completion
        return true
    }

    func highlightPathCompletion(index: Int) {
        guard var completion = pathCompletion,
              completion.suggestions.indices.contains(index) else {
            return
        }
        completion.selectedIndex = index
        pathCompletion = completion
    }

    func selectPathCompletion(index: Int) {
        highlightPathCompletion(index: index)
        _ = acceptPathCompletion()
    }

    @discardableResult
    func acceptPathCompletion() -> Bool {
        guard let completion = pathCompletion,
              let suggestion = completion.selectedSuggestion else {
            return false
        }
        applyPathCompletion(suggestion, context: completion.context)
        return true
    }

    @discardableResult
    func cancelPathCompletion() -> Bool {
        let hadCompletion = pathCompletion != nil || pathCompletionTask != nil
        pathCompletionRevision += 1
        pathCompletionTask?.cancel()
        pathCompletionTask = nil
        pathCompletion = nil
        return hadCompletion
    }

    func clearContext() {
        cancelPathCompletion()
        guard canClearContext else { return }
        guard let client, let previousSessionID = sessionID else {
            append(kind: .error, text: "No rust-agent session is available.")
            return
        }

        isClearingContext = true
        contextClearTask = Task {
            defer {
                isClearingContext = false
                contextClearTask = nil
            }
            do {
                let session = try await client.createSession()
                sessionID = session.sessionID
                applySessionModel(session.model)
                applySessionPermissions(session.toolPolicy ?? selectedPermission)
                resetActiveTranscriptState()
                tokenUsage = "0 / 272k tokens"
                replaceSubmittedMessages(with: [])
                replaceTranscriptLines([
                    TranscriptLine(
                        kind: .status,
                        text: "cleared conversation with id \(previousSessionID)"
                    )
                ])
                refreshSearchMatches(debounce: false)
                requestScrollToLastLine()
                try await startSessionEventStream(sessionID: session.sessionID, client: client)
            } catch {
                if !Self.isCancellation(error) {
                    append(kind: .error, text: error.localizedDescription)
                }
            }
        }
    }

    func compactContext() {
        cancelPathCompletion()
        guard canCompactContext else { return }
        guard let client, let sessionID else {
            append(kind: .error, text: "No rust-agent session is available.")
            return
        }

        isCompactingContext = true
        append(kind: .status, text: "compacting context...")
        contextCompactTask = Task {
            defer {
                isCompactingContext = false
                contextCompactTask = nil
            }
            do {
                try await applySelectedModelForNextTurn(client: client, sessionID: sessionID)
                let response = try await client.compactSession(
                    sessionID: sessionID,
                    instructions: nil,
                    reasoningEffort: effort
                )
                guard self.sessionID == sessionID else { return }
                append(kind: .status, text: compactionStatus(response))
            } catch {
                if !Self.isCancellation(error) {
                    append(kind: .error, text: error.localizedDescription)
                }
            }
        }
    }

    func selectPreviousSubmittedMessage() -> Bool {
        guard let message = submittedMessageHistory.previous(currentDraft: draft) else {
            return false
        }
        cancelPathCompletion()
        applyDraftFromHistory(message)
        return true
    }

    func selectNextSubmittedMessage() -> Bool {
        guard let message = submittedMessageHistory.next() else {
            return false
        }
        cancelPathCompletion()
        applyDraftFromHistory(message)
        return true
    }

    private func requestAutomaticPathCompletion(cursor: Int) {
        guard let context = PromptPathCompletion.context(
            in: draft,
            cursor: cursor,
            explicitTab: false,
            terminalMode: isTerminalPassthroughEnabled
        ) else {
            cancelPathCompletion()
            return
        }
        requestPathCompletion(context: context, debounce: true, acceptsSingleSuggestion: false)
    }

    private func requestPathCompletion(
        context: PromptPathCompletionContext,
        debounce: Bool,
        acceptsSingleSuggestion: Bool
    ) {
        guard isReady, let client else {
            cancelPathCompletion()
            return
        }

        pathCompletionRevision += 1
        let revision = pathCompletionRevision
        let draftSnapshot = draft
        pathCompletionTask?.cancel()
        pathCompletionTask = Task { [weak self] in
            if debounce {
                do {
                    try await Task.sleep(nanoseconds: Self.pathCompletionDebounceNanoseconds)
                } catch {
                    return
                }
            }

            do {
                let response = try await client.completePaths(
                    prefix: context.prefix,
                    kind: context.kind.rawValue,
                    limit: Self.pathCompletionLimit
                )
                guard !Task.isCancelled else { return }
                await MainActor.run {
                    guard let self,
                          self.pathCompletionRevision == revision,
                          self.draft == draftSnapshot else {
                        return
                    }
                    self.pathCompletionTask = nil
                    guard !response.suggestions.isEmpty else {
                        self.pathCompletion = nil
                        return
                    }
                    if acceptsSingleSuggestion, response.suggestions.count == 1 {
                        self.applyPathCompletion(response.suggestions[0], context: context)
                        return
                    }
                    self.pathCompletion = PromptPathCompletionState(
                        context: context,
                        suggestions: response.suggestions,
                        selectedIndex: 0
                    )
                }
            } catch {
                guard !Task.isCancelled else { return }
                await MainActor.run {
                    guard let self, self.pathCompletionRevision == revision else { return }
                    self.pathCompletionTask = nil
                    self.pathCompletion = nil
                }
            }
        }
    }

    private func applyPathCompletion(
        _ suggestion: PathCompletionSuggestion,
        context: PromptPathCompletionContext
    ) {
        let result = PromptPathCompletion.apply(
            suggestion: suggestion,
            to: draft,
            context: context
        )
        cancelPathCompletion()
        draft = result.text
        draftSelectionLocation = result.cursor
    }

    func toggleTerminalPassthrough() {
        cancelPathCompletion()
        isTerminalPassthroughEnabled.toggle()
        append(
            kind: .status,
            text: "terminal passthrough \(isTerminalPassthroughEnabled ? "on" : "off")"
        )
    }

    func showSearch() {
        cancelPathCompletion()
        isSearchVisible = true
        refreshSearchMatches(debounce: false)
    }

    func closeSearch() {
        searchRefreshRevision += 1
        searchRefreshTask?.cancel()
        searchRefreshTask = nil
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

        cancelPathCompletion()
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
        guard !isClearingContext else {
            append(kind: .status, text: "context clear already running")
            return
        }
        guard !isCompactingContext else {
            append(kind: .status, text: "context compaction already running")
            return
        }
        guard !isRunningTerminalCommand else {
            append(kind: .status, text: "terminal command already running")
            return
        }
        guard !isSending else {
            append(kind: .status, text: "turn already running")
            return
        }
        guard isReady else {
            append(kind: .status, text: "rust-agent is still starting")
            return
        }
        guard let client, let sessionID else {
            append(kind: .error, text: "No rust-agent session is available.")
            return
        }

        recordSubmittedMessage(command)
        draft = ""
        isRunningTerminalCommand = true
        append(kind: .user, text: "$ \(command)")
        let rootURL = workspace.url
        terminalTask = Task {
            do {
                try await applySelectedPermissionsForNextTurn(
                    client: client,
                    sessionID: sessionID
                )
                let result = try await client.runTerminalCommand(
                    sessionID: sessionID,
                    command: command
                )
                appendTerminalResult(result)
                try? await refreshWorkspace(client: client, fallbackURL: rootURL)
            } catch {
                append(kind: .error, text: error.localizedDescription)
            }
            isRunningTerminalCommand = false
            terminalTask = nil
        }
    }

    private func restoreSession(_ targetSessionID: UInt64) {
        guard !isClearingContext else {
            append(kind: .status, text: "context clear already running")
            return
        }
        guard !isCompactingContext else {
            append(kind: .status, text: "context compaction already running")
            return
        }
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
                try await startSessionEventStream(
                    sessionID: restored.sessionID,
                    client: client
                )
                append(kind: .status, text: "restored session \(restored.sessionID)")
            } catch {
                append(kind: .error, text: error.localizedDescription)
            }
            isSending = false
            streamTask = nil
        }
    }

    private func appendTerminalResult(_ result: TerminalCommandResponse) {
        let output = result.output.isEmpty ? "terminal command completed" : result.output
        if result.success {
            append(kind: .status, text: output)
        } else {
            append(kind: .error, text: output)
        }
    }

    func startLogin() {
        guard !isCompactingContext else {
            append(kind: .status, text: "context compaction already running")
            return
        }
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
                isLoggedIn = true
                append(kind: .status, text: "login complete")
                try await createFreshSession()
            } catch {
                append(kind: .error, text: error.localizedDescription)
            }
            isLoggingIn = false
        }
    }

    private func handle(_ event: AgentServerEvent) {
        if let eventSessionID = event.sessionID, eventSessionID != sessionID {
            return
        }

        switch event {
        case .serverConnected, .serverHeartbeat:
            break
        case let .eventsLagged(_, skipped):
            append(kind: .error, text: "missed \(skipped ?? 0) session events")
        case let .statusChanged(_, status):
            if status == "running", isSending {
                showAssistantPlaceholder("thinking...")
            }
        case let .textDelta(_, delta):
            appendAssistantDelta(delta ?? "")
        case let .messageCompleted(_, role, text):
            guard role == "assistant" else { return }
            replaceAssistantText(text ?? "")
            attachPendingCacheStats()
            currentTurnFinalAssistantLineID = currentAssistantLineID
            currentAssistantLineID = nil
        case let .toolCallStarted(_, toolCallID, toolName, toolArguments):
            markCurrentTurnToolActivity()
            upsertToolLine(
                callID: toolCallID,
                display: toolDisplay(
                    name: toolName,
                    arguments: toolArguments,
                    status: .running
                )
            )
        case let .toolCallCompleted(_, toolCallID, toolName, toolArguments, success):
            markCurrentTurnToolActivity()
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
            if let cache {
                pendingCacheStats.append(ResponseCacheStats(cache: cache))
            }
        case let .turnTokenUsage(_, usage):
            updateTokenUsage(usage)
        case .compactionStarted, .compactionCompleted:
            break
        case let .error(_, eventMessage):
            let message = eventMessage ?? "rust-agent reported an error."
            append(kind: .error, text: message)
            if message.contains("not logged in") {
                append(kind: .status, text: "type /login to authorize rust-agent")
            }
            if isSending {
                finishErroredTurn()
            }
        case let .turnCompleted(_, durationMS):
            finishTurn(durationMS: durationMS)
        case .turnCancelled:
            finishCancelledTurn()
        case .unknown:
            break
        }
    }

    private func finishTurn(durationMS: UInt64? = nil) {
        flushPendingAssistantDelta()
        markCurrentAssistantLineStreaming(false)
        attachResponseDuration(durationMS)
        isSending = false
        canCancelTurn = false
        streamTask = nil
        currentAssistantLineID = nil
        assistantPlaceholderLineID = nil
        currentTurnFinalAssistantLineID = nil
        currentTurnHadToolActivity = false
        pendingCacheStats.removeAll(keepingCapacity: true)
    }

    private func finishCancelledTurn() {
        let wasCancellable = canCancelTurn
        flushPendingAssistantDelta()
        markCurrentAssistantLineStreaming(false)
        isSending = false
        canCancelTurn = false
        streamTask = nil
        removeAssistantPlaceholder()
        currentAssistantLineID = nil
        assistantPlaceholderLineID = nil
        currentTurnFinalAssistantLineID = nil
        currentTurnHadToolActivity = false
        pendingCacheStats.removeAll(keepingCapacity: true)
        if wasCancellable {
            append(kind: .status, text: "turn cancelled")
        }
    }

    private func finishErroredTurn() {
        flushPendingAssistantDelta()
        markCurrentAssistantLineStreaming(false)
        isSending = false
        canCancelTurn = false
        streamTask = nil
        removeAssistantPlaceholder()
        currentAssistantLineID = nil
        assistantPlaceholderLineID = nil
        currentTurnFinalAssistantLineID = nil
        currentTurnHadToolActivity = false
        pendingCacheStats.removeAll(keepingCapacity: true)
    }

    private func failTurn(_ error: Error) {
        flushPendingAssistantDelta()
        markCurrentAssistantLineStreaming(false)
        isSending = false
        canCancelTurn = false
        streamTask = nil
        removeAssistantPlaceholder()
        currentAssistantLineID = nil
        assistantPlaceholderLineID = nil
        currentTurnFinalAssistantLineID = nil
        currentTurnHadToolActivity = false
        pendingCacheStats.removeAll(keepingCapacity: true)
        append(kind: .error, text: error.localizedDescription)
    }

    private static func isCancellation(_ error: Error) -> Bool {
        if error is CancellationError {
            return true
        }
        return (error as? URLError)?.code == .cancelled
    }

    private func appendAssistantDelta(_ delta: String) {
        guard !delta.isEmpty else { return }
        _ = ensureAssistantLine()

        pendingAssistantDisplayFragment.append(delta)
        if pendingAssistantDisplayFragment.utf8.count
            >= Self.assistantDisplayImmediateFlushCharacters {
            flushPendingAssistantDelta()
        } else {
            scheduleAssistantDisplayFlush()
        }
    }

    private func flushPendingAssistantDelta() {
        assistantDisplayFlushTask?.cancel()
        assistantDisplayFlushTask = nil
        guard !pendingAssistantDisplayFragment.isEmpty else { return }
        let delta = pendingAssistantDisplayFragment
        pendingAssistantDisplayFragment = ""
        applyAssistantDelta(delta)
    }

    private func clearPendingAssistantDelta() {
        assistantDisplayFlushTask?.cancel()
        assistantDisplayFlushTask = nil
        pendingAssistantDisplayFragment = ""
    }

    private func applyAssistantDelta(_ delta: String) {
        let id = ensureAssistantLine()
        guard let index = lineIndex(for: id) else { return }
        if assistantPlaceholderLineID == id {
            resetAssistantStreamContent()
        } else if isAssistantStreamContentEmpty {
            appendToAssistantStream(lines[index].text)
        }
        assistantPlaceholderLineID = nil
        appendToAssistantStream(delta)
        publishAssistantStream(lineID: id)
        if !lines[index].isStreaming {
            lines[index].isStreaming = true
        }
        refreshSearchMatches()
        requestStreamingScrollTo(id)
    }

    private func scheduleAssistantDisplayFlush() {
        guard assistantDisplayFlushTask == nil else { return }
        assistantDisplayFlushTask = Task { [weak self] in
            do {
                try await Task.sleep(nanoseconds: Self.assistantDisplayFlushNanoseconds)
            } catch {
                return
            }
            await MainActor.run {
                guard let self else { return }
                self.assistantDisplayFlushTask = nil
                self.flushPendingAssistantDelta()
            }
        }
    }

    private var isAssistantStreamContentEmpty: Bool {
        assistantStreamChunks.isEmpty && assistantStreamTail.isEmpty
    }

    private func resetAssistantStreamBuffers() {
        clearPendingAssistantDelta()
        resetAssistantStreamContent()
    }

    private func resetAssistantStreamContent() {
        assistantStreamChunks.removeAll(keepingCapacity: true)
        assistantStreamTail = ""
        nextAssistantStreamChunkID = 0
    }

    private func appendToAssistantStream(_ text: String) {
        guard !text.isEmpty else { return }
        var remainder = text[...]

        while let newline = remainder.firstIndex(of: "\n") {
            assistantStreamTail += String(remainder[..<newline])
            assistantStreamChunks.append(StreamingTextChunk(
                id: nextAssistantStreamChunkID,
                text: assistantStreamTail
            ))
            nextAssistantStreamChunkID += 1
            assistantStreamTail = ""
            remainder = remainder[remainder.index(after: newline)...]
        }

        assistantStreamTail += String(remainder)
    }

    private func publishAssistantStream(lineID: UUID) {
        activeAssistantStream = ActiveAssistantStream(
            lineID: lineID,
            chunks: assistantStreamChunks,
            tail: assistantStreamTail
        )
    }

    private func replaceAssistantText(_ text: String) {
        clearPendingAssistantDelta()
        let id = ensureAssistantLine()
        guard let index = lineIndex(for: id) else { return }
        lines[index].text = text
        lines[index].renderedMarkdown = nil
        assistantPlaceholderLineID = nil
        resetAssistantStreamContent()
        appendToAssistantStream(text)
        publishAssistantStream(lineID: id)
        if !lines[index].isStreaming {
            lines[index].isStreaming = true
        }
        scheduleMarkdownRendering(MarkdownRenderSnapshot(
            lineID: id,
            text: text,
            finalizesStreamingLine: true
        ))
        refreshSearchMatches()
        requestScrollTo(id)
    }

    private func attachPendingCacheStats() {
        guard !pendingCacheStats.isEmpty,
              let currentAssistantLineID,
              let index = lineIndex(for: currentAssistantLineID) else {
            return
        }
        lines[index].cacheStats.append(contentsOf: pendingCacheStats)
        pendingCacheStats.removeAll(keepingCapacity: true)
    }

    private func markCurrentTurnToolActivity() {
        guard isSending else { return }
        currentTurnHadToolActivity = true
    }

    private func attachResponseDuration(_ durationMS: UInt64?) {
        guard currentTurnHadToolActivity,
              let durationMS,
              let currentTurnFinalAssistantLineID,
              let index = lineIndex(for: currentTurnFinalAssistantLineID),
              lines[index].kind == .assistant else {
            return
        }
        lines[index].responseDurationMS = durationMS
        requestScrollTo(currentTurnFinalAssistantLineID)
    }

    private func showAssistantPlaceholder(_ text: String) {
        let id = ensureAssistantLine()
        guard let index = lineIndex(for: id) else { return }
        let visibleText = activeAssistantStream?.lineID == id
            ? activeAssistantStream?.text ?? ""
            : lines[index].text
        guard assistantPlaceholderLineID == id || visibleText.isEmpty else { return }
        activeAssistantStream = ActiveAssistantStream(lineID: id, chunks: [], tail: text)
        resetAssistantStreamContent()
        if !lines[index].isStreaming {
            lines[index].isStreaming = true
        }
        assistantPlaceholderLineID = id
        refreshSearchMatches()
        requestScrollTo(id)
    }

    private func removeAssistantPlaceholder() {
        guard let assistantPlaceholderLineID,
              let index = lineIndex(for: assistantPlaceholderLineID) else {
            return
        }
        if activeAssistantStream?.lineID == assistantPlaceholderLineID {
            activeAssistantStream = nil
        }
        resetAssistantStreamContent()
        removeLine(at: index)
        self.assistantPlaceholderLineID = nil
        refreshSearchMatches()
        requestScrollToLastLine()
    }

    private func ensureAssistantLine() -> UUID {
        if let currentAssistantLineID {
            return currentAssistantLineID
        }

        let line = TranscriptLine(kind: .assistant, text: "", isStreaming: isSending)
        appendLine(line)
        currentAssistantLineID = line.id
        requestScrollTo(line.id)
        return line.id
    }

    private func markCurrentAssistantLineStreaming(_ isStreaming: Bool) {
        guard let currentAssistantLineID,
              let index = lineIndex(for: currentAssistantLineID) else {
            return
        }
        if !isStreaming,
           activeAssistantStream?.lineID == currentAssistantLineID {
            lines[index].text = activeAssistantStream?.text ?? lines[index].text
            activeAssistantStream = nil
        }
        if !isStreaming {
            resetAssistantStreamContent()
        }
        lines[index].isStreaming = isStreaming
        if isStreaming {
            lines[index].renderedMarkdown = nil
        } else if lines[index].kind == .assistant {
            lines[index].renderedMarkdown = nil
            scheduleMarkdownRendering(for: lines[index])
        }
    }

    private func append(kind: TranscriptKind, text: String) {
        let line = TranscriptLine(kind: kind, text: text)
        appendLine(line)
        refreshSearchMatches()
        requestScrollTo(line.id)
    }

    private func lineIndex(for id: UUID) -> Int? {
        if let index = lineIndexesByID[id],
           lines.indices.contains(index),
           lines[index].id == id {
            return index
        }

        guard let index = lines.firstIndex(where: { $0.id == id }) else {
            lineIndexesByID.removeValue(forKey: id)
            return nil
        }
        lineIndexesByID[id] = index
        return index
    }

    private func appendLine(_ line: TranscriptLine) {
        lines.append(line)
        lineIndexesByID[line.id] = lines.count - 1
    }

    private func insertLine(_ line: TranscriptLine, at index: Int) {
        lines.insert(line, at: index)
        rebuildLineIndexes()
    }

    private func removeLine(at index: Int) {
        lineIndexesByID.removeValue(forKey: lines[index].id)
        lines.remove(at: index)
        rebuildLineIndexes()
    }

    private func replaceTranscriptLines(_ newLines: [TranscriptLine]) {
        lines = newLines
        rebuildLineIndexes()
    }

    private func rebuildLineIndexes() {
        lineIndexesByID.removeAll(keepingCapacity: true)
        for (index, line) in lines.enumerated() {
            lineIndexesByID[line.id] = index
        }
    }

    private func requestScrollToLastLine() {
        guard let id = lines.last?.id else { return }
        requestScrollTo(id)
    }

    private func requestScrollTo(_ id: UUID) {
        transcriptScrollRevision += 1
        transcriptScrollTarget = TranscriptScrollTarget(
            lineID: id,
            revision: transcriptScrollRevision
        )
    }

    private func requestStreamingScrollTo(_ id: UUID) {
        let now = Date()
        guard now.timeIntervalSince(lastAssistantScrollRequest)
            >= Self.assistantScrollThrottleSeconds else {
            return
        }
        lastAssistantScrollRequest = now
        requestScrollTo(id)
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
           let index = lineIndex(for: lineID) {
            lines[index].text = text
            lines[index].toolCall = display
            refreshSearchMatches()
            requestScrollTo(lineID)
            return
        }

        let lineID = insertToolLine(text, display: display)
        toolLineIDsByCallID[callID] = lineID
    }

    @discardableResult
    private func insertToolLine(_ text: String, display: ToolCallTranscript? = nil) -> UUID {
        flushPendingAssistantDelta()
        removePlaceholderBeforeToolLine()

        let line = TranscriptLine(kind: .tool, text: text, toolCall: display)
        let lineID = line.id

        if let currentAssistantLineID,
           let index = lineIndex(for: currentAssistantLineID) {
            insertLine(line, at: index + 1)
            refreshSearchMatches()
            requestScrollTo(lineID)
            return lineID
        }

        appendLine(line)
        refreshSearchMatches()
        requestScrollTo(lineID)
        return lineID
    }

    private func removePlaceholderBeforeToolLine() {
        guard let assistantPlaceholderLineID else { return }
        guard let index = lineIndex(for: assistantPlaceholderLineID) else {
            if activeAssistantStream?.lineID == assistantPlaceholderLineID {
                activeAssistantStream = nil
            }
            resetAssistantStreamContent()
            self.assistantPlaceholderLineID = nil
            if currentAssistantLineID == assistantPlaceholderLineID {
                currentAssistantLineID = nil
            }
            return
        }

        if activeAssistantStream?.lineID == assistantPlaceholderLineID {
            activeAssistantStream = nil
        }
        resetAssistantStreamContent()
        removeLine(at: index)
        self.assistantPlaceholderLineID = nil
        if currentAssistantLineID == assistantPlaceholderLineID {
            currentAssistantLineID = nil
        }
    }

    private func applyRestoredSession(_ restored: RestoreSessionResponse) {
        sessionID = restored.sessionID
        applySessionModel(restored.model)
        applySessionPermissions(restored.toolPolicy)
        resetActiveTranscriptState()
        let restoredLines = transcriptLines(from: restored.messages)
        replaceTranscriptLines(restoredLines)
        scheduleMarkdownRendering(for: restoredLines)
        replaceSubmittedMessages(with: restored.messages)
        refreshSearchMatches()
        requestScrollToLastLine()
    }

    private func resetActiveTranscriptState() {
        currentAssistantLineID = nil
        assistantPlaceholderLineID = nil
        currentTurnFinalAssistantLineID = nil
        currentTurnHadToolActivity = false
        activeAssistantStream = nil
        resetAssistantStreamBuffers()
        pendingCacheStats.removeAll(keepingCapacity: true)
        toolLineIDsByCallID.removeAll(keepingCapacity: true)
        toolDisplaysByCallID.removeAll(keepingCapacity: true)
        pendingMarkdownRenders.removeAll(keepingCapacity: true)
        markdownRenderTask?.cancel()
        markdownRenderTask = nil
    }

    private func transcriptLines(from records: [TranscriptRecord]) -> [TranscriptLine] {
        var restoredLines: [TranscriptLine] = []
        var lineIndexesByCallID: [String: Int] = [:]
        var displaysByCallID: [String: ToolCallTranscript] = [:]

        for record in records {
            switch record.kind {
            case "message":
                guard let kind = transcriptKind(forRole: record.role) else { continue }
                let text = record.text ?? ""
                restoredLines.append(TranscriptLine(
                    kind: kind,
                    text: text
                ))
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

    private func scheduleMarkdownRendering(for line: TranscriptLine) {
        scheduleMarkdownRendering(for: [line])
    }

    private func scheduleMarkdownRendering(for lines: [TranscriptLine]) {
        let snapshots = lines.compactMap { line -> MarkdownRenderSnapshot? in
            guard line.kind == .assistant, !line.text.isEmpty else { return nil }
            return MarkdownRenderSnapshot(
                lineID: line.id,
                text: line.text,
                finalizesStreamingLine: false
            )
        }
        scheduleMarkdownRendering(snapshots)
    }

    private func scheduleMarkdownRendering(_ snapshot: MarkdownRenderSnapshot) {
        scheduleMarkdownRendering([snapshot])
    }

    private func scheduleMarkdownRendering(_ snapshots: [MarkdownRenderSnapshot]) {
        guard !snapshots.isEmpty else { return }
        for snapshot in snapshots {
            pendingMarkdownRenders[snapshot.lineID] = snapshot
        }
        startMarkdownRenderTaskIfNeeded()
    }

    private func startMarkdownRenderTaskIfNeeded() {
        guard markdownRenderTask == nil, !pendingMarkdownRenders.isEmpty else { return }
        let snapshots = Array(pendingMarkdownRenders.values)
        pendingMarkdownRenders.removeAll(keepingCapacity: true)

        markdownRenderTask = Task { [weak self] in
            let assignments = await Task.detached(priority: .utility) {
                snapshots.map { snapshot in
                    MarkdownRenderAssignment(
                        lineID: snapshot.lineID,
                        text: snapshot.text,
                        finalizesStreamingLine: snapshot.finalizesStreamingLine,
                        markdown: RenderedTerminalMarkdown(text: snapshot.text)
                    )
                }
            }.value
            guard !Task.isCancelled else { return }
            self?.finishMarkdownRendering(assignments)
        }
    }

    private func finishMarkdownRendering(_ assignments: [MarkdownRenderAssignment]) {
        for assignment in assignments {
            guard let index = lineIndex(for: assignment.lineID),
                  lines[index].kind == .assistant,
                  lines[index].text == assignment.text else {
                continue
            }
            if assignment.finalizesStreamingLine {
                lines[index].renderedMarkdown = assignment.markdown
                lines[index].isStreaming = false
                if activeAssistantStream?.lineID == assignment.lineID {
                    activeAssistantStream = nil
                    resetAssistantStreamContent()
                }
            } else if !lines[index].isStreaming {
                lines[index].renderedMarkdown = assignment.markdown
            }
        }

        markdownRenderTask = nil
        startMarkdownRenderTaskIfNeeded()
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

    private func startSessionEventStream(
        sessionID: UInt64,
        client: any AgentClientProtocol
    ) async throws {
        sessionEventTask?.cancel()
        sessionEventTask = nil
        isSessionEventStreamConnected = false

        let startup = SessionEventStreamStartup()
        sessionEventTask = Task { [weak self] in
            do {
                try await client.streamSessionEvents(sessionID: sessionID) { event in
                    await MainActor.run {
                        guard let self, self.sessionID == sessionID else { return }
                        if !self.isSessionEventStreamConnected {
                            self.isSessionEventStreamConnected = true
                            startup.succeed()
                        }
                        self.handle(event)
                    }
                }
                startup.fail(SessionEventStreamError.disconnected)
                await MainActor.run {
                    guard let self, self.sessionID == sessionID else { return }
                    self.isSessionEventStreamConnected = false
                    self.isReady = false
                    if self.isSending {
                        self.finishErroredTurn()
                    }
                    self.append(kind: .error, text: "session event stream disconnected")
                }
            } catch {
                if Self.isCancellation(error) || Task.isCancelled {
                    startup.fail(CancellationError())
                    return
                }
                startup.fail(error)
                await MainActor.run {
                    guard let self, self.sessionID == sessionID else { return }
                    self.isSessionEventStreamConnected = false
                    self.isReady = false
                    if self.isSending {
                        self.finishErroredTurn()
                    }
                    self.append(kind: .error, text: error.localizedDescription)
                }
            }
        }

        try await startup.wait()
    }

    private func refreshAuthStatus() async {
        switch await auth.status() {
        case .loggedIn:
            isLoggedIn = true
        case .loggedOut:
            isLoggedIn = false
            append(kind: .status, text: "not logged in. type /login to authorize rust-agent")
        case let .unknown(message):
            append(kind: .error, text: message)
        }
    }

    private func refreshSearchMatches(debounce: Bool = true) {
        searchRefreshRevision += 1
        searchRefreshTask?.cancel()
        searchRefreshTask = nil

        guard isSearchVisible else {
            clearSearchMatchesIfNeeded()
            return
        }

        let query = searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else {
            clearSearchMatchesIfNeeded()
            return
        }

        let snapshots = searchLineSnapshots()
        let preferredLineID = selectedSearchLineID
        let revision = searchRefreshRevision
        searchRefreshTask = Task { [weak self] in
            if debounce {
                do {
                    try await Task.sleep(nanoseconds: Self.searchRefreshDebounceNanoseconds)
                } catch {
                    return
                }
            }

            let matches = await Task.detached(priority: .utility) {
                Self.searchMatches(in: snapshots, query: query)
            }.value
            guard !Task.isCancelled else { return }
            self?.applySearchMatches(
                matches,
                preferredLineID: preferredLineID,
                revision: revision
            )
        }
    }

    private func applySearchMatches(
        _ matches: [UUID],
        preferredLineID: UUID?,
        revision: Int
    ) {
        guard revision == searchRefreshRevision else { return }
        searchRefreshTask = nil
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

    private func searchLineSnapshots() -> [SearchLineSnapshot] {
        lines.map { line in
            SearchLineSnapshot(id: line.id, text: searchableText(for: line))
        }
    }

    nonisolated private static func searchMatches(
        in snapshots: [SearchLineSnapshot],
        query: String
    ) -> [UUID] {
        snapshots.compactMap { snapshot -> UUID? in
            snapshot.text.range(
                of: query,
                options: [.caseInsensitive, .diacriticInsensitive]
            ) == nil ? nil : snapshot.id
        }
    }

    private func clearSearchMatchesIfNeeded() {
        guard !searchMatchedLineIDsInOrder.isEmpty
            || !searchMatchLineIDs.isEmpty
            || selectedSearchLineID != nil
            || !searchResultSummary.isEmpty else {
            return
        }
        searchMatchedLineIDsInOrder.removeAll(keepingCapacity: true)
        searchMatchLineIDs.removeAll(keepingCapacity: true)
        selectedSearchLineID = nil
        searchResultSummary = ""
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

    private func searchableText(for line: TranscriptLine) -> String {
        if activeAssistantStream?.lineID == line.id {
            return activeAssistantStream?.text ?? line.text
        }
        return line.text
    }

    private func createFreshSession() async throws {
        guard let client else { return }
        let session = try await client.createSession()
        sessionID = session.sessionID
        applySessionModel(session.model)
        applySessionPermissions(session.toolPolicy ?? selectedPermission)
        replaceSubmittedMessages(with: [])
        try await startSessionEventStream(sessionID: session.sessionID, client: client)
        append(kind: .status, text: "Session ID: \(session.sessionID)")
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

    private func compactionStatus(_ response: CompactSessionResponse) -> String {
        "compaction complete. \(compactNumber(response.tokensBefore)) tokens before compaction"
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
        draftSelectionLocation = value.utf16.count
        isApplyingDraftFromHistory = false
    }

    private func resetDraftHistoryNavigation() {
        submittedMessageHistory.reset()
    }
}

private struct MarkdownRenderSnapshot: Sendable {
    let lineID: UUID
    let text: String
    let finalizesStreamingLine: Bool
}

private struct MarkdownRenderAssignment: Sendable {
    let lineID: UUID
    let text: String
    let finalizesStreamingLine: Bool
    let markdown: RenderedTerminalMarkdown
}

private struct SearchLineSnapshot: Sendable {
    let id: UUID
    let text: String
}

private enum SessionEventStreamError: LocalizedError {
    case disconnected

    var errorDescription: String? {
        "session event stream disconnected"
    }
}

private final class SessionEventStreamStartup {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<Void, Error>?
    private var result: Result<Void, Error>?

    func wait() async throws {
        try await withCheckedThrowingContinuation { continuation in
            var resolved: Result<Void, Error>?
            lock.lock()
            if let result {
                resolved = result
            } else {
                self.continuation = continuation
            }
            lock.unlock()

            if let resolved {
                continuation.resume(with: resolved)
            }
        }
    }

    func succeed() {
        complete(.success(()))
    }

    func fail(_ error: Error) {
        complete(.failure(error))
    }

    private func complete(_ result: Result<Void, Error>) {
        let continuation: CheckedContinuation<Void, Error>?
        lock.lock()
        if self.result == nil {
            self.result = result
            continuation = self.continuation
            self.continuation = nil
        } else {
            continuation = nil
        }
        lock.unlock()

        continuation?.resume(with: result)
    }
}
