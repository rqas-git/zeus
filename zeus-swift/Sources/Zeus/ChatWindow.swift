import AppKit
import SwiftUI

struct ChatWindow: View {
    @Bindable var viewModel: ChatViewModel
    @State private var footerMenuController = FooterMenuController()

    var body: some View {
        ZStack {
            TerminalBackground()

            VStack(spacing: 0) {
                HeaderBar(
                    isLoggedIn: viewModel.isLoggedIn,
                    canClearContext: viewModel.canClearContext,
                    onClearContext: viewModel.clearContext,
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
                    .padding(.top, TerminalLayout.inputTopPadding)
                }

                TranscriptView(
                    lines: viewModel.lines,
                    activeAssistantStream: viewModel.activeAssistantStream,
                    isCacheStatsVisible: viewModel.showCacheStats,
                    searchMatchLineIDs: viewModel.searchMatchLineIDs,
                    selectedSearchLineID: viewModel.selectedSearchLineID,
                    scrollTarget: viewModel.transcriptScrollTarget
                )
                .padding(.top, TerminalLayout.transcriptTopPadding)

                InputPrompt(
                    text: $viewModel.draft,
                    prompt: viewModel.inputPrompt,
                    placeholder: viewModel.inputPlaceholder,
                    onSubmit: viewModel.sendDraft,
                    onHistoryPrevious: viewModel.selectPreviousSubmittedMessage,
                    onHistoryNext: viewModel.selectNextSubmittedMessage,
                    onMoveDownFromCurrent: { footerMenuController.focusFirstMenu(viewModel: viewModel) },
                    isCancelVisible: viewModel.canCancelTurn,
                    onCancel: viewModel.cancelCurrentTurn
                )
                .padding(.top, TerminalLayout.inputTopPadding)

                FooterBar(
                    workspace: viewModel.workspace,
                    branch: menuConfig(
                        .branch,
                        title: viewModel.workspace.branch,
                        selectedOption: viewModel.workspace.branch,
                        isEnabled: viewModel.canChangeBranch,
                        enabledColor: TerminalPalette.green,
                        help: "Branch",
                        onSelect: viewModel.selectBranch
                    ),
                    model: menuConfig(
                        .model,
                        title: viewModel.model,
                        selectedOption: viewModel.selectedModel,
                        isEnabled: viewModel.canChangeModel,
                        optionTitle: viewModel.displayModel,
                        help: "Model",
                        onSelect: viewModel.selectModel
                    ),
                    effort: menuConfig(
                        .effort,
                        title: viewModel.effort,
                        selectedOption: viewModel.effort,
                        isEnabled: viewModel.canChangeEffort,
                        help: "Reasoning Effort",
                        onSelect: viewModel.selectEffort
                    ),
                    permissions: menuConfig(
                        .permissions,
                        title: viewModel.permissions,
                        selectedOption: viewModel.selectedPermission,
                        isEnabled: viewModel.canChangePermissions,
                        optionTitle: viewModel.displayPermission,
                        help: "Permissions",
                        onSelect: viewModel.selectPermissions
                    ),
                    tokenUsage: viewModel.tokenUsage,
                    activeMenu: $footerMenuController.activeMenu,
                    focusedMenu: $footerMenuController.focusedMenu
                )
                .padding(.top, TerminalLayout.footerTopPadding)
            }
            .padding(.horizontal, TerminalLayout.windowHorizontalPadding)
            .padding(.top, TerminalLayout.windowTopPadding)
            .padding(.bottom, TerminalLayout.windowBottomPadding)
        }
        .ignoresSafeArea(.container, edges: .top)
        .background(WindowConfigurator())
        .background(LocalEventMonitor(onEvent: handleLocalEvent(_:)))
        .font(TerminalTypography.chatSmall)
        .foregroundStyle(TerminalPalette.primaryText)
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in
            viewModel.shutdown()
        }
        .onDisappear {
            viewModel.shutdown()
        }
    }

    private func menuConfig(
        _ id: FooterMenuID,
        title: String,
        selectedOption: String,
        isEnabled: Bool,
        enabledColor: Color = TerminalPalette.cyan,
        optionTitle: @escaping (String) -> String = { $0 },
        help: String,
        onSelect: @escaping (String) -> Void
    ) -> FooterMenuConfig {
        FooterMenuConfig(
            id: id,
            title: title,
            options: footerMenuController.options(for: id, viewModel: viewModel),
            selectedOption: selectedOption,
            isEnabled: isEnabled,
            enabledColor: enabledColor,
            highlightedOption: footerMenuController.highlightedOptionByMenu[id],
            optionTitle: optionTitle,
            help: help,
            onSelect: onSelect,
            onHighlight: { footerMenuController.highlightedOptionByMenu[id] = $0 }
        )
    }

    private func handleLocalEvent(_ event: NSEvent) -> Bool {
        switch event.type {
        case .keyDown:
            return handleKeyDown(event)
        case .leftMouseUp, .rightMouseUp:
            if footerMenuController.activeMenu != nil || footerMenuController.focusedMenu != nil {
                footerMenuController.closeAll()
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
        if let shortcut = AppShortcut.matching(event), handle(shortcut) {
            return true
        }
        if event.keyCode == KeyCode.escape, viewModel.isSearchVisible {
            viewModel.closeSearch()
            return true
        }
        if footerMenuController.focusedMenu != nil || footerMenuController.activeMenu != nil {
            if footerMenuController.handleFooterNavigationKey(event, viewModel: viewModel) {
                return true
            }
            footerMenuController.closeAll()
        }
        return false
    }

    private func handle(_ shortcut: AppShortcut) -> Bool {
        switch shortcut {
        case .search:
            footerMenuController.closeAll()
            viewModel.showSearch()
        case .searchNext:
            viewModel.selectNextSearchMatch()
        case .searchPrevious:
            viewModel.selectPreviousSearchMatch()
        case .terminalPassthrough:
            viewModel.toggleTerminalPassthrough()
        case .clearInput:
            viewModel.clearDraft()
        case .branchMenu:
            footerMenuController.open(.branch, viewModel: viewModel)
        case .modelMenu:
            footerMenuController.open(.model, viewModel: viewModel)
        case .effortMenu:
            footerMenuController.open(.effort, viewModel: viewModel)
        case .permissionsMenu:
            footerMenuController.open(.permissions, viewModel: viewModel)
        default:
            return false
        }
        return true
    }
}
