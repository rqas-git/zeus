import SwiftUI
import ZeusCore

struct TerminalMarkdownView: View {
    let text: String

    private var blocks: [TerminalMarkdownBlock] {
        TerminalMarkdownParser.parse(text)
    }

    var body: some View {
        if blocks.isEmpty {
            Text(" ")
                .foregroundStyle(TerminalPalette.primaryText)
        } else {
            VStack(alignment: .leading, spacing: 7) {
                ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                    blockView(block)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    @ViewBuilder
    private func blockView(_ block: TerminalMarkdownBlock) -> some View {
        switch block {
        case let .paragraph(text):
            inlineText(text)
                .foregroundStyle(TerminalPalette.primaryText)
        case let .heading(level, text):
            inlineText(text)
                .font(.system(size: headingSize(level), weight: .semibold, design: .monospaced))
                .foregroundStyle(TerminalPalette.cyan)
                .padding(.top, level == 1 ? 1 : 0)
        case let .unorderedList(items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    listRow(marker: "-", text: item, markerWidth: 16)
                }
            }
        case let .orderedList(items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    listRow(marker: "\(item.number).", text: item.text, markerWidth: 28)
                }
            }
        case let .quote(lines):
            HStack(alignment: .top, spacing: 7) {
                Rectangle()
                    .fill(TerminalPalette.green.opacity(0.7))
                    .frame(width: 2)
                VStack(alignment: .leading, spacing: 3) {
                    ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                        inlineText(line)
                            .foregroundStyle(TerminalPalette.dimText)
                    }
                }
            }
            .padding(.vertical, 2)
        case let .codeBlock(language, code):
            CodeBlockView(language: language, code: code)
        case .rule:
            Rectangle()
                .fill(TerminalPalette.dimText.opacity(0.35))
                .frame(width: 220, height: 1)
                .padding(.vertical, 2)
        }
    }

    private func listRow(marker: String, text: String, markerWidth: CGFloat) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 5) {
            Text(marker)
                .foregroundStyle(TerminalPalette.cyan)
                .frame(width: markerWidth, alignment: .trailing)
            inlineText(text)
                .foregroundStyle(TerminalPalette.primaryText)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func inlineText(_ source: String) -> Text {
        let options = AttributedString.MarkdownParsingOptions(
            interpretedSyntax: .inlineOnlyPreservingWhitespace,
            failurePolicy: .returnPartiallyParsedIfPossible
        )
        if let attributed = try? AttributedString(markdown: source, options: options) {
            return Text(attributed)
        }
        return Text(source)
    }

    private func headingSize(_ level: Int) -> CGFloat {
        switch level {
        case 1:
            return 13
        case 2:
            return 12.5
        default:
            return 12
        }
    }
}

private struct CodeBlockView: View {
    let language: String?
    let code: String

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let language, !language.isEmpty {
                Text(language.lowercased())
                    .font(.system(size: 10, weight: .regular, design: .monospaced))
                    .foregroundStyle(TerminalPalette.cyan)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(TerminalPalette.cyan.opacity(0.045))
            }

            ScrollView(.horizontal, showsIndicators: false) {
                Text(code.isEmpty ? " " : code)
                    .font(.system(size: 11, weight: .regular, design: .monospaced))
                    .foregroundStyle(TerminalPalette.primaryText)
                    .fixedSize(horizontal: true, vertical: false)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 7)
            }
        }
        .background(
            Rectangle()
                .fill(Color(red: 0.015, green: 0.020, blue: 0.022).opacity(0.94))
        )
        .overlay(
            Rectangle()
                .stroke(TerminalPalette.dimText.opacity(0.38), lineWidth: 1)
        )
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
