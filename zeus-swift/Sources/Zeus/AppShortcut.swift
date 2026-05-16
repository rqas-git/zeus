import AppKit

enum AppShortcut: CaseIterable, Identifiable {
    case newWindow
    case branchMenu
    case modelMenu
    case effortMenu
    case permissionsMenu
    case search
    case searchNext
    case searchPrevious
    case terminalPassthrough
    case clearInput
    case pathCompletion
    case insertNewline
    case historyOrMenuPrevious
    case historyOrMenuNext
    case footerNavigation
    case activateFooterItem
    case escape

    var id: Self { self }

    var display: String {
        switch self {
        case .newWindow: "Cmd+N"
        case .branchMenu: "Cmd+B"
        case .modelMenu: "Cmd+M"
        case .effortMenu: "Cmd+E"
        case .permissionsMenu: "Cmd+P"
        case .search: "Cmd+F"
        case .searchNext: "Cmd+G"
        case .searchPrevious: "Cmd+Shift+G"
        case .terminalPassthrough: "Cmd+T"
        case .clearInput: "Ctrl+C"
        case .pathCompletion: "Tab"
        case .insertNewline: "Ctrl+Enter"
        case .historyOrMenuPrevious: "Up Arrow"
        case .historyOrMenuNext: "Down Arrow"
        case .footerNavigation: "Left / Right Arrow"
        case .activateFooterItem: "Return / Enter"
        case .escape: "Esc"
        }
    }

    var actionDescription: String {
        switch self {
        case .newWindow: "Open a new Zeus window"
        case .branchMenu: "Open branch menu"
        case .modelMenu: "Open model menu"
        case .effortMenu: "Open effort menu"
        case .permissionsMenu: "Open permissions menu"
        case .search: "Open transcript search"
        case .searchNext: "Next search match"
        case .searchPrevious: "Previous search match"
        case .terminalPassthrough: "Toggle terminal passthrough"
        case .clearInput: "Clear input"
        case .pathCompletion: "Complete path or accept suggestion"
        case .insertNewline: "Insert newline"
        case .historyOrMenuPrevious: "Previous message, open footer menu, or previous option"
        case .historyOrMenuNext: "Next message, next option, or close menu at bottom"
        case .footerNavigation: "Move between footer controls"
        case .activateFooterItem: "Activate footer control or menu option"
        case .escape: "Cancel response, close completion, search, or footer UI"
        }
    }

    func matches(_ event: NSEvent) -> Bool {
        switch self {
        case .branchMenu: return event.isCommandShortcut("b")
        case .modelMenu: return event.isCommandShortcut("m")
        case .effortMenu: return event.isCommandShortcut("e")
        case .permissionsMenu: return event.isCommandShortcut("p")
        case .search: return event.isCommandShortcut("f")
        case .searchNext: return event.isCommandShortcut("g")
        case .searchPrevious: return event.isCommandShiftShortcut("g")
        case .terminalPassthrough: return event.isCommandShortcut("t")
        case .clearInput:
            let flags = event.independentModifierFlags
            return flags.contains(.control)
                && flags.intersection([.command, .option, .shift]).isEmpty
                && event.charactersIgnoringModifiers?.lowercased() == "c"
        default:
            return false
        }
    }

    static func matching(_ event: NSEvent) -> AppShortcut? {
        allCases.first { $0.matches(event) }
    }
}

private extension NSEvent {
    func isCommandShortcut(_ key: String) -> Bool {
        independentModifierFlags.contains(.command)
            && independentModifierFlags.intersection([.control, .option, .shift]).isEmpty
            && charactersIgnoringModifiers?.lowercased() == key
    }

    func isCommandShiftShortcut(_ key: String) -> Bool {
        independentModifierFlags.contains(.command)
            && independentModifierFlags.contains(.shift)
            && independentModifierFlags.intersection([.control, .option]).isEmpty
            && charactersIgnoringModifiers?.lowercased() == key
    }
}
