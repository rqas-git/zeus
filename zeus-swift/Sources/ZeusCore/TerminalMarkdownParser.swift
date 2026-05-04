import Foundation

public enum TerminalMarkdownBlock {
    case paragraph(String)
    case heading(level: Int, text: String)
    case unorderedList([String])
    case orderedList([(number: Int, text: String)])
    case quote([String])
    case codeBlock(language: String?, code: String)
    case rule
}

public enum TerminalMarkdownParser {
    public static func parse(_ source: String) -> [TerminalMarkdownBlock] {
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
