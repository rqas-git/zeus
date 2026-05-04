import SwiftUI

private enum TerminalLayout {
    static let markerWidth: CGFloat = 10
    static let markerTextSpacing: CGFloat = 8
}

struct ChatWindow: View {
    @ObservedObject var viewModel: ChatViewModel

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
                    permissions: viewModel.permissions,
                    tokenUsage: viewModel.tokenUsage,
                    modelTitle: { viewModel.displayModel($0) },
                    onSelectModel: viewModel.selectModel
                )
                .padding(.top, 11)
            }
            .padding(.horizontal, 19)
            .padding(.top, 10)
            .padding(.bottom, 14)
        }
        .ignoresSafeArea(.container, edges: .top)
        .background(WindowConfigurator())
        .font(.system(size: 12, weight: .regular, design: .monospaced))
        .foregroundStyle(TerminalPalette.primaryText)
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.willTerminateNotification)) { _ in
            viewModel.shutdown()
        }
        .onDisappear {
            viewModel.shutdown()
        }
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

    var body: some View {
        HStack(alignment: .center, spacing: 4) {
            toolCell(
                width: 24,
                borderColor: iconColor.opacity(0.42),
                horizontalPadding: 0,
                alignment: .center
            ) {
                Image(systemName: iconName)
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(iconColor)
                    .frame(width: 14, height: 13, alignment: .center)
            }

            toolCell(width: 76, borderColor: statusColor.opacity(0.52)) {
                Text(statusText)
                    .foregroundStyle(statusColor)
            }

            toolCell(width: 118, borderColor: TerminalPalette.cyan.opacity(0.42)) {
                Text(toolCall.name)
                    .foregroundStyle(TerminalPalette.cyan)
                    .fontWeight(.semibold)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }

            if let target = toolCall.target, !target.isEmpty {
                toolCell(borderColor: TerminalPalette.dimText.opacity(0.38)) {
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
        borderColor: Color,
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
                        .frame(minHeight: 23, alignment: .center),
                    borderColor: borderColor
                )
            } else {
                toolCellStyle(
                    content()
                        .padding(.horizontal, horizontalPadding)
                        .frame(minHeight: 23, alignment: .center),
                    borderColor: borderColor
                )
            }
        }
    }

    private func toolCellStyle<Content: View>(
        _ content: Content,
        borderColor: Color
    ) -> some View {
        content
            .background(
                Rectangle()
                    .fill(TerminalPalette.cyan.opacity(0.026))
            )
            .overlay(
                Rectangle()
                    .stroke(borderColor, lineWidth: 1)
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
    let permissions: String
    let tokenUsage: String
    let modelTitle: (String) -> String
    let onSelectModel: (String) -> Void
    private let itemSpacing: CGFloat = 22
    private let pathSpacing: CGFloat = 32

    var body: some View {
        HStack(spacing: 0) {
            HStack(spacing: itemSpacing) {
                footerText(workspace.name, color: TerminalPalette.dimText)
                footerText(workspace.branch, color: TerminalPalette.green)
                ModelFooterMenu(
                    title: model,
                    options: modelOptions,
                    selectedModel: selectedModel,
                    isEnabled: isModelMenuEnabled,
                    modelTitle: modelTitle,
                    onSelect: onSelectModel
                )
                footerText(effort, color: TerminalPalette.primaryText)
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

private struct ModelFooterMenu: View {
    let title: String
    let options: [String]
    let selectedModel: String
    let isEnabled: Bool
    let modelTitle: (String) -> String
    let onSelect: (String) -> Void
    @State private var isOpen = false

    private var menuOptions: [String] {
        options.isEmpty ? [selectedModel] : options
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
                isOpen.toggle()
            }
            .help("Model")
            .overlay(alignment: .bottom) {
                if isOpen {
                    ModelDropdown(
                        options: menuOptions,
                        selectedModel: selectedModel,
                        modelTitle: modelTitle
                    ) { model in
                        isOpen = false
                        onSelect(model)
                    }
                    .offset(y: -23)
                    .zIndex(30)
                }
            }
            .onChange(of: isEnabled) { newValue in
                if !newValue {
                    isOpen = false
                }
            }
            .zIndex(isOpen ? 30 : 0)
    }
}

private struct ModelDropdown: View {
    let options: [String]
    let selectedModel: String
    let modelTitle: (String) -> String
    let onSelect: (String) -> Void

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(options, id: \.self) { option in
                    Button {
                        onSelect(option)
                    } label: {
                        HStack(spacing: 7) {
                            if option == selectedModel {
                                Image(systemName: "checkmark")
                                    .font(.system(size: 10, weight: .medium))
                                    .foregroundStyle(TerminalPalette.cyan)
                                    .frame(width: 12)
                            } else {
                                Color.clear
                                    .frame(width: 12, height: 10)
                            }

                            Text(modelTitle(option))
                                .foregroundStyle(
                                    option == selectedModel
                                        ? TerminalPalette.cyan
                                        : TerminalPalette.primaryText
                                )
                                .lineLimit(1)

                            Spacer(minLength: 0)
                        }
                        .padding(.horizontal, 9)
                        .padding(.vertical, 6)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(TerminalMenuButtonStyle())
                }
            }
            .frame(width: 178)
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
}
