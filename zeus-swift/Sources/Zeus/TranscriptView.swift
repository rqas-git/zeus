import Foundation
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
                        if let separator = item.separatorBefore {
                            CodexTranscriptSeparator(separator: separator)
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
                        if let separator = item.separatorAfter {
                            CodexTranscriptSeparator(separator: separator)
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
                separatorBefore: Self.showsToolResponseSeparator(
                    before: index,
                    in: lines
                ) ? .plain : nil,
                separatorAfter: Self.separatorAfter(
                    index: index,
                    startupBlockLastIndex: startupBlockLastIndex,
                    in: lines
                )
            )
        }
    }

    private static func separatorAfter(
        index: Int,
        startupBlockLastIndex: Int?,
        in lines: [TranscriptLine]
    ) -> TranscriptSeparator? {
        if let durationMS = lines[index].responseDurationMS {
            return .workedFor(durationMS: durationMS)
        }
        if startupBlockLastIndex == index, index + 1 < lines.count {
            return .plain
        }
        return nil
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
              first.text.hasPrefix("Session ID: ") else {
            return nil
        }

        var index = 0
        while index + 1 < lines.count {
            let next = lines[index + 1]
            if isStartupAuthStatusLine(next) {
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
    let separatorBefore: TranscriptSeparator?
    let separatorAfter: TranscriptSeparator?

    var id: UUID {
        line.id
    }
}

private enum TranscriptSeparator: Equatable {
    case plain
    case workedFor(durationMS: UInt64)
}

private struct CodexTranscriptSeparator: View {
    private static let rule = String(repeating: "─", count: 240)
    let separator: TranscriptSeparator

    var body: some View {
        Text(text)
            .font(TerminalTypography.chat)
            .foregroundStyle(TerminalPalette.dimText.opacity(0.72))
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .frame(maxWidth: .infinity, alignment: .leading)
            .clipped()
            .accessibilityHidden(true)
    }

    private var text: String {
        switch separator {
        case .plain:
            return Self.rule
        case let .workedFor(durationMS):
            return "─ Worked for \(Self.formatElapsed(milliseconds: durationMS)) " + Self.rule
        }
    }

    private static func formatElapsed(milliseconds: UInt64) -> String {
        let seconds = milliseconds / 1_000
        if seconds < 60 {
            return "\(seconds)s"
        }
        if seconds < 3_600 {
            return "\(seconds / 60)m \(String(format: "%02d", Int(seconds % 60)))s"
        }
        return "\(seconds / 3_600)h "
            + "\(String(format: "%02d", Int((seconds % 3_600) / 60)))m "
            + "\(String(format: "%02d", Int(seconds % 60)))s"
    }
}

private struct TerminalLineView: View, Equatable {
    private static let messageMarkerSize: CGFloat = 5

    let line: TranscriptLine
    let streamingStream: ActiveAssistantStream?
    let isCacheStatsVisible: Bool
    let isSearchMatch: Bool
    let isSelectedSearchMatch: Bool

    var body: some View {
        Group {
            if isSessionStatusLine {
                sessionStatusBadge
            } else if line.kind == .tool {
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

    private var isSessionStatusLine: Bool {
        line.kind == .status && line.text.hasPrefix("Session ID: ")
    }

    private var sessionStatusBadge: some View {
        Text(line.text)
            .font(TerminalTypography.chatSmallBold)
            .foregroundStyle(TerminalPalette.dimText)
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
            .fill(toolPrefixColor)
            .frame(width: Self.messageMarkerSize, height: Self.messageMarkerSize)
            .frame(width: TerminalLayout.markerWidth, alignment: .center)
    }

    private var toolPrefixColor: Color {
        line.toolCall?.status == .failed ? TerminalPalette.red : TerminalPalette.green
    }

    private func marker(color: Color) -> some View {
        Circle()
            .fill(color)
            .frame(width: Self.messageMarkerSize, height: Self.messageMarkerSize)
            .frame(width: TerminalLayout.markerWidth, alignment: .center)
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
