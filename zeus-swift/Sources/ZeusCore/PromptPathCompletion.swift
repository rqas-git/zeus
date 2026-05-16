public enum PromptPathCompletionKind: String, Equatable {
    case fileReference = "file_reference"
    case path
}

public struct PromptPathCompletionContext: Equatable {
    public let kind: PromptPathCompletionKind
    public let prefix: String
    public let replacementStart: Int
    public let replacementEnd: Int
    public let isQuotedPrefix: Bool

    public init(
        kind: PromptPathCompletionKind,
        prefix: String,
        replacementStart: Int,
        replacementEnd: Int,
        isQuotedPrefix: Bool
    ) {
        self.kind = kind
        self.prefix = prefix
        self.replacementStart = replacementStart
        self.replacementEnd = replacementEnd
        self.isQuotedPrefix = isQuotedPrefix
    }
}

public struct PromptPathCompletionResult: Equatable {
    public let text: String
    public let cursor: Int

    public init(text: String, cursor: Int) {
        self.text = text
        self.cursor = cursor
    }
}

public enum PromptPathCompletion {
    public static func context(
        in text: String,
        cursor: Int,
        explicitTab: Bool,
        terminalMode: Bool
    ) -> PromptPathCompletionContext? {
        guard let cursorIndex = stringIndex(in: text, utf16Offset: cursor) else { return nil }
        let beforeCursor = String(text[..<cursorIndex])

        if !terminalMode, let atContext = atReferenceContext(
            in: beforeCursor,
            cursor: cursor
        ) {
            return atContext
        }

        guard explicitTab else { return nil }
        let path = pathContext(in: beforeCursor, cursor: cursor)
        if terminalMode, path?.prefix.hasPrefix("@") == true {
            return nil
        }
        guard terminalMode || path?.kind == .path else { return path }
        return path
    }

    public static func apply(
        suggestion: PathCompletionSuggestion,
        to text: String,
        context: PromptPathCompletionContext
    ) -> PromptPathCompletionResult {
        guard let start = stringIndex(in: text, utf16Offset: context.replacementStart),
              let end = stringIndex(in: text, utf16Offset: context.replacementEnd) else {
            return PromptPathCompletionResult(text: text, cursor: text.utf16.count)
        }

        let before = String(text[..<start])
        var after = String(text[end...])
        if context.isQuotedPrefix,
           suggestion.value.hasSuffix("\""),
           after.hasPrefix("\"") {
            after.removeFirst()
        }

        let suffix = context.kind == .fileReference && !suggestion.isDirectory ? " " : ""
        let replacement = suggestion.value + suffix
        let newText = before + replacement + after
        var cursor = (before + replacement).utf16.count
        if suggestion.isDirectory && suggestion.value.hasSuffix("\"") {
            cursor = max(before.utf16.count, cursor - 1)
        }
        return PromptPathCompletionResult(text: newText, cursor: cursor)
    }

    private static func atReferenceContext(
        in text: String,
        cursor: Int
    ) -> PromptPathCompletionContext? {
        if let quoted = quotedPrefix(in: text), quoted.prefix.hasPrefix("@\"") {
            return PromptPathCompletionContext(
                kind: .fileReference,
                prefix: quoted.prefix,
                replacementStart: quoted.start,
                replacementEnd: cursor,
                isQuotedPrefix: true
            )
        }

        let token = trailingToken(in: text)
        guard token.prefix.hasPrefix("@") else { return nil }
        return PromptPathCompletionContext(
            kind: .fileReference,
            prefix: token.prefix,
            replacementStart: token.start,
            replacementEnd: cursor,
            isQuotedPrefix: false
        )
    }

    private static func pathContext(
        in text: String,
        cursor: Int
    ) -> PromptPathCompletionContext? {
        if let quoted = quotedPrefix(in: text) {
            return PromptPathCompletionContext(
                kind: .path,
                prefix: quoted.prefix,
                replacementStart: quoted.start,
                replacementEnd: cursor,
                isQuotedPrefix: true
            )
        }

        let token = trailingToken(in: text)
        return PromptPathCompletionContext(
            kind: .path,
            prefix: token.prefix,
            replacementStart: token.start,
            replacementEnd: cursor,
            isQuotedPrefix: false
        )
    }

    private static func quotedPrefix(in text: String) -> (prefix: String, start: Int)? {
        var inQuote = false
        var quoteStartIndex: String.Index?
        var quoteStartOffset = 0
        var offset = 0

        for index in text.indices {
            if text[index] == "\"" {
                inQuote.toggle()
                if inQuote {
                    quoteStartIndex = index
                    quoteStartOffset = offset
                }
            }
            offset += String(text[index]).utf16.count
        }

        guard inQuote, let quoteStartIndex else { return nil }
        if quoteStartIndex > text.startIndex {
            let beforeQuote = text.index(before: quoteStartIndex)
            if text[beforeQuote] == "@", isTokenStart(text, beforeQuote) {
                return (String(text[beforeQuote...]), quoteStartOffset - 1)
            }
        }
        guard isTokenStart(text, quoteStartIndex) else { return nil }
        return (String(text[quoteStartIndex...]), quoteStartOffset)
    }

    private static func trailingToken(in text: String) -> (prefix: String, start: Int) {
        var tokenStart = text.startIndex
        var tokenStartOffset = 0
        var offset = 0
        for index in text.indices {
            let character = text[index]
            offset += String(character).utf16.count
            if isDelimiter(character) {
                tokenStart = text.index(after: index)
                tokenStartOffset = offset
            }
        }
        return (String(text[tokenStart...]), tokenStartOffset)
    }

    private static func isTokenStart(_ text: String, _ index: String.Index) -> Bool {
        guard index > text.startIndex else { return true }
        return isDelimiter(text[text.index(before: index)])
    }

    private static func isDelimiter(_ character: Character) -> Bool {
        character.isWhitespace || character == "\"" || character == "'" || character == "="
    }

    private static func stringIndex(in text: String, utf16Offset: Int) -> String.Index? {
        guard utf16Offset >= 0,
              let utf16Index = text.utf16.index(
                text.utf16.startIndex,
                offsetBy: utf16Offset,
                limitedBy: text.utf16.endIndex
              ) else {
            return nil
        }
        return String.Index(utf16Index, within: text)
    }
}
