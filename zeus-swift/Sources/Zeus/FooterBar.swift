import SwiftUI

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

struct FooterMenuConfig {
    let id: FooterMenuID
    let title: String
    let options: [String]
    let selectedOption: String
    let isEnabled: Bool
    let enabledColor: Color
    let highlightedOption: String?
    let optionTitle: (String) -> String
    let help: String
    let onSelect: (String) -> Void
    let onHighlight: (String) -> Void

    init(
        id: FooterMenuID,
        title: String,
        options: [String],
        selectedOption: String,
        isEnabled: Bool,
        enabledColor: Color = TerminalPalette.cyan,
        highlightedOption: String?,
        optionTitle: @escaping (String) -> String = { $0 },
        help: String,
        onSelect: @escaping (String) -> Void,
        onHighlight: @escaping (String) -> Void
    ) {
        self.id = id
        self.title = title
        self.options = options
        self.selectedOption = selectedOption
        self.isEnabled = isEnabled
        self.enabledColor = enabledColor
        self.highlightedOption = highlightedOption
        self.optionTitle = optionTitle
        self.help = help
        self.onSelect = onSelect
        self.onHighlight = onHighlight
    }
}

struct FooterBar: View {
    let workspace: WorkspaceMetadata
    let branch: FooterMenuConfig
    let model: FooterMenuConfig
    let effort: FooterMenuConfig
    let permissions: FooterMenuConfig
    let tokenUsage: String
    @Binding var activeMenu: FooterMenuID?
    @Binding var focusedMenu: FooterMenuID?
    private let itemSpacing: CGFloat = 22
    private let pathSpacing: CGFloat = 32

    var body: some View {
        HStack(spacing: 0) {
            HStack(spacing: itemSpacing) {
                footerText(workspace.name, color: TerminalPalette.dimText)
                FooterMenu(config: branch, activeMenu: $activeMenu, focusedMenu: $focusedMenu)
                FooterMenu(config: model, activeMenu: $activeMenu, focusedMenu: $focusedMenu)
                FooterMenu(config: effort, activeMenu: $activeMenu, focusedMenu: $focusedMenu)
                FooterMenu(config: permissions, activeMenu: $activeMenu, focusedMenu: $focusedMenu)
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
        .font(CodexTypography.chatSmall)
        .frame(height: 20)
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
            .font(CodexTypography.chatSmallBold)
            .foregroundStyle(TerminalPalette.cyan)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .frame(height: 20, alignment: .center)
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
        .font(CodexTypography.chatSmall)
        .fixedSize(horizontal: true, vertical: true)
    }
}

private struct FooterMenu: View {
    let config: FooterMenuConfig
    @Binding var activeMenu: FooterMenuID?
    @Binding var focusedMenu: FooterMenuID?

    private var menuOptions: [String] {
        config.options.isEmpty ? [config.selectedOption] : config.options
    }

    private var isOpen: Bool { activeMenu == config.id }
    private var isFocused: Bool { focusedMenu == config.id }

    var body: some View {
        Text(config.title)
            .font(CodexTypography.chatSmallBold)
            .foregroundStyle(config.isEnabled ? config.enabledColor : TerminalPalette.dimText)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .frame(height: 20, alignment: .center)
            .padding(.horizontal, 3)
            .background(
                Rectangle()
                    .fill(isFocused ? config.enabledColor.opacity(0.12) : .clear)
            )
            .contentShape(Rectangle())
            .onTapGesture {
                guard config.isEnabled else { return }
                if isOpen {
                    activeMenu = nil
                    focusedMenu = config.id
                } else {
                    DispatchQueue.main.async {
                        focusedMenu = config.id
                        activeMenu = config.id
                    }
                }
            }
            .help(config.help)
            .overlay(alignment: .bottom) {
                if isOpen {
                    FooterDropdown(
                        options: menuOptions,
                        selectedOption: config.selectedOption,
                        highlightedOption: config.highlightedOption,
                        optionTitle: config.optionTitle
                    ) { option in
                        activeMenu = nil
                        focusedMenu = nil
                        config.onSelect(option)
                    } onHighlight: { option in
                        config.onHighlight(option)
                    }
                    .offset(y: -23)
                    .zIndex(30)
                }
            }
            .onChange(of: config.isEnabled) { _, newValue in
                if !newValue {
                    activeMenu = nil
                    if focusedMenu == config.id { focusedMenu = nil }
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
    let onSelect: (String) -> Void
    let onHighlight: (String) -> Void

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(options, id: \.self) { option in
                    dropdownButton(for: option)
                }
            }
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
        .font(CodexTypography.chatSmall)
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
            }
            .padding(.horizontal, 9)
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
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
