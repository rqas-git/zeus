import Testing
import ZeusCheckSuite

@Test
func terminalMarkdownParserParsesCommonBlocks() throws {
    try ZeusCoreChecks.testCommonMarkdownBlocks()
}

@Test
func terminalMarkdownParserStopsParagraphsAtBlockStarts() throws {
    try ZeusCoreChecks.testParagraphBoundaries()
}

@Test
func toolMetadataMapsKnownTools() throws {
    try ZeusCoreChecks.testToolMetadata()
}

@Test
func toolMetadataSummarizesArguments() throws {
    try ZeusCoreChecks.testToolTargets()
}

@Test
func agentServerEventDecodesTypedEvents() throws {
    try ZeusCoreChecks.testAgentServerEvents()
}

@Test
func pathDisplayAbbreviatesHomePaths() throws {
    try ZeusCoreChecks.testPathDisplay()
}
