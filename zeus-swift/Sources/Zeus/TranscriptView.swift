import SwiftUI

struct TranscriptView: View {
    let lines: [TranscriptLine]
    let activeAssistantStream: ActiveAssistantStream?
    let isCacheStatsVisible: Bool
    let searchMatchLineIDs: Set<UUID>
    let selectedSearchLineID: UUID?
    let scrollTarget: TranscriptScrollTarget?

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 10) {
                    ForEach(lines) { line in
                        TerminalLineView(
                            line: line,
                            streamingText: activeAssistantStream?.lineID == line.id
                                ? activeAssistantStream?.text
                                : nil,
                            isCacheStatsVisible: isCacheStatsVisible,
                            isSearchMatch: searchMatchLineIDs.contains(line.id),
                            isSelectedSearchMatch: selectedSearchLineID == line.id
                        )
                            .id(line.id)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.bottom, 6)
            }
            .scrollIndicators(.hidden)
            .onChange(of: scrollTarget) { _, target in
                guard selectedSearchLineID == nil else { return }
                guard let target else { return }
                proxy.scrollTo(target.lineID, anchor: .bottom)
            }
            .onChange(of: selectedSearchLineID) { _, lineID in
                guard let lineID else { return }
                withAnimation(.easeOut(duration: 0.12)) {
                    proxy.scrollTo(lineID, anchor: .center)
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

private struct TerminalLineView: View {
    let line: TranscriptLine
    let streamingText: String?
    let isCacheStatsVisible: Bool
    let isSearchMatch: Bool
    let isSelectedSearchMatch: Bool

    var body: some View {
        Group {
            if line.kind == .tool {
                HStack(alignment: .center, spacing: TerminalLayout.markerTextSpacing) {
                    toolPrefix
                        .frame(width: TerminalLayout.markerWidth, alignment: .leading)
                    lineText
                }
            } else {
                HStack(alignment: .firstTextBaseline, spacing: TerminalLayout.markerTextSpacing) {
                    prefix
                        .frame(width: TerminalLayout.markerWidth, alignment: .leading)
                    lineText
                }
            }
        }
        .padding(.vertical, isSearchMatch ? 2 : 0)
        .background(searchBackground)
    }

    @ViewBuilder
    private var searchBackground: some View {
        if isSelectedSearchMatch {
            Rectangle().fill(TerminalPalette.cyan.opacity(0.16))
        } else if isSearchMatch {
            Rectangle().fill(TerminalPalette.green.opacity(0.08))
        }
    }

    private var lineText: some View {
        Group {
            if line.kind == .tool, let toolCall = line.toolCall {
                ToolCallLine(toolCall: toolCall)
            } else if line.kind == .assistant {
                assistantLine
            } else {
                Text(line.text.isEmpty ? " " : line.text)
                    .foregroundStyle(textColor)
            }
        }
        .fixedSize(horizontal: false, vertical: true)
        .textSelection(.enabled)
    }

    private var assistantLine: some View {
        VStack(alignment: .leading, spacing: 4) {
            if line.isStreaming {
                let text = streamingText ?? line.text
                Text(text.isEmpty ? " " : text)
                    .foregroundStyle(TerminalPalette.primaryText)
            } else if let markdown = line.renderedMarkdown {
                TerminalMarkdownView(markdown: markdown)
            } else {
                Text(line.text.isEmpty ? " " : line.text)
                    .foregroundStyle(TerminalPalette.primaryText)
            }

            if isCacheStatsVisible {
                ForEach(line.cacheStats.indices, id: \.self) { index in
                    Text(line.cacheStats[index].displayText)
                        .font(CodexTypography.chatXSmall)
                        .foregroundStyle(TerminalPalette.dimText)
                }
            }
        }
    }

    @ViewBuilder
    private var prefix: some View {
        switch line.kind {
        case .user:
            Text(">")
                .foregroundStyle(TerminalPalette.cyan)
        case .assistant, .status, .tool:
            marker(color: TerminalPalette.green)
        case .error:
            marker(color: TerminalPalette.red)
        }
    }

    private var toolPrefix: some View {
        Circle()
            .fill(TerminalPalette.green)
            .frame(width: 7, height: 7)
    }

    private func marker(color: Color) -> some View {
        Circle()
            .fill(color)
            .frame(width: 7, height: 7)
            .alignmentGuide(.firstTextBaseline) { d in d[VerticalAlignment.center] + 5 }
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
        .font(CodexTypography.chatSmall)
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
                content()
                    .padding(.horizontal, horizontalPadding)
                    .frame(width: width, alignment: alignment)
                    .frame(minHeight: 23, alignment: .center)
            } else {
                content()
                    .padding(.horizontal, horizontalPadding)
                    .frame(minHeight: 23, alignment: .center)
            }
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
