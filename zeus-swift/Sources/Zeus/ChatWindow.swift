import AppKit
import SwiftUI

private enum TerminalLayout {
    static let markerWidth: CGFloat = 10
    static let markerTextSpacing: CGFloat = 8
}

private enum FooterMenuID {
    case branch
    case model
    case effort
    case permissions
    case shortcuts
}

private struct ShortcutItem: Identifiable {
    let shortcut: String
    let action: String

    var id: String {
        shortcut
    }
}

private let shortcutItems = [
    ShortcutItem(shortcut: "Cmd+N", action: "Open a new Zeus window"),
    ShortcutItem(shortcut: "Cmd+B", action: "Open branch menu"),
    ShortcutItem(shortcut: "Cmd+M", action: "Open model menu"),
    ShortcutItem(shortcut: "Cmd+E", action: "Open effort menu"),
    ShortcutItem(shortcut: "Cmd+P", action: "Open permissions menu"),
    ShortcutItem(shortcut: "Cmd+F", action: "Open transcript search"),
    ShortcutItem(shortcut: "Cmd+G", action: "Next search match"),
    ShortcutItem(shortcut: "Cmd+Shift+G", action: "Previous search match"),
    ShortcutItem(shortcut: "Cmd+T", action: "Toggle terminal passthrough"),
    ShortcutItem(shortcut: "Ctrl+C", action: "Clear input"),
    ShortcutItem(shortcut: "Ctrl+Enter", action: "Insert newline"),
    ShortcutItem(shortcut: "Up Arrow", action: "Previous message, open footer menu, or previous option"),
    ShortcutItem(shortcut: "Down Arrow", action: "Next message, next option, or close menu at bottom"),
    ShortcutItem(shortcut: "Left / Right Arrow", action: "Move between footer controls"),
    ShortcutItem(shortcut: "Return / Enter", action: "Activate footer control or menu option"),
    ShortcutItem(shortcut: "Esc", action: "Cancel response, close search, or close footer UI")
]

private enum KeyCode {
    static let returnKey: UInt16 = 36
    static let escape: UInt16 = 53
    static let keypadEnter: UInt16 = 76
    static let downArrow: UInt16 = 125
    static let upArrow: UInt16 = 126
    static let leftArrow: UInt16 = 123
    static let rightArrow: UInt16 = 124
}

struct ChatWindow: View {
    @ObservedObject var viewModel: ChatViewModel
    @State private var activeFooterMenu: FooterMenuID?
    @State private var focusedFooterMenu: FooterMenuID?
    @State private var branchMenuHighlightedOption: String?
    @State private var modelMenuHighlightedOption: String?
    @State private var effortMenuHighlightedOption: String?
    @State private var permissionsMenuHighlightedOption: String?

    var body: some View {
        ZStack {
            TerminalBackground()

            VStack(spacing: 0) {
                HeaderBar(onLoginStatus: viewModel.showLoginStatus)

                if viewModel.isSearchVisible {
                    SearchBar(
                        text: Binding(
                            get: { viewModel.searchQuery },
                            set: viewModel.setSearchQuery
                        ),
                        resultSummary: viewModel.searchResultSummary,
                        onPrevious: viewModel.selectPreviousSearchMatch,
                        onNext: viewModel.selectNextSearchMatch,
                        onClose: viewModel.closeSearch
                    )
                    .padding(.top, 8)
                }

                TranscriptView(
                    lines: viewModel.lines,
                    isCacheStatsVisible: viewModel.showCacheStats,
                    searchMatchLineIDs: viewModel.searchMatchLineIDs,
                    selectedSearchLineID: viewModel.selectedSearchLineID,
                    scrollTarget: viewModel.transcriptScrollTarget
                )
                    .padding(.top, 10)

                InputPrompt(
                    text: $viewModel.draft,
                    prompt: viewModel.inputPrompt,
                    placeholder: viewModel.inputPlaceholder,
                    onSubmit: viewModel.sendDraft,
                    onHistoryPrevious: viewModel.selectPreviousSubmittedMessage,
                    onHistoryNext: viewModel.selectNextSubmittedMessage,
                    onMoveDownFromCurrent: focusFirstFooterMenu,
                    isCancelVisible: viewModel.canCancelTurn,
                    onCancel: viewModel.cancelCurrentTurn
                )
                .padding(.top, 8)

                FooterBar(
                    workspace: viewModel.workspace,
                    branchOptions: viewModel.branchOptions,
                    selectedBranch: viewModel.workspace.branch,
                    isBranchMenuEnabled: viewModel.canChangeBranch,
                    model: viewModel.model,
                    modelOptions: viewModel.modelOptions,
                    selectedModel: viewModel.selectedModel,
                    isModelMenuEnabled: viewModel.canChangeModel,
                    effort: viewModel.effort,
                    effortOptions: viewModel.effortOptions,
                    isEffortMenuEnabled: viewModel.canChangeEffort,
                    permissions: viewModel.permissions,
                    permissionOptions: viewModel.permissionOptions,
                    selectedPermission: viewModel.selectedPermission,
                    isPermissionsMenuEnabled: viewModel.canChangePermissions,
                    tokenUsage: viewModel.tokenUsage,
                    activeMenu: $activeFooterMenu,
                    focusedMenu: $focusedFooterMenu,
                    branchHighlightedOption: branchMenuHighlightedOption,
                    modelHighlightedOption: modelMenuHighlightedOption,
                    effortHighlightedOption: effortMenuHighlightedOption,
                    permissionsHighlightedOption: permissionsMenuHighlightedOption,
                    modelTitle: { viewModel.displayModel($0) },
                    permissionTitle: { viewModel.displayPermission($0) },
                    onSelectBranch: viewModel.selectBranch,
                    onSelectModel: viewModel.selectModel,
                    onSelectEffort: viewModel.selectEffort,
                    onSelectPermissions: viewModel.selectPermissions,
                    onHighlightBranch: { branchMenuHighlightedOption = $0 },
                    onHighlightModel: { modelMenuHighlightedOption = $0 },
                    onHighlightEffort: { effortMenuHighlightedOption = $0 },
                    onHighlightPermissions: { permissionsMenuHighlightedOption = $0 }
                )
                .padding(.top, 11)
            }
            .padding(.horizontal, 19)
            .padding(.top, 10)
            .padding(.bottom, 11)
        }
        .ignoresSafeArea(.container, edges: .top)
        .background(WindowConfigurator())
        .background(LocalEventMonitor(onEvent: handleLocalEvent(_:)))
        .font(.system(size: 12, weight: .regular, design: .monospaced))
        .foregroundStyle(TerminalPalette.primaryText)
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in
            viewModel.shutdown()
        }
        .onDisappear {
            viewModel.shutdown()
        }
    }

    private func handleLocalEvent(_ event: NSEvent) -> Bool {
        switch event.type {
        case .keyDown:
            return handleKeyDown(event)
        case .leftMouseUp, .rightMouseUp:
            if activeFooterMenu != nil || focusedFooterMenu != nil {
                DispatchQueue.main.async {
                    activeFooterMenu = nil
                    focusedFooterMenu = nil
                }
            }
            return false
        default:
            return false
        }
    }

    private func handleKeyDown(_ event: NSEvent) -> Bool {
        if event.keyCode == KeyCode.escape, viewModel.canCancelTurn {
            viewModel.cancelCurrentTurn()
            return true
        }
        if isSearchShortcut(event) {
            activeFooterMenu = nil
            viewModel.showSearch()
            return true
        }
        if isSearchNextShortcut(event) {
            viewModel.selectNextSearchMatch()
            return true
        }
        if isSearchPreviousShortcut(event) {
            viewModel.selectPreviousSearchMatch()
            return true
        }
        if event.keyCode == KeyCode.escape, viewModel.isSearchVisible {
            viewModel.closeSearch()
            return true
        }
        if isTerminalPassthroughShortcut(event) {
            viewModel.toggleTerminalPassthrough()
            return true
        }
        if isClearInputShortcut(event) {
            viewModel.clearDraft()
            return true
        }
        if isBranchShortcut(event) {
            openBranchMenu()
            return true
        }
        if isModelShortcut(event) {
            openModelMenu()
            return true
        }
        if isEffortShortcut(event) {
            openEffortMenu()
            return true
        }
        if isPermissionsShortcut(event) {
            openPermissionsMenu()
            return true
        }
        if focusedFooterMenu != nil || activeFooterMenu != nil {
            if handleFooterNavigationKey(event) {
                return true
            }
            focusedFooterMenu = nil
            activeFooterMenu = nil
            return false
        }

        return false
    }

    private func handleFooterNavigationKey(_ event: NSEvent) -> Bool {
        guard hasNoKeyModifiers(event) else { return false }

        if focusedFooterMenu == nil {
            focusedFooterMenu = activeFooterMenu ?? footerNavigationMenus.first
        }

        guard activeFooterMenu != nil else {
            return handleFocusedFooterKey(event)
        }

        return handleOpenFooterMenuKey(event)
    }

    private func handleFocusedFooterKey(_ event: NSEvent) -> Bool {
        switch event.keyCode {
        case KeyCode.escape:
            focusedFooterMenu = nil
            return true
        case KeyCode.returnKey, KeyCode.keypadEnter:
            openFocusedFooterMenu()
            return true
        case KeyCode.upArrow:
            openFocusedFooterMenu()
            return true
        case KeyCode.downArrow:
            focusedFooterMenu = nil
            return true
        case KeyCode.rightArrow:
            moveFocusedFooterMenu(by: 1)
            return true
        case KeyCode.leftArrow:
            moveFocusedFooterMenu(by: -1)
            return true
        default:
            return false
        }
    }

    private func handleOpenFooterMenuKey(_ event: NSEvent) -> Bool {
        switch event.keyCode {
        case KeyCode.escape:
            activeFooterMenu = nil
            return true
        case KeyCode.returnKey, KeyCode.keypadEnter:
            return selectActiveMenuOption()
        case KeyCode.downArrow:
            if isActiveMenuHighlightAtLastOption() {
                activeFooterMenu = nil
            } else {
                moveActiveMenuHighlight(by: 1)
            }
            return true
        case KeyCode.upArrow:
            moveActiveMenuHighlight(by: -1)
            return true
        case KeyCode.rightArrow:
            moveFocusedFooterMenu(by: 1)
            return true
        case KeyCode.leftArrow:
            moveFocusedFooterMenu(by: -1)
            return true
        default:
            return false
        }
    }

    private func hasNoKeyModifiers(_ event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let disallowed: NSEvent.ModifierFlags = [.command, .control, .option, .shift]
        return flags.intersection(disallowed).isEmpty
    }

    private func isModelShortcut(_ event: NSEvent) -> Bool {
        isCommandShortcut(event, key: "m")
    }

    private func isBranchShortcut(_ event: NSEvent) -> Bool {
        isCommandShortcut(event, key: "b")
    }

    private func isEffortShortcut(_ event: NSEvent) -> Bool {
        isCommandShortcut(event, key: "e")
    }

    private func isPermissionsShortcut(_ event: NSEvent) -> Bool {
        isCommandShortcut(event, key: "p")
    }

    private func isTerminalPassthroughShortcut(_ event: NSEvent) -> Bool {
        isCommandShortcut(event, key: "t")
    }

    private func isClearInputShortcut(_ event: NSEvent) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let disallowed: NSEvent.ModifierFlags = [.command, .option, .shift]
        return flags.contains(.control)
            && flags.intersection(disallowed).isEmpty
            && event.charactersIgnoringModifiers?.lowercased() == "c"
    }

    private func isSearchShortcut(_ event: NSEvent) -> Bool {
        isCommandShortcut(event, key: "f")
    }

    private func isSearchNextShortcut(_ event: NSEvent) -> Bool {
        isCommandShortcut(event, key: "g")
    }

    private func isSearchPreviousShortcut(_ event: NSEvent) -> Bool {
        isCommandShiftShortcut(event, key: "g")
    }

    private func isCommandShortcut(_ event: NSEvent, key: String) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let disallowed: NSEvent.ModifierFlags = [.control, .option, .shift]
        return flags.contains(.command)
            && flags.intersection(disallowed).isEmpty
            && event.charactersIgnoringModifiers?.lowercased() == key
    }

    private func isCommandShiftShortcut(_ event: NSEvent, key: String) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let disallowed: NSEvent.ModifierFlags = [.control, .option]
        return flags.contains(.command)
            && flags.contains(.shift)
            && flags.intersection(disallowed).isEmpty
            && event.charactersIgnoringModifiers?.lowercased() == key
    }

    private func openModelMenu() {
        guard viewModel.canChangeModel else { return }
        let options = modelMenuOptions
        modelMenuHighlightedOption = options.contains(viewModel.selectedModel)
            ? viewModel.selectedModel
            : options.first
        focusedFooterMenu = .model
        activeFooterMenu = .model
    }

    private func openBranchMenu() {
        guard viewModel.canChangeBranch else { return }
        let options = branchMenuOptions
        branchMenuHighlightedOption = options.contains(viewModel.workspace.branch)
            ? viewModel.workspace.branch
            : options.first
        focusedFooterMenu = .branch
        activeFooterMenu = .branch
    }

    private func openEffortMenu() {
        guard viewModel.canChangeEffort else { return }
        let options = effortMenuOptions
        effortMenuHighlightedOption = options.contains(viewModel.effort)
            ? viewModel.effort
            : options.first
        focusedFooterMenu = .effort
        activeFooterMenu = .effort
    }

    private func openPermissionsMenu() {
        guard viewModel.canChangePermissions else { return }
        let options = permissionsMenuOptions
        permissionsMenuHighlightedOption = options.contains(viewModel.selectedPermission)
            ? viewModel.selectedPermission
            : options.first
        focusedFooterMenu = .permissions
        activeFooterMenu = .permissions
    }

    private func focusFirstFooterMenu() {
        activeFooterMenu = nil
        focusedFooterMenu = footerNavigationMenus.first
    }

    private func openFocusedFooterMenu() {
        switch focusedFooterMenu {
        case .branch:
            openBranchMenu()
        case .model:
            openModelMenu()
        case .effort:
            openEffortMenu()
        case .permissions:
            openPermissionsMenu()
        case .shortcuts:
            activeFooterMenu = .shortcuts
        case nil:
            focusFirstFooterMenu()
        }
    }

    private func moveFocusedFooterMenu(by offset: Int) {
        let menus = footerNavigationMenus
        guard !menus.isEmpty else { return }
        let current = focusedFooterMenu
            .flatMap { menus.firstIndex(of: $0) }
            ?? 0
        let next = (current + offset + menus.count) % menus.count
        activeFooterMenu = nil
        focusedFooterMenu = menus[next]
    }

    private func moveActiveMenuHighlight(by offset: Int) {
        switch activeFooterMenu {
        case .branch:
            moveBranchHighlight(by: offset)
        case .model:
            moveModelHighlight(by: offset)
        case .effort:
            moveEffortHighlight(by: offset)
        case .permissions:
            movePermissionsHighlight(by: offset)
        case .shortcuts:
            break
        case nil:
            break
        }
    }

    private func selectActiveMenuOption() -> Bool {
        switch activeFooterMenu {
        case .branch:
            guard let branch = branchMenuHighlightedOption ?? branchMenuOptions.first else {
                return false
            }
            activeFooterMenu = nil
            focusedFooterMenu = nil
            viewModel.selectBranch(branch)
            return true
        case .model:
            guard let model = modelMenuHighlightedOption ?? modelMenuOptions.first else {
                return false
            }
            activeFooterMenu = nil
            focusedFooterMenu = nil
            viewModel.selectModel(model)
            return true
        case .effort:
            guard let effort = effortMenuHighlightedOption ?? effortMenuOptions.first else {
                return false
            }
            activeFooterMenu = nil
            focusedFooterMenu = nil
            viewModel.selectEffort(effort)
            return true
        case .permissions:
            guard let permissions = permissionsMenuHighlightedOption ?? permissionsMenuOptions.first else {
                return false
            }
            activeFooterMenu = nil
            focusedFooterMenu = nil
            viewModel.selectPermissions(permissions)
            return true
        case .shortcuts:
            activeFooterMenu = nil
            return true
        case nil:
            return false
        }
    }

    private func moveModelHighlight(by offset: Int) {
        let options = modelMenuOptions
        let current = modelMenuHighlightedOption ?? viewModel.selectedModel
        modelMenuHighlightedOption = nextMenuOption(in: options, current: current, offset: offset)
    }

    private func moveBranchHighlight(by offset: Int) {
        let options = branchMenuOptions
        let current = branchMenuHighlightedOption ?? viewModel.workspace.branch
        branchMenuHighlightedOption = nextMenuOption(in: options, current: current, offset: offset)
    }

    private func moveEffortHighlight(by offset: Int) {
        let options = effortMenuOptions
        let current = effortMenuHighlightedOption ?? viewModel.effort
        effortMenuHighlightedOption = nextMenuOption(in: options, current: current, offset: offset)
    }

    private func movePermissionsHighlight(by offset: Int) {
        let options = permissionsMenuOptions
        let current = permissionsMenuHighlightedOption ?? viewModel.selectedPermission
        permissionsMenuHighlightedOption = nextMenuOption(
            in: options,
            current: current,
            offset: offset
        )
    }

    private func isActiveMenuHighlightAtLastOption() -> Bool {
        switch activeFooterMenu {
        case .branch:
            isLastMenuOption(
                in: branchMenuOptions,
                current: branchMenuHighlightedOption ?? viewModel.workspace.branch
            )
        case .model:
            isLastMenuOption(
                in: modelMenuOptions,
                current: modelMenuHighlightedOption ?? viewModel.selectedModel
            )
        case .effort:
            isLastMenuOption(
                in: effortMenuOptions,
                current: effortMenuHighlightedOption ?? viewModel.effort
            )
        case .permissions:
            isLastMenuOption(
                in: permissionsMenuOptions,
                current: permissionsMenuHighlightedOption ?? viewModel.selectedPermission
            )
        case .shortcuts, nil:
            true
        }
    }

    private func isLastMenuOption(in options: [String], current: String) -> Bool {
        guard let last = options.last else { return true }
        return current == last
    }

    private func nextMenuOption(in options: [String], current: String, offset: Int) -> String? {
        guard !options.isEmpty else { return nil }
        let currentIndex = options.firstIndex(of: current) ?? 0
        let nextIndex = min(max(currentIndex + offset, 0), options.count - 1)
        return options[nextIndex]
    }

    private var modelMenuOptions: [String] {
        viewModel.modelOptions.isEmpty ? [viewModel.selectedModel] : viewModel.modelOptions
    }

    private var branchMenuOptions: [String] {
        viewModel.branchOptions.isEmpty ? [viewModel.workspace.branch] : viewModel.branchOptions
    }

    private var effortMenuOptions: [String] {
        viewModel.effortOptions.isEmpty ? [viewModel.effort] : viewModel.effortOptions
    }

    private var permissionsMenuOptions: [String] {
        viewModel.permissionOptions.isEmpty ? [viewModel.selectedPermission] : viewModel.permissionOptions
    }

    private var footerNavigationMenus: [FooterMenuID] {
        var menus: [FooterMenuID] = []
        if viewModel.canChangeBranch {
            menus.append(.branch)
        }
        if viewModel.canChangeModel {
            menus.append(.model)
        }
        if viewModel.canChangeEffort {
            menus.append(.effort)
        }
        if viewModel.canChangePermissions {
            menus.append(.permissions)
        }
        menus.append(.shortcuts)
        return menus
    }
}

private struct TerminalBackground: View {
    var body: some View {
        ZStack {
            TerminalPalette.background
            LinearGradient(
                colors: [
                    TerminalPalette.backgroundHighlight.opacity(0.75),
                    TerminalPalette.backgroundLow
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
            Rectangle()
                .strokeBorder(TerminalPalette.border.opacity(0.24), lineWidth: 1)
        }
        .ignoresSafeArea()
    }
}

private struct HeaderBar: View {
    let onLoginStatus: () -> Void
    @State private var isShowingSettings = false

    var body: some View {
        HStack {
            Text("zeus")
                .font(.system(size: 12, weight: .regular, design: .monospaced))
                .foregroundStyle(TerminalPalette.dimText)
                .padding(.leading, 66)
                .allowsHitTesting(false)

            Spacer()

            Button {
                isShowingSettings.toggle()
            } label: {
                Image(systemName: "gearshape")
                    .font(.system(size: 10, weight: .regular))
                    .foregroundStyle(
                        isShowingSettings ? TerminalPalette.cyan : TerminalPalette.dimText
                    )
                    .frame(width: 18, height: 16)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Settings")
        }
        .frame(height: 16)
        .overlay(alignment: .topTrailing) {
            if isShowingSettings {
                SettingsDropdown {
                    isShowingSettings = false
                    onLoginStatus()
                }
                .offset(y: 20)
                .zIndex(20)
            }
        }
        .zIndex(20)
    }
}

private struct SettingsDropdown: View {
    let onLoginStatus: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button(action: onLoginStatus) {
                HStack(spacing: 7) {
                    Image(systemName: "person.crop.circle")
                        .font(.system(size: 10, weight: .regular))
                        .foregroundStyle(TerminalPalette.cyan)
                        .frame(width: 12)

                    Text("Login Status")
                        .font(.system(size: 11, weight: .regular, design: .monospaced))
                        .foregroundStyle(TerminalPalette.primaryText)

                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .contentShape(Rectangle())
            }
            .buttonStyle(TerminalMenuButtonStyle())
        }
        .frame(width: 142)
        .background(
            Rectangle()
                .fill(TerminalPalette.background)
        )
        .overlay(
            Rectangle()
                .stroke(TerminalPalette.border.opacity(0.45), lineWidth: 1)
        )
        .shadow(color: TerminalPalette.shadow.opacity(0.18), radius: 8, x: 0, y: 6)
    }
}

private struct TerminalMenuButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(
                Rectangle()
                    .fill(configuration.isPressed ? TerminalPalette.cyan.opacity(0.12) : .clear)
            )
    }
}

private struct LocalEventMonitor: NSViewRepresentable {
    let onEvent: (NSEvent) -> Bool

    func makeCoordinator() -> Coordinator {
        Coordinator(onEvent: onEvent)
    }

    func makeNSView(context: Context) -> NSView {
        context.coordinator.start()
        return NSView(frame: .zero)
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        context.coordinator.onEvent = onEvent
    }

    static func dismantleNSView(_ nsView: NSView, coordinator: Coordinator) {
        coordinator.stop()
    }

    final class Coordinator {
        var onEvent: (NSEvent) -> Bool
        private var monitor: Any?

        init(onEvent: @escaping (NSEvent) -> Bool) {
            self.onEvent = onEvent
        }

        deinit {
            stop()
        }

        func start() {
            guard monitor == nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(
                matching: [.keyDown, .leftMouseUp, .rightMouseUp]
            ) { [weak self] event in
                guard let self else { return event }
                return self.onEvent(event) ? nil : event
            }
        }

        func stop() {
            if let monitor {
                NSEvent.removeMonitor(monitor)
            }
            monitor = nil
        }
    }
}

private struct SearchBar: View {
    @Binding var text: String
    let resultSummary: String
    let onPrevious: () -> Void
    let onNext: () -> Void
    let onClose: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 10, weight: .regular))
                .foregroundStyle(TerminalPalette.cyan)
                .frame(width: TerminalLayout.markerWidth, alignment: .leading)

            PromptTextField(
                text: $text,
                placeholder: "search transcript...",
                onSubmit: onNext,
                onHistoryPrevious: { false },
                onHistoryNext: { false }
            )
            .frame(height: 18)
            .frame(maxWidth: .infinity, alignment: .leading)

            Text(resultSummary)
                .foregroundStyle(TerminalPalette.dimText)
                .lineLimit(1)
                .frame(minWidth: 82, alignment: .trailing)

            searchButton(systemName: "chevron.up", help: "Previous Match", action: onPrevious)
            searchButton(systemName: "chevron.down", help: "Next Match", action: onNext)
            searchButton(systemName: "xmark", help: "Close Search", action: onClose)
        }
        .font(.system(size: 11, weight: .regular, design: .monospaced))
        .frame(height: 20)
    }

    private func searchButton(
        systemName: String,
        help: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(TerminalPalette.dimText)
                .frame(width: 18, height: 18)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(help)
    }
}

private struct TranscriptView: View {
    let lines: [TranscriptLine]
    let isCacheStatsVisible: Bool
    let searchMatchLineIDs: Set<UUID>
    let selectedSearchLineID: UUID?
    let scrollTarget: TranscriptScrollTarget?

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    ForEach(lines) { line in
                        TerminalLineView(
                            line: line,
                            isCacheStatsVisible: isCacheStatsVisible,
                            isSearchMatch: searchMatchLineIDs.contains(line.id),
                            isSelectedSearchMatch: selectedSearchLineID == line.id
                        )
                            .id(line.id)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.bottom, 6)
            }
            .scrollIndicators(.hidden)
            .onChange(of: scrollTarget) { target in
                guard selectedSearchLineID == nil else { return }
                guard let target else { return }
                proxy.scrollTo(target.lineID, anchor: .bottom)
            }
            .onChange(of: selectedSearchLineID) { lineID in
                guard let lineID else { return }
                withAnimation(.easeOut(duration: 0.12)) {
                    proxy.scrollTo(lineID, anchor: .center)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct TerminalLineView: View {
    let line: TranscriptLine
    let isCacheStatsVisible: Bool
    let isSearchMatch: Bool
    let isSelectedSearchMatch: Bool

    var body: some View {
        Group {
            if line.kind == .tool {
                HStack(alignment: .center, spacing: TerminalLayout.markerTextSpacing) {
                    toolPrefix
                        .frame(width: TerminalLayout.markerWidth, alignment: .leading)
                    lineText
                }
            } else {
                HStack(alignment: .top, spacing: TerminalLayout.markerTextSpacing) {
                    prefix
                        .frame(width: TerminalLayout.markerWidth, alignment: .leading)
                    lineText
                }
            }
        }
        .padding(.vertical, isSearchMatch ? 2 : 0)
        .background(searchBackground)
    }

    @ViewBuilder
    private var searchBackground: some View {
        if isSelectedSearchMatch {
            Rectangle().fill(TerminalPalette.cyan.opacity(0.16))
        } else if isSearchMatch {
            Rectangle().fill(TerminalPalette.green.opacity(0.08))
        }
    }

    private var lineText: some View {
        Group {
            if line.kind == .tool, let toolCall = line.toolCall {
                ToolCallLine(toolCall: toolCall)
            } else if line.kind == .assistant {
                assistantLine
            } else {
                Text(line.text.isEmpty ? " " : line.text)
                    .foregroundStyle(textColor)
            }
        }
        .fixedSize(horizontal: false, vertical: true)
        .textSelection(.enabled)
    }

    private var assistantLine: some View {
        VStack(alignment: .leading, spacing: 4) {
            if line.isStreaming {
                Text(line.text.isEmpty ? " " : line.text)
                    .foregroundStyle(TerminalPalette.primaryText)
            } else if let blocks = line.markdownBlocks {
                TerminalMarkdownView(blocks: blocks)
            } else {
                TerminalMarkdownView(text: line.text)
            }

            if isCacheStatsVisible {
                ForEach(line.cacheStats.indices, id: \.self) { index in
                    Text(line.cacheStats[index].displayText)
                        .font(.system(size: 10, weight: .regular, design: .monospaced))
                        .foregroundStyle(TerminalPalette.dimText)
                }
            }
        }
    }

    @ViewBuilder
    private var prefix: some View {
        switch line.kind {
        case .user:
            Text(">")
                .foregroundStyle(TerminalPalette.cyan)
        case .assistant:
            marker(color: TerminalPalette.green)
        case .status, .tool:
            marker(color: TerminalPalette.green)
        case .error:
            marker(color: TerminalPalette.red)
        }
    }

    private var toolPrefix: some View {
        marker(color: TerminalPalette.green, topPadding: 0)
    }

    private func marker(color: Color) -> some View {
        marker(color: color, topPadding: 4)
    }

    private func marker(color: Color, topPadding: CGFloat) -> some View {
        Circle()
            .fill(color)
            .frame(width: 7, height: 7)
            .padding(.top, topPadding)
    }

    private var textColor: Color {
        switch line.kind {
        case .error:
            return TerminalPalette.red
        default:
            return TerminalPalette.primaryText
        }
    }
}

private struct ToolCallLine: View {
    let toolCall: ToolCallTranscript
    private let toolCellChromeColor = Color.clear

    var body: some View {
        HStack(alignment: .center, spacing: 4) {
            toolCell(
                width: 24,
                horizontalPadding: 0,
                alignment: .center
            ) {
                Image(systemName: iconName)
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(iconColor)
                    .frame(width: 14, height: 13, alignment: .center)
            }

            toolCell(width: 76) {
                Text(statusText)
                    .foregroundStyle(statusColor)
            }

            toolCell(width: 42) {
                Text(toolCall.name)
                    .foregroundStyle(TerminalPalette.cyan)
                    .fontWeight(.semibold)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }

            if let target = toolCall.target, !target.isEmpty {
                toolCell {
                    Text(target)
                        .foregroundStyle(TerminalPalette.primaryText)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
        }
        .font(.system(size: 11, weight: .regular, design: .monospaced))
        .fixedSize(horizontal: false, vertical: true)
    }

    private func toolCell<Content: View>(
        width: CGFloat? = nil,
        horizontalPadding: CGFloat = 7,
        alignment: Alignment = .leading,
        @ViewBuilder content: () -> Content
    ) -> some View {
        Group {
            if let width {
                toolCellStyle(
                    content()
                        .padding(.horizontal, horizontalPadding)
                        .frame(width: width, alignment: alignment)
                        .frame(minHeight: 23, alignment: .center)
                )
            } else {
                toolCellStyle(
                    content()
                        .padding(.horizontal, horizontalPadding)
                        .frame(minHeight: 23, alignment: .center)
                )
            }
        }
    }

    private func toolCellStyle<Content: View>(_ content: Content) -> some View {
        content
            .background(
                Rectangle()
                    .fill(toolCellChromeColor)
            )
            .overlay(
                Rectangle()
                    .stroke(toolCellChromeColor, lineWidth: 1)
            )
    }

    private var statusText: String {
        switch toolCall.status {
        case .running:
            return toolCall.action
        case .completed:
            return "completed"
        case .failed:
            return "failed"
        }
    }

    private var iconName: String {
        toolCall.iconName
    }

    private var iconColor: Color {
        switch toolCall.status {
        case .failed:
            return TerminalPalette.red
        default:
            return TerminalPalette.cyan
        }
    }

    private var statusColor: Color {
        switch toolCall.status {
        case .running:
            return TerminalPalette.dimText
        case .completed:
            return TerminalPalette.green
        case .failed:
            return TerminalPalette.red
        }
    }
}

private struct InputPrompt: View {
    @Binding var text: String
    let prompt: String
    let placeholder: String
    let onSubmit: () -> Void
    let onHistoryPrevious: () -> Bool
    let onHistoryNext: () -> Bool
    let onMoveDownFromCurrent: () -> Void
    let isCancelVisible: Bool
    let onCancel: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: TerminalLayout.markerTextSpacing) {
            Text(prompt)
                .foregroundStyle(TerminalPalette.cyan)
                .frame(width: TerminalLayout.markerWidth, alignment: .leading)

            PromptTextField(
                text: $text,
                placeholder: placeholder,
                onSubmit: onSubmit,
                onHistoryPrevious: onHistoryPrevious,
                onHistoryNext: onHistoryNext,
                onMoveDownFromCurrent: {
                    onMoveDownFromCurrent()
                    return true
                }
            )
                .frame(height: 18)
                .frame(maxWidth: .infinity, alignment: .leading)

            if isCancelVisible {
                Button(action: onCancel) {
                    Image(systemName: "xmark.circle")
                        .font(.system(size: 11, weight: .regular))
                        .foregroundStyle(TerminalPalette.red)
                        .frame(width: 18, height: 18)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Cancel Turn")
            }
        }
    }
}

private struct FooterBar: View {
    let workspace: WorkspaceMetadata
    let branchOptions: [String]
    let selectedBranch: String
    let isBranchMenuEnabled: Bool
    let model: String
    let modelOptions: [String]
    let selectedModel: String
    let isModelMenuEnabled: Bool
    let effort: String
    let effortOptions: [String]
    let isEffortMenuEnabled: Bool
    let permissions: String
    let permissionOptions: [String]
    let selectedPermission: String
    let isPermissionsMenuEnabled: Bool
    let tokenUsage: String
    @Binding var activeMenu: FooterMenuID?
    @Binding var focusedMenu: FooterMenuID?
    let branchHighlightedOption: String?
    let modelHighlightedOption: String?
    let effortHighlightedOption: String?
    let permissionsHighlightedOption: String?
    let modelTitle: (String) -> String
    let permissionTitle: (String) -> String
    let onSelectBranch: (String) -> Void
    let onSelectModel: (String) -> Void
    let onSelectEffort: (String) -> Void
    let onSelectPermissions: (String) -> Void
    let onHighlightBranch: (String) -> Void
    let onHighlightModel: (String) -> Void
    let onHighlightEffort: (String) -> Void
    let onHighlightPermissions: (String) -> Void
    private let itemSpacing: CGFloat = 22
    private let pathSpacing: CGFloat = 32

    var body: some View {
        HStack(spacing: 0) {
            HStack(spacing: itemSpacing) {
                footerText(workspace.name, color: TerminalPalette.dimText)
                FooterMenu(
                    id: .branch,
                    title: workspace.branch,
                    options: branchOptions,
                    selectedOption: selectedBranch,
                    highlightedOption: branchHighlightedOption,
                    isEnabled: isBranchMenuEnabled,
                    enabledColor: TerminalPalette.green,
                    activeMenu: $activeMenu,
                    focusedMenu: $focusedMenu,
                    optionTitle: { $0 },
                    menuWidth: 164,
                    help: "Branch",
                    onSelect: onSelectBranch,
                    onHighlight: onHighlightBranch
                )
                FooterMenu(
                    id: .model,
                    title: model,
                    options: modelOptions,
                    selectedOption: selectedModel,
                    highlightedOption: modelHighlightedOption,
                    isEnabled: isModelMenuEnabled,
                    activeMenu: $activeMenu,
                    focusedMenu: $focusedMenu,
                    optionTitle: modelTitle,
                    menuWidth: 178,
                    help: "Model",
                    onSelect: onSelectModel,
                    onHighlight: onHighlightModel
                )
                FooterMenu(
                    id: .effort,
                    title: effort,
                    options: effortOptions,
                    selectedOption: effort,
                    highlightedOption: effortHighlightedOption,
                    isEnabled: isEffortMenuEnabled,
                    activeMenu: $activeMenu,
                    focusedMenu: $focusedMenu,
                    optionTitle: { $0 },
                    menuWidth: 88,
                    help: "Reasoning Effort",
                    onSelect: onSelectEffort,
                    onHighlight: onHighlightEffort
                )
                FooterMenu(
                    id: .permissions,
                    title: permissions,
                    options: permissionOptions,
                    selectedOption: selectedPermission,
                    highlightedOption: permissionsHighlightedOption,
                    isEnabled: isPermissionsMenuEnabled,
                    activeMenu: $activeMenu,
                    focusedMenu: $focusedMenu,
                    optionTitle: permissionTitle,
                    menuWidth: 88,
                    help: "Permissions",
                    onSelect: onSelectPermissions,
                    onHighlight: onHighlightPermissions
                )
                footerText(tokenUsage, color: TerminalPalette.dimText)
            }
            .layoutPriority(1)

            Spacer(minLength: pathSpacing)

            HStack(spacing: 18) {
                FooterShortcutsMenu(activeMenu: $activeMenu, focusedMenu: $focusedMenu)

                footerText(workspace.displayPath, color: TerminalPalette.dimText)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .frame(maxWidth: 360, alignment: .trailing)
        }
        .font(.system(size: 11, weight: .regular, design: .monospaced))
        .frame(height: 18)
    }

    private func footerText(_ text: String, color: Color) -> some View {
        Text(text)
            .foregroundStyle(color)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
    }
}

private struct FooterShortcutsMenu: View {
    @Binding var activeMenu: FooterMenuID?
    @Binding var focusedMenu: FooterMenuID?

    private var isOpen: Bool {
        activeMenu == .shortcuts
    }

    private var isFocused: Bool {
        focusedMenu == .shortcuts
    }

    var body: some View {
        Text("shortcuts")
            .foregroundStyle(TerminalPalette.cyan)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .frame(height: 18, alignment: .center)
            .padding(.horizontal, 3)
            .background(
                Rectangle()
                    .fill(isFocused ? TerminalPalette.cyan.opacity(0.12) : .clear)
            )
            .contentShape(Rectangle())
            .onTapGesture {
                if isOpen {
                    activeMenu = nil
                    focusedMenu = .shortcuts
                } else {
                    DispatchQueue.main.async {
                        focusedMenu = .shortcuts
                        activeMenu = .shortcuts
                    }
                }
            }
            .help("Shortcuts")
            .overlay(alignment: .bottomTrailing) {
                if isOpen {
                    ShortcutsDropdown(items: shortcutItems)
                        .offset(y: -23)
                        .zIndex(30)
                }
            }
            .zIndex(isOpen ? 30 : 0)
    }
}

private struct ShortcutsDropdown: View {
    let items: [ShortcutItem]

    var body: some View {
        VStack(alignment: .trailing, spacing: 0) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(items) { item in
                    HStack(spacing: 12) {
                        Text(item.shortcut)
                            .foregroundStyle(TerminalPalette.cyan)
                            .frame(width: 96, alignment: .leading)

                        Text(item.action)
                            .foregroundStyle(TerminalPalette.primaryText)
                            .lineLimit(1)

                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 9)
                    .padding(.vertical, 5)
                }
            }
            .frame(width: 386)
            .background(Rectangle().fill(TerminalPalette.background))
            .overlay(
                Rectangle()
                    .stroke(TerminalPalette.border.opacity(0.45), lineWidth: 1)
            )
            .shadow(color: TerminalPalette.shadow.opacity(0.18), radius: 8, x: 0, y: 6)

            Rectangle()
                .fill(TerminalPalette.background)
                .frame(width: 10, height: 10)
                .rotationEffect(.degrees(45))
                .padding(.trailing, 28)
                .offset(y: -5)
        }
        .font(.system(size: 11, weight: .regular, design: .monospaced))
        .fixedSize(horizontal: true, vertical: true)
    }
}

private struct FooterMenu: View {
    let id: FooterMenuID
    let title: String
    let options: [String]
    let selectedOption: String
    let highlightedOption: String?
    let isEnabled: Bool
    var enabledColor = TerminalPalette.cyan
    @Binding var activeMenu: FooterMenuID?
    @Binding var focusedMenu: FooterMenuID?
    let optionTitle: (String) -> String
    let menuWidth: CGFloat
    let help: String
    let onSelect: (String) -> Void
    let onHighlight: (String) -> Void

    private var menuOptions: [String] {
        options.isEmpty ? [selectedOption] : options
    }

    private var isOpen: Bool {
        activeMenu == id
    }

    private var isFocused: Bool {
        focusedMenu == id
    }

    var body: some View {
        Text(title)
            .foregroundStyle(isEnabled ? enabledColor : TerminalPalette.dimText)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .frame(height: 18, alignment: .center)
            .padding(.horizontal, 3)
            .background(
                Rectangle()
                    .fill(isFocused ? enabledColor.opacity(0.12) : .clear)
            )
            .contentShape(Rectangle())
            .onTapGesture {
                guard isEnabled else { return }
                if isOpen {
                    activeMenu = nil
                    focusedMenu = id
                } else {
                    DispatchQueue.main.async {
                        focusedMenu = id
                        activeMenu = id
                    }
                }
            }
            .help(help)
            .overlay(alignment: .bottom) {
                if isOpen {
                    FooterDropdown(
                        options: menuOptions,
                        selectedOption: selectedOption,
                        highlightedOption: highlightedOption,
                        optionTitle: optionTitle,
                        menuWidth: menuWidth
                    ) { option in
                        activeMenu = nil
                        focusedMenu = nil
                        onSelect(option)
                    } onHighlight: { option in
                        onHighlight(option)
                    }
                    .offset(y: -23)
                    .zIndex(30)
                }
            }
            .onChange(of: isEnabled) { newValue in
                if !newValue {
                    activeMenu = nil
                    if focusedMenu == id {
                        focusedMenu = nil
                    }
                }
            }
            .zIndex(isOpen ? 30 : 0)
    }
}

private struct FooterDropdown: View {
    let options: [String]
    let selectedOption: String
    let highlightedOption: String?
    let optionTitle: (String) -> String
    let menuWidth: CGFloat
    let onSelect: (String) -> Void
    let onHighlight: (String) -> Void

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(options, id: \.self) { option in
                    dropdownButton(for: option)
                }
            }
            .frame(width: menuWidth)
            .background(Rectangle().fill(TerminalPalette.background))
            .overlay(
                Rectangle()
                    .stroke(TerminalPalette.border.opacity(0.45), lineWidth: 1)
            )
            .shadow(color: TerminalPalette.shadow.opacity(0.18), radius: 8, x: 0, y: 6)

            Rectangle()
                .fill(TerminalPalette.background)
                .frame(width: 10, height: 10)
                .rotationEffect(.degrees(45))
                .offset(y: -5)
        }
        .font(.system(size: 11, weight: .regular, design: .monospaced))
        .fixedSize(horizontal: true, vertical: true)
    }

    private func dropdownButton(for option: String) -> some View {
        let isSelected = option == selectedOption
        let isHighlighted = option == highlightedOption

        return Button {
            onSelect(option)
        } label: {
            HStack(spacing: 7) {
                if isSelected {
                    Image(systemName: "checkmark")
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(TerminalPalette.cyan)
                        .frame(width: 12)
                } else {
                    Color.clear
                        .frame(width: 12, height: 10)
                }

                Text(optionTitle(option))
                    .foregroundStyle(
                        isSelected || isHighlighted
                            ? TerminalPalette.cyan
                            : TerminalPalette.primaryText
                    )
                    .lineLimit(1)

                Spacer(minLength: 0)
            }
            .padding(.horizontal, 9)
            .padding(.vertical, 6)
            .contentShape(Rectangle())
            .background(
                Rectangle()
                    .fill(isHighlighted ? TerminalPalette.cyan.opacity(0.12) : .clear)
            )
        }
        .buttonStyle(TerminalMenuButtonStyle())
        .onHover { isHovering in
            guard isHovering else { return }
            onHighlight(option)
        }
    }
}
