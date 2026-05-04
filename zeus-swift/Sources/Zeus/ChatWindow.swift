import SwiftUI

struct ChatWindow: View {
    @ObservedObject var viewModel: ChatViewModel

    var body: some View {
        ZStack {
            TerminalBackground()

            VStack(spacing: 0) {
                HeaderBar(onLoginStatus: viewModel.showLoginStatus)

                TranscriptView(lines: viewModel.lines)
                    .padding(.top, 12)

                InputPrompt(
                    text: $viewModel.draft,
                    onSubmit: viewModel.sendDraft
                )
                .padding(.top, 10)

                FooterBar(
                    workspace: viewModel.workspace,
                    model: viewModel.model,
                    effort: viewModel.effort,
                    permissions: viewModel.permissions,
                    tokenUsage: viewModel.tokenUsage
                )
                .padding(.top, 14)
            }
            .padding(.horizontal, 24)
            .padding(.top, 12)
            .padding(.bottom, 18)
        }
        .font(.system(size: 15, weight: .regular, design: .monospaced))
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
                    .font(.system(size: 13, weight: .regular))
                    .foregroundStyle(TerminalColors.dimText)
                    .frame(width: 20, height: 18)
                    .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Settings")
        }
        .frame(height: 18)
    }
}

private struct TranscriptView: View {
    let lines: [TranscriptLine]

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 13) {
                    ForEach(lines) { line in
                        TerminalLineView(line: line)
                            .id(line.id)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.bottom, 8)
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
    private let markerWidth: CGFloat = 12
    private let markerTextSpacing: CGFloat = 10

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
        Text(line.text.isEmpty ? " " : line.text)
            .foregroundStyle(textColor)
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
            .frame(width: 9, height: 9)
            .padding(.top, 5)
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

private struct InputPrompt: View {
    @Binding var text: String
    let onSubmit: () -> Void
    private let markerWidth: CGFloat = 12
    private let markerTextSpacing: CGFloat = 10

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
                .frame(height: 22)
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
                .frame(minWidth: 120, alignment: .leading)
            footerText(workspace.branch, color: TerminalColors.green)
                .frame(minWidth: 120, alignment: .leading)
            footerText(model, color: TerminalColors.cyan)
                .frame(minWidth: 150, alignment: .leading)
            footerText(effort, color: TerminalColors.primaryText)
                .frame(minWidth: 115, alignment: .leading)
            footerText(permissions, color: TerminalColors.primaryText)
                .frame(minWidth: 115, alignment: .leading)
            footerText(tokenUsage, color: TerminalColors.dimText)
                .frame(minWidth: 170, alignment: .leading)
            Spacer(minLength: 18)
            footerText(workspace.displayPath, color: TerminalColors.dimText)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .font(.system(size: 14, weight: .regular, design: .monospaced))
        .frame(height: 22)
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
