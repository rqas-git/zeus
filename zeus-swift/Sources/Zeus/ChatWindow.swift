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
        .font(.system(size: 12, weight: .regular, design: .monospaced))
        .foregroundStyle(TerminalColors.primaryText)
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

    var body: some View {
        HStack {
            Spacer()
            Menu {
                Button("Login Status", action: onLoginStatus)
            } label: {
                Image(systemName: "gearshape")
                    .font(.system(size: 10, weight: .regular))
                    .foregroundStyle(TerminalColors.dimText)
                    .frame(width: 16, height: 14)
                    .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Settings")
        }
        .frame(height: 14)
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
        switch line.kind {
        case .user, .assistant:
            HStack(alignment: .top, spacing: markerTextSpacing) {
                prefix
                    .frame(width: markerWidth, alignment: .leading)
                lineText
            }
        default:
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

    private func marker(color: Color) -> some View {
        Circle()
            .fill(color)
            .frame(width: 7, height: 7)
            .padding(.top, 4)
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
        HStack(alignment: .center, spacing: 7) {
            Image(systemName: iconName)
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(iconColor)
                .frame(width: 13, height: 13)

            Text(statusText)
                .foregroundStyle(statusColor)

            Rectangle()
                .fill(TerminalColors.dimText.opacity(0.35))
                .frame(width: 1, height: 11)

            Text(toolCall.name)
                .foregroundStyle(TerminalColors.cyan)
                .fontWeight(.semibold)

            if let target = toolCall.target, !target.isEmpty {
                Text(":")
                    .foregroundStyle(TerminalColors.dimText)

                Text(target)
                    .foregroundStyle(TerminalColors.primaryText)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
        .font(.system(size: 11, weight: .regular, design: .monospaced))
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 5, style: .continuous)
                .fill(TerminalColors.cyan.opacity(0.035))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 5, style: .continuous)
                .stroke(borderColor, lineWidth: 1)
        )
        .overlay(alignment: .leading) {
            Rectangle()
                .fill(statusColor.opacity(0.9))
                .frame(width: 2)
                .clipShape(
                    UnevenRoundedRectangle(
                        topLeadingRadius: 5,
                        bottomLeadingRadius: 5
                    )
                )
        }
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
        switch toolCall.name {
        case "read_file", "read_file_range":
            return "doc.text"
        case "list_dir":
            return "folder"
        case "search_files", "search_text":
            return "magnifyingglass"
        case "apply_patch":
            return "square.and.pencil"
        case "exec_command":
            return "terminal"
        case "git_add":
            return "plus.square"
        case "git_restore":
            return "arrow.uturn.backward.square"
        case "git_diff":
            return "arrow.left.arrow.right"
        case "git_log":
            return "clock"
        case "git_query", "git_status":
            return "checklist"
        case "git_commit":
            return "arrow.trianglehead.branch"
        default:
            return "wrench.and.screwdriver"
        }
    }

    private var iconColor: Color {
        switch toolCall.status {
        case .failed:
            return TerminalColors.red
        default:
            return TerminalColors.cyan
        }
    }

    private var borderColor: Color {
        switch toolCall.status {
        case .running:
            return TerminalColors.dimText.opacity(0.32)
        case .completed:
            return TerminalColors.green.opacity(0.45)
        case .failed:
            return TerminalColors.red.opacity(0.55)
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

    var body: some View {
        HStack(spacing: 0) {
            footerText(workspace.name, color: TerminalColors.dimText)
                .frame(minWidth: 96, alignment: .leading)
            footerText(workspace.branch, color: TerminalColors.green)
                .frame(minWidth: 96, alignment: .leading)
            footerText(model, color: TerminalColors.cyan)
                .frame(minWidth: 120, alignment: .leading)
            footerText(effort, color: TerminalColors.primaryText)
                .frame(minWidth: 92, alignment: .leading)
            footerText(permissions, color: TerminalColors.primaryText)
                .frame(minWidth: 92, alignment: .leading)
            footerText(tokenUsage, color: TerminalColors.dimText)
                .frame(minWidth: 136, alignment: .leading)
            Spacer(minLength: 14)
            footerText(workspace.displayPath, color: TerminalColors.dimText)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .font(.system(size: 11, weight: .regular, design: .monospaced))
        .frame(height: 18)
    }

    private func footerText(_ text: String, color: Color) -> some View {
        Text(text)
            .foregroundStyle(color)
            .fixedSize(horizontal: false, vertical: true)
    }
}

private enum TerminalColors {
    static let primaryText = TerminalPalette.primaryText
    static let dimText = TerminalPalette.dimText
    static let cyan = TerminalPalette.cyan
    static let green = TerminalPalette.green
    static let red = TerminalPalette.red
}
