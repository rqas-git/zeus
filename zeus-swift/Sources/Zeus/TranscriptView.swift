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
                    ForEach(decoratedLines) { item in
                        if item.showsSeparatorBefore {
                            CodexTranscriptSeparator()
                        }
                        TerminalLineView(
                            line: item.line,
                            streamingStream: activeAssistantStream?.lineID == item.line.id
                                ? activeAssistantStream
                                : nil,
                            isCacheStatsVisible: isCacheStatsVisible,
                            isSearchMatch: searchMatchLineIDs.contains(item.line.id),
                            isSelectedSearchMatch: selectedSearchLineID == item.line.id
                        )
                            .equatable()
                            .id(item.line.id)
                        if item.showsSeparatorAfter {
                            CodexTranscriptSeparator()
                        }
                    }
                    Color.clear
                        .frame(height: 1)
                        .id(TranscriptScrollID.bottom)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.bottom, 6)
            }
            .scrollIndicators(.hidden)
            .onChange(of: scrollTarget) { _, target in
                guard selectedSearchLineID == nil else { return }
                guard target != nil else { return }
                Task { @MainActor in
                    await Task.yield()
                    proxy.scrollTo(TranscriptScrollID.bottom, anchor: .bottom)
                }
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

    private var decoratedLines: [TranscriptDecoratedLine] {
        let startupBlockLastIndex = Self.startupBlockLastIndex(in: lines)
        return lines.indices.map { index in
            TranscriptDecoratedLine(
                line: lines[index],
                showsSeparatorBefore: Self.showsToolResponseSeparator(
                    before: index,
                    in: lines
                ),
                showsSeparatorAfter: startupBlockLastIndex == index
                    && index + 1 < lines.count
            )
        }
    }

    private static func showsToolResponseSeparator(
        before index: Int,
        in lines: [TranscriptLine]
    ) -> Bool {
        guard index > 0, lines.indices.contains(index) else { return false }
        return lines[index].kind == .assistant
            && lines[index - 1].kind == .tool
    }

    private static func startupBlockLastIndex(in lines: [TranscriptLine]) -> Int? {
        guard let first = lines.first,
              first.kind == .status,
              first.text == "starting server..." else {
            return nil
        }

        var index = 0
        var sawReady = false
        while index + 1 < lines.count {
            let next = lines[index + 1]
            if isStartupProgressLine(next) {
                index += 1
            } else if isStartupReadyLine(next) {
                index += 1
                sawReady = true
            } else if sawReady, isStartupAuthStatusLine(next) {
                index += 1
            } else if next.kind == .error {
                index += 1
                break
            } else {
                break
            }
        }

        return index
    }

    private static func isStartupProgressLine(_ line: TranscriptLine) -> Bool {
        guard line.kind == .status else { return false }
        return line.text == "creating session..."
            || line.text == "connecting session events..."
    }

    private static func isStartupReadyLine(_ line: TranscriptLine) -> Bool {
        line.kind == .status && line.text.hasPrefix("ready. session ")
    }

    private static func isStartupAuthStatusLine(_ line: TranscriptLine) -> Bool {
        line.kind == .status
            && line.text == "not logged in. type /login to authorize rust-agent"
    }
}

private enum TranscriptScrollID {
    static let bottom = "transcript-bottom"
}

private struct TranscriptDecoratedLine: Identifiable {
    let line: TranscriptLine
    let showsSeparatorBefore: Bool
    let showsSeparatorAfter: Bool

    var id: UUID {
        line.id
    }
}

private struct CodexTranscriptSeparator: View {
    private static let rule = String(repeating: "─", count: 240)

    var body: some View {
        Text(Self.rule)
            .font(TerminalTypography.chat)
            .foregroundStyle(TerminalPalette.dimText.opacity(0.72))
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .frame(maxWidth: .infinity, alignment: .leading)
            .clipped()
            .accessibilityHidden(true)
    }
}

private struct TerminalLineView: View, Equatable {
    let line: TranscriptLine
    let streamingStream: ActiveAssistantStream?
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

    @ViewBuilder
    private var lineText: some View {
        if line.isStreaming {
            lineTextContent
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.disabled)
        } else {
            lineTextContent
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
        }
    }

    @ViewBuilder
    private var lineTextContent: some View {
        if line.kind == .tool, let toolCall = line.toolCall {
            ToolCallLine(toolCall: toolCall)
        } else if line.kind == .assistant {
            assistantLine
        } else {
            Text(line.text.isEmpty ? " " : line.text)
                .foregroundStyle(textColor)
        }
    }

    private var assistantLine: some View {
        VStack(alignment: .leading, spacing: 4) {
            if line.isStreaming {
                StreamingAssistantText(stream: streamingStream, fallbackText: line.text)
            } else if let markdown = line.renderedMarkdown {
                TerminalMarkdownView(markdown: markdown)
            } else {
                Text(line.text.isEmpty ? " " : line.text)
                    .foregroundStyle(TerminalPalette.primaryText)
            }

            if isCacheStatsVisible {
                ForEach(line.cacheStats.indices, id: \.self) { index in
                    Text(line.cacheStats[index].displayText)
                        .font(TerminalTypography.chatXSmall)
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

private struct StreamingAssistantText: View {
    let stream: ActiveAssistantStream?
    let fallbackText: String

    var body: some View {
        if let stream {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(stream.chunks) { chunk in
                    text(chunk.text)
                }
                if !stream.tail.isEmpty || stream.chunks.isEmpty {
                    text(stream.tail)
                }
            }
        } else {
            text(fallbackText)
        }
    }

    private func text(_ value: String) -> some View {
        Text(value.isEmpty ? " " : value)
            .foregroundStyle(TerminalPalette.primaryText)
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
        .font(TerminalTypography.chatSmall)
        .fixedSize(horizontal: false, vertical: true)
        .terminalPanelChrome()
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
