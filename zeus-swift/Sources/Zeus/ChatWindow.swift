import SwiftUI

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
                    effort: viewModel.effort,
                    permissions: viewModel.permissions,
                    tokenUsage: viewModel.tokenUsage
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
        .foregroundStyle(TerminalColors.primaryText)
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
            Color(red: 0.025, green: 0.032, blue: 0.034)
            LinearGradient(
                colors: [
                    Color(red: 0.05, green: 0.075, blue: 0.078).opacity(0.75),
                    Color(red: 0.015, green: 0.019, blue: 0.02)
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
                    .foregroundStyle(TerminalColors.dimText)
                    .padding(.leading, 66)
                    .allowsHitTesting(false)

                Spacer()

                Button {
                    isShowingSettings.toggle()
                } label: {
                    Image(systemName: "gearshape")
                        .font(.system(size: 10, weight: .regular))
                        .foregroundStyle(
                            isShowingSettings ? TerminalColors.cyan : TerminalColors.dimText
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
                        .foregroundStyle(TerminalColors.cyan)
                        .frame(width: 12)

                    Text("Login Status")
                        .font(.system(size: 11, weight: .regular, design: .monospaced))
                        .foregroundStyle(TerminalColors.primaryText)

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
                .fill(Color(red: 0.025, green: 0.032, blue: 0.034))
        )
        .overlay(
            Rectangle()
                .stroke(TerminalColors.dimText.opacity(0.48), lineWidth: 1)
        )
        .shadow(color: .black.opacity(0.28), radius: 8, x: 0, y: 6)
    }
}

private struct TerminalMenuButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(
                Rectangle()
                    .fill(configuration.isPressed ? TerminalColors.cyan.opacity(0.12) : .clear)
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
    private let markerWidth: CGFloat = 10
    private let markerTextSpacing: CGFloat = 8

    var body: some View {
        if line.kind == .tool {
            HStack(alignment: .center, spacing: markerTextSpacing) {
                toolPrefix
                    .frame(width: markerWidth, alignment: .leading)
                lineText
            }
        } else {
            HStack(alignment: .top, spacing: markerTextSpacing) {
                prefix
                    .frame(width: markerWidth, alignment: .leading)
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
                .foregroundStyle(TerminalColors.cyan)
        case .assistant:
            marker(color: TerminalColors.green)
        case .status, .tool:
            marker(color: TerminalColors.green)
        case .error:
            marker(color: TerminalColors.red)
        }
    }

    private var toolPrefix: some View {
        marker(color: TerminalColors.green, topPadding: 0)
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
            return TerminalColors.red
        default:
            return TerminalColors.primaryText
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

            toolCell(width: 118, borderColor: TerminalColors.cyan.opacity(0.42)) {
                Text(toolCall.name)
                    .foregroundStyle(TerminalColors.cyan)
                    .fontWeight(.semibold)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }

            if let target = toolCall.target, !target.isEmpty {
                toolCell(borderColor: TerminalColors.dimText.opacity(0.38)) {
                    Text(target)
                        .foregroundStyle(TerminalColors.primaryText)
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
        maxWidth: CGFloat? = nil,
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
                        .frame(maxWidth: maxWidth, alignment: alignment)
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
                    .fill(TerminalColors.cyan.opacity(0.026))
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
            return TerminalColors.red
        default:
            return TerminalColors.cyan
        }
    }

    private var statusColor: Color {
        switch toolCall.status {
        case .running:
            return TerminalColors.dimText
        case .completed:
            return TerminalColors.green
        case .failed:
            return TerminalColors.red
        }
    }
}

private struct InputPrompt: View {
    @Binding var text: String
    let onSubmit: () -> Void
    private let markerWidth: CGFloat = 10
    private let markerTextSpacing: CGFloat = 8

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: markerTextSpacing) {
            Text(">")
                .foregroundStyle(TerminalColors.cyan)
                .frame(width: markerWidth, alignment: .leading)

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
    let effort: String
    let permissions: String
    let tokenUsage: String
    private let itemSpacing: CGFloat = 22
    private let pathSpacing: CGFloat = 32

    var body: some View {
        HStack(spacing: 0) {
            HStack(spacing: itemSpacing) {
                footerText(workspace.name, color: TerminalColors.dimText)
                footerText(workspace.branch, color: TerminalColors.green)
                footerText(model, color: TerminalColors.cyan)
                footerText(effort, color: TerminalColors.primaryText)
                footerText(permissions, color: TerminalColors.primaryText)
                footerText(tokenUsage, color: TerminalColors.dimText)
            }
            .layoutPriority(1)

            Spacer(minLength: pathSpacing)

            footerText(workspace.displayPath, color: TerminalColors.dimText)
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

private enum TerminalColors {
    static let primaryText = TerminalPalette.primaryText
    static let dimText = TerminalPalette.dimText
    static let cyan = TerminalPalette.cyan
    static let green = TerminalPalette.green
    static let red = TerminalPalette.red
}
