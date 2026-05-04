import SwiftUI

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

private enum TerminalMarkdownBlock {
    case paragraph(String)
    case heading(level: Int, text: String)
    case unorderedList([String])
    case orderedList([(number: Int, text: String)])
    case quote([String])
    case codeBlock(language: String?, code: String)
    case rule
}

private enum TerminalMarkdownParser {
    static func parse(_ source: String) -> [TerminalMarkdownBlock] {
        let normalized = source
            .replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
        let lines = normalized.components(separatedBy: "\n")
        var blocks: [TerminalMarkdownBlock] = []
        var index = 0

        while index < lines.count {
            let trimmed = trim(lines[index])
            if trimmed.isEmpty {
                index += 1
                continue
            }

            if let fence = fenceMarker(trimmed) {
                blocks.append(parseCodeBlock(lines: lines, index: &index, fence: fence))
                continue
            }

            if let heading = parseHeading(trimmed) {
                blocks.append(.heading(level: heading.level, text: heading.text))
                index += 1
                continue
            }

            if isRule(trimmed) {
                blocks.append(.rule)
                index += 1
                continue
            }

            if isQuote(trimmed) {
                blocks.append(parseQuote(lines: lines, index: &index))
                continue
            }

            if unorderedItemText(trimmed) != nil {
                blocks.append(parseUnorderedList(lines: lines, index: &index))
                continue
            }

            if orderedItem(trimmed) != nil {
                blocks.append(parseOrderedList(lines: lines, index: &index))
                continue
            }

            blocks.append(parseParagraph(lines: lines, index: &index))
        }

        return blocks
    }

    private static func parseCodeBlock(
        lines: [String],
        index: inout Int,
        fence: String
    ) -> TerminalMarkdownBlock {
        let opening = trim(lines[index])
        let language = trim(String(opening.dropFirst(fence.count)))
        index += 1

        var codeLines: [String] = []
        while index < lines.count {
            if trim(lines[index]).hasPrefix(fence) {
                index += 1
                break
            }
            codeLines.append(lines[index])
            index += 1
        }

        return .codeBlock(
            language: language.isEmpty ? nil : language,
            code: codeLines.joined(separator: "\n")
        )
    }

    private static func parseQuote(lines: [String], index: inout Int) -> TerminalMarkdownBlock {
        var quoteLines: [String] = []
        while index < lines.count {
            let trimmed = trim(lines[index])
            guard isQuote(trimmed) else { break }
            var text = String(trimmed.dropFirst())
            if text.hasPrefix(" ") {
                text.removeFirst()
            }
            quoteLines.append(text)
            index += 1
        }
        return .quote(quoteLines)
    }

    private static func parseUnorderedList(
        lines: [String],
        index: inout Int
    ) -> TerminalMarkdownBlock {
        var items: [String] = []
        while index < lines.count {
            let trimmed = trim(lines[index])
            guard let item = unorderedItemText(trimmed) else { break }
            items.append(item)
            index += 1
        }
        return .unorderedList(items)
    }

    private static func parseOrderedList(
        lines: [String],
        index: inout Int
    ) -> TerminalMarkdownBlock {
        var items: [(number: Int, text: String)] = []
        while index < lines.count {
            let trimmed = trim(lines[index])
            guard let item = orderedItem(trimmed) else { break }
            items.append(item)
            index += 1
        }
        return .orderedList(items)
    }

    private static func parseParagraph(
        lines: [String],
        index: inout Int
    ) -> TerminalMarkdownBlock {
        var paragraphLines: [String] = []

        while index < lines.count {
            let line = lines[index]
            let trimmed = trim(line)
            if trimmed.isEmpty {
                break
            }
            if !paragraphLines.isEmpty, isBlockStart(trimmed) {
                break
            }
            paragraphLines.append(line.trimmingCharacters(in: .whitespaces))
            index += 1
        }

        return .paragraph(paragraphLines.joined(separator: "\n"))
    }

    private static func parseHeading(_ line: String) -> (level: Int, text: String)? {
        var level = 0
        for character in line {
            guard character == "#" else { break }
            level += 1
        }

        guard (1...6).contains(level) else { return nil }
        let remainder = String(line.dropFirst(level))
        guard remainder.hasPrefix(" ") else { return nil }
        return (level, trim(remainder))
    }

    private static func unorderedItemText(_ line: String) -> String? {
        for marker in ["- ", "* ", "+ "] {
            if line.hasPrefix(marker) {
                return String(line.dropFirst(marker.count))
            }
        }
        return nil
    }

    private static func orderedItem(_ line: String) -> (number: Int, text: String)? {
        var digits = ""
        var cursor = line.startIndex
        while cursor < line.endIndex, line[cursor].isNumber {
            digits.append(line[cursor])
            cursor = line.index(after: cursor)
        }

        guard !digits.isEmpty,
              cursor < line.endIndex,
              line[cursor] == "." || line[cursor] == ")" else {
            return nil
        }

        cursor = line.index(after: cursor)
        guard cursor < line.endIndex, line[cursor] == " " else { return nil }
        cursor = line.index(after: cursor)
        return (Int(digits) ?? 1, String(line[cursor...]))
    }

    private static func fenceMarker(_ line: String) -> String? {
        if line.hasPrefix("```") {
            return "```"
        }
        if line.hasPrefix("~~~") {
            return "~~~"
        }
        return nil
    }

    private static func isBlockStart(_ line: String) -> Bool {
        fenceMarker(line) != nil
            || parseHeading(line) != nil
            || isRule(line)
            || isQuote(line)
            || unorderedItemText(line) != nil
            || orderedItem(line) != nil
    }

    private static func isQuote(_ line: String) -> Bool {
        line.hasPrefix(">")
    }

    private static func isRule(_ line: String) -> Bool {
        guard line.count >= 3 else { return false }
        let characters = Set(line)
        return characters == ["-"] || characters == ["*"] || characters == ["_"]
    }

    private static func trim(_ value: String) -> String {
        value.trimmingCharacters(in: .whitespaces)
    }
}
