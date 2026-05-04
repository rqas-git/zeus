import Testing
import ZeusCore

@Test
func parsesCommonMarkdownBlocks() {
    let blocks = TerminalMarkdownParser.parse(
        """
        # Title

        Paragraph line one
        paragraph line two

        - one
        - two

        1. first
        2. second

        > quote

        ```swift
        let value = 1
        ```

        ---
        """
    )

    #expect(blocks.count == 7)

    guard case let .heading(level, text) = blocks[0] else {
        Issue.record("Expected heading block")
        return
    }
    #expect(level == 1)
    #expect(text == "Title")

    guard case let .paragraph(paragraph) = blocks[1] else {
        Issue.record("Expected paragraph block")
        return
    }
    #expect(paragraph == "Paragraph line one\nparagraph line two")

    guard case let .unorderedList(unorderedItems) = blocks[2] else {
        Issue.record("Expected unordered list block")
        return
    }
    #expect(unorderedItems == ["one", "two"])

    guard case let .orderedList(orderedItems) = blocks[3] else {
        Issue.record("Expected ordered list block")
        return
    }
    #expect(orderedItems.map(\.number) == [1, 2])
    #expect(orderedItems.map(\.text) == ["first", "second"])

    guard case let .quote(quoteLines) = blocks[4] else {
        Issue.record("Expected quote block")
        return
    }
    #expect(quoteLines == ["quote"])

    guard case let .codeBlock(language, code) = blocks[5] else {
        Issue.record("Expected code block")
        return
    }
    #expect(language == "swift")
    #expect(code == "let value = 1")

    guard case .rule = blocks[6] else {
        Issue.record("Expected rule block")
        return
    }
}

@Test
func keepsParagraphTextUntilNextBlockStart() {
    let blocks = TerminalMarkdownParser.parse(
        """
        alpha
        beta
        ## Next
        """
    )

    #expect(blocks.count == 2)

    guard case let .paragraph(text) = blocks[0] else {
        Issue.record("Expected paragraph block")
        return
    }
    #expect(text == "alpha\nbeta")

    guard case let .heading(level, text) = blocks[1] else {
        Issue.record("Expected heading block")
        return
    }
    #expect(level == 2)
    #expect(text == "Next")
}
