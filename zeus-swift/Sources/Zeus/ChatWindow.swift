import AppKit
import SwiftUI

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
    @Bindable var viewModel: ChatViewModel
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
                HeaderBar(
                    isLoggedIn: viewModel.isLoggedIn,
                    onLogin: viewModel.startLogin,
                    onLoginStatus: viewModel.showLoginStatus
                )

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
                    activeAssistantStream: viewModel.activeAssistantStream,
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
