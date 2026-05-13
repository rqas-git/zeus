import AppKit
import Observation

@MainActor
@Observable
final class FooterMenuController {
    var activeMenu: FooterMenuID?
    var focusedMenu: FooterMenuID?
    var highlightedOptionByMenu: [FooterMenuID: String] = [:]

    func closeAll() {
        activeMenu = nil
        focusedMenu = nil
    }

    func focusFirstMenu(viewModel: ChatViewModel) {
        activeMenu = nil
        focusedMenu = navigationMenus(viewModel: viewModel).first
    }

    func open(_ menu: FooterMenuID, viewModel: ChatViewModel) {
        guard menu == .shortcuts || isEnabled(menu, viewModel: viewModel) else { return }
        highlightedOptionByMenu[menu] = preferredHighlight(for: menu, viewModel: viewModel)
        focusedMenu = menu
        activeMenu = menu
    }

    func handleFooterNavigationKey(_ event: NSEvent, viewModel: ChatViewModel) -> Bool {
        guard event.hasNoModifiers() else { return false }
        if focusedMenu == nil {
            focusedMenu = activeMenu ?? navigationMenus(viewModel: viewModel).first
        }
        guard activeMenu != nil else {
            return handleFocusedFooterKey(event, viewModel: viewModel)
        }
        return handleOpenFooterMenuKey(event, viewModel: viewModel)
    }

    private func handleFocusedFooterKey(_ event: NSEvent, viewModel: ChatViewModel) -> Bool {
        switch event.keyCode {
        case KeyCode.escape:
            focusedMenu = nil
            return true
        case KeyCode.returnKey, KeyCode.keypadEnter, KeyCode.upArrow:
            openFocusedMenu(viewModel: viewModel)
            return true
        case KeyCode.downArrow:
            focusedMenu = nil
            return true
        case KeyCode.rightArrow:
            moveFocusedMenu(by: 1, viewModel: viewModel)
            return true
        case KeyCode.leftArrow:
            moveFocusedMenu(by: -1, viewModel: viewModel)
            return true
        default:
            return false
        }
    }

    private func handleOpenFooterMenuKey(_ event: NSEvent, viewModel: ChatViewModel) -> Bool {
        switch event.keyCode {
        case KeyCode.escape:
            activeMenu = nil
            return true
        case KeyCode.returnKey, KeyCode.keypadEnter:
            return selectActiveOption(viewModel: viewModel)
        case KeyCode.downArrow:
            if isActiveMenuHighlightAtLastOption(viewModel: viewModel) {
                activeMenu = nil
            } else {
                moveActiveMenuHighlight(by: 1, viewModel: viewModel)
            }
            return true
        case KeyCode.upArrow:
            moveActiveMenuHighlight(by: -1, viewModel: viewModel)
            return true
        case KeyCode.rightArrow:
            moveFocusedMenu(by: 1, viewModel: viewModel)
            return true
        case KeyCode.leftArrow:
            moveFocusedMenu(by: -1, viewModel: viewModel)
            return true
        default:
            return false
        }
    }

    private func openFocusedMenu(viewModel: ChatViewModel) {
        if let focusedMenu {
            open(focusedMenu, viewModel: viewModel)
        } else {
            focusFirstMenu(viewModel: viewModel)
        }
    }

    private func moveFocusedMenu(by offset: Int, viewModel: ChatViewModel) {
        let menus = navigationMenus(viewModel: viewModel)
        guard !menus.isEmpty else { return }
        let current = focusedMenu.flatMap { menus.firstIndex(of: $0) } ?? 0
        let next = (current + offset + menus.count) % menus.count
        activeMenu = nil
        focusedMenu = menus[next]
    }

    private func moveActiveMenuHighlight(by offset: Int, viewModel: ChatViewModel) {
        guard let menu = activeMenu, menu != .shortcuts else { return }
        let options = options(for: menu, viewModel: viewModel)
        let current = highlightedOptionByMenu[menu] ?? selectedOption(for: menu, viewModel: viewModel)
        highlightedOptionByMenu[menu] = nextMenuOption(in: options, current: current, offset: offset)
    }

    private func selectActiveOption(viewModel: ChatViewModel) -> Bool {
        guard let menu = activeMenu else { return false }
        guard menu != .shortcuts else {
            activeMenu = nil
            return true
        }
        guard let option = highlightedOptionByMenu[menu] ?? options(for: menu, viewModel: viewModel).first else {
            return false
        }
        closeAll()
        switch menu {
        case .branch: viewModel.selectBranch(option)
        case .model: viewModel.selectModel(option)
        case .effort: viewModel.selectEffort(option)
        case .permissions: viewModel.selectPermissions(option)
        case .shortcuts: break
        }
        return true
    }

    private func isActiveMenuHighlightAtLastOption(viewModel: ChatViewModel) -> Bool {
        guard let menu = activeMenu, menu != .shortcuts else { return true }
        let options = options(for: menu, viewModel: viewModel)
        guard let last = options.last else { return true }
        return (highlightedOptionByMenu[menu] ?? selectedOption(for: menu, viewModel: viewModel)) == last
    }

    private func preferredHighlight(for menu: FooterMenuID, viewModel: ChatViewModel) -> String? {
        let options = options(for: menu, viewModel: viewModel)
        let selected = selectedOption(for: menu, viewModel: viewModel)
        return options.contains(selected) ? selected : options.first
    }

    func options(for menu: FooterMenuID, viewModel: ChatViewModel) -> [String] {
        switch menu {
        case .branch: viewModel.branchOptions.isEmpty ? [viewModel.workspace.branch] : viewModel.branchOptions
        case .model: viewModel.modelOptions.isEmpty ? [viewModel.selectedModel] : viewModel.modelOptions
        case .effort: viewModel.effortOptions.isEmpty ? [viewModel.effort] : viewModel.effortOptions
        case .permissions: viewModel.permissionOptions.isEmpty ? [viewModel.selectedPermission] : viewModel.permissionOptions
        case .shortcuts: []
        }
    }

    private func selectedOption(for menu: FooterMenuID, viewModel: ChatViewModel) -> String {
        switch menu {
        case .branch: viewModel.workspace.branch
        case .model: viewModel.selectedModel
        case .effort: viewModel.effort
        case .permissions: viewModel.selectedPermission
        case .shortcuts: ""
        }
    }

    private func isEnabled(_ menu: FooterMenuID, viewModel: ChatViewModel) -> Bool {
        switch menu {
        case .branch: viewModel.canChangeBranch
        case .model: viewModel.canChangeModel
        case .effort: viewModel.canChangeEffort
        case .permissions: viewModel.canChangePermissions
        case .shortcuts: true
        }
    }

    private func navigationMenus(viewModel: ChatViewModel) -> [FooterMenuID] {
        var menus: [FooterMenuID] = []
        if viewModel.canChangeBranch { menus.append(.branch) }
        if viewModel.canChangeModel { menus.append(.model) }
        if viewModel.canChangeEffort { menus.append(.effort) }
        if viewModel.canChangePermissions { menus.append(.permissions) }
        menus.append(.shortcuts)
        return menus
    }

    private func nextMenuOption(in options: [String], current: String, offset: Int) -> String? {
        guard !options.isEmpty else { return nil }
        let currentIndex = options.firstIndex(of: current) ?? 0
        let nextIndex = min(max(currentIndex + offset, 0), options.count - 1)
        return options[nextIndex]
    }
}
