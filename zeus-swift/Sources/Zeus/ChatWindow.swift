import AppKit
import SwiftUI

private enum TerminalLayout {
    static let markerWidth: CGFloat = 10
    static let markerTextSpacing: CGFloat = 8
}

private enum FooterMenuID {
    case model
    case effort
}

private enum KeyCode {
    static let returnKey: UInt16 = 36
    static let escape: UInt16 = 53
    static let keypadEnter: UInt16 = 76
    static let downArrow: UInt16 = 125
    static let upArrow: UInt16 = 126
}

struct ChatWindow: View {
    @ObservedObject var viewModel: ChatViewModel
    @State private var activeFooterMenu: FooterMenuID?
    @State private var modelMenuHighlightedOption: String?
    @State private var effortMenuHighlightedOption: String?

    var body: some View {
        ZStack {
            TerminalBackground()

            VStack(spacing: 0) {
                HeaderBar(onLoginStatus: viewModel.showLoginStatus)

                TranscriptView(lines: viewModel.lines)
                    .padding(.top, 10)

                InputPrompt(
                    text: $viewModel.draft,
                    onSubmit: viewModel.sendDraft
                )
                .padding(.top, 8)

                FooterBar(
                    workspace: viewModel.workspace,
                    model: viewModel.model,
                    modelOptions: viewModel.modelOptions,
                    selectedModel: viewModel.selectedModel,
                    isModelMenuEnabled: viewModel.canChangeModel,
                    effort: viewModel.effort,
                    effortOptions: viewModel.effortOptions,
                    isEffortMenuEnabled: viewModel.canChangeEffort,
                    permissions: viewModel.permissions,
                    tokenUsage: viewModel.tokenUsage,
                    activeMenu: $activeFooterMenu,
                    modelHighlightedOption: modelMenuHighlightedOption,
                    effortHighlightedOption: effortMenuHighlightedOption,
                    modelTitle: { viewModel.displayModel($0) },
                    onSelectModel: viewModel.selectModel,
                    onSelectEffort: viewModel.selectEffort,
                    onHighlightModel: { modelMenuHighlightedOption = $0 },
                    onHighlightEffort: { effortMenuHighlightedOption = $0 }
                )
                .padding(.top, 11)
            }
            .padding(.horizontal, 19)
            .padding(.top, 10)
            .padding(.bottom, 14)
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
            if activeFooterMenu != nil {
                DispatchQueue.main.async {
                    activeFooterMenu = nil
                }
            }
            return false
        default:
            return false
        }
    }

    private func handleKeyDown(_ event: NSEvent) -> Bool {
        if isModelShortcut(event) {
            openModelMenu()
            return true
        }
        if isEffortShortcut(event) {
            openEffortMenu()
            return true
        }

        guard activeFooterMenu != nil else { return false }

        switch event.keyCode {
        case KeyCode.escape:
            activeFooterMenu = nil
            return true
        case KeyCode.downArrow:
            moveActiveMenuHighlight(by: 1)
            return true
        case KeyCode.upArrow:
            moveActiveMenuHighlight(by: -1)
            return true
        case KeyCode.returnKey, KeyCode.keypadEnter:
            return selectActiveMenuOption()
        default:
            return false
        }
    }

    private func isModelShortcut(_ event: NSEvent) -> Bool {
        isCommandShortcut(event, key: "m")
    }

    private func isEffortShortcut(_ event: NSEvent) -> Bool {
        isCommandShortcut(event, key: "e")
    }

    private func isCommandShortcut(_ event: NSEvent, key: String) -> Bool {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let disallowed: NSEvent.ModifierFlags = [.control, .option, .shift]
        return flags.contains(.command)
            && flags.intersection(disallowed).isEmpty
            && event.charactersIgnoringModifiers?.lowercased() == key
    }

    private func openModelMenu() {
        guard viewModel.canChangeModel else { return }
        let options = modelMenuOptions
        modelMenuHighlightedOption = options.contains(viewModel.selectedModel)
            ? viewModel.selectedModel
            : options.first
        activeFooterMenu = .model
    }

    private func openEffortMenu() {
        guard viewModel.canChangeEffort else { return }
        let options = effortMenuOptions
        effortMenuHighlightedOption = options.contains(viewModel.effort)
            ? viewModel.effort
            : options.first
        activeFooterMenu = .effort
    }

    private func moveActiveMenuHighlight(by offset: Int) {
        switch activeFooterMenu {
        case .model:
            moveModelHighlight(by: offset)
        case .effort:
            moveEffortHighlight(by: offset)
        case nil:
            break
        }
    }

    private func selectActiveMenuOption() -> Bool {
        switch activeFooterMenu {
        case .model:
            guard let model = modelMenuHighlightedOption ?? modelMenuOptions.first else {
                return false
            }
            activeFooterMenu = nil
            viewModel.selectModel(model)
            return true
        case .effort:
            guard let effort = effortMenuHighlightedOption ?? effortMenuOptions.first else {
                return false
            }
            activeFooterMenu = nil
            viewModel.selectEffort(effort)
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

    private func moveEffortHighlight(by offset: Int) {
        let options = effortMenuOptions
        let current = effortMenuHighlightedOption ?? viewModel.effort
        effortMenuHighlightedOption = nextMenuOption(in: options, current: current, offset: offset)
    }

    private func nextMenuOption(in options: [String], current: String, offset: Int) -> String? {
        guard !options.isEmpty else { return nil }
        let currentIndex = options.firstIndex(of: current) ?? 0
        let nextIndex = (currentIndex + offset + options.count) % options.count
        return options[nextIndex]
    }

    private var modelMenuOptions: [String] {
        viewModel.modelOptions.isEmpty ? [viewModel.selectedModel] : viewModel.modelOptions
    }

    private var effortMenuOptions: [String] {
        viewModel.effortOptions.isEmpty ? [viewModel.effort] : viewModel.effortOptions
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
                .strokeBorder(Color.white.opacity(0.06), lineWidth: 1)
        }
        .ignoresSafeArea()
    }
}

private struct HeaderBar: View {
    let onLoginStatus: () -> Void
    @State private var isShowingSettings = false

    var body: some View {
        ZStack(alignment: .topTrailing) {
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

            if isShowingSettings {
                SettingsDropdown {
                    isShowingSettings = false
                    onLoginStatus()
                }
                .offset(y: 20)
                .zIndex(20)
            }
        }
        .frame(height: 16)
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
                .stroke(TerminalPalette.dimText.opacity(0.48), lineWidth: 1)
        )
        .shadow(color: .black.opacity(0.28), radius: 8, x: 0, y: 6)
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

private struct TranscriptView: View {
    let lines: [TranscriptLine]

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    ForEach(lines) { line in
                        TerminalLineView(line: line)
                            .id(line.id)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.bottom, 6)
            }
            .scrollIndicators(.hidden)
            .onChange(of: lines) { newLines in
                guard let last = newLines.last else { return }
                withAnimation(.easeOut(duration: 0.16)) {
                    proxy.scrollTo(last.id, anchor: .bottom)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct TerminalLineView: View {
    let line: TranscriptLine

    var body: some View {
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

    private var lineText: some View {
        Group {
            if line.kind == .tool, let toolCall = line.toolCall {
                ToolCallLine(toolCall: toolCall)
            } else if line.kind == .assistant {
                TerminalMarkdownView(text: line.text)
            } else {
                Text(line.text.isEmpty ? " " : line.text)
                    .foregroundStyle(textColor)
            }
        }
        .fixedSize(horizontal: false, vertical: true)
        .textSelection(.enabled)
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
    let onSubmit: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: TerminalLayout.markerTextSpacing) {
            Text(">")
                .foregroundStyle(TerminalPalette.cyan)
                .frame(width: TerminalLayout.markerWidth, alignment: .leading)

            PromptTextField(
                text: $text,
                placeholder: "type a command or ask anything...",
                onSubmit: onSubmit
            )
                .frame(height: 18)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }
}

private struct FooterBar: View {
    let workspace: WorkspaceMetadata
    let model: String
    let modelOptions: [String]
    let selectedModel: String
    let isModelMenuEnabled: Bool
    let effort: String
    let effortOptions: [String]
    let isEffortMenuEnabled: Bool
    let permissions: String
    let tokenUsage: String
    @Binding var activeMenu: FooterMenuID?
    let modelHighlightedOption: String?
    let effortHighlightedOption: String?
    let modelTitle: (String) -> String
    let onSelectModel: (String) -> Void
    let onSelectEffort: (String) -> Void
    let onHighlightModel: (String) -> Void
    let onHighlightEffort: (String) -> Void
    private let itemSpacing: CGFloat = 22
    private let pathSpacing: CGFloat = 32

    var body: some View {
        HStack(spacing: 0) {
            HStack(spacing: itemSpacing) {
                footerText(workspace.name, color: TerminalPalette.dimText)
                footerText(workspace.branch, color: TerminalPalette.green)
                FooterMenu(
                    id: .model,
                    title: model,
                    options: modelOptions,
                    selectedOption: selectedModel,
                    highlightedOption: modelHighlightedOption,
                    isEnabled: isModelMenuEnabled,
                    activeMenu: $activeMenu,
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
                    optionTitle: { $0 },
                    menuWidth: 88,
                    help: "Reasoning Effort",
                    onSelect: onSelectEffort,
                    onHighlight: onHighlightEffort
                )
                footerText(permissions, color: TerminalPalette.primaryText)
                footerText(tokenUsage, color: TerminalPalette.dimText)
            }
            .layoutPriority(1)

            Spacer(minLength: pathSpacing)

            footerText(workspace.displayPath, color: TerminalPalette.dimText)
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: 260, alignment: .trailing)
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

private struct FooterMenu: View {
    let id: FooterMenuID
    let title: String
    let options: [String]
    let selectedOption: String
    let highlightedOption: String?
    let isEnabled: Bool
    @Binding var activeMenu: FooterMenuID?
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

    var body: some View {
        Text(title)
            .foregroundStyle(isEnabled ? TerminalPalette.cyan : TerminalPalette.dimText)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .frame(height: 18, alignment: .center)
            .contentShape(Rectangle())
            .onTapGesture {
                guard isEnabled else { return }
                if isOpen {
                    activeMenu = nil
                } else {
                    DispatchQueue.main.async {
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
                    .stroke(TerminalPalette.dimText.opacity(0.48), lineWidth: 1)
            )
            .shadow(color: .black.opacity(0.28), radius: 8, x: 0, y: 6)

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
