import Foundation
import ZeusCheckSuite

@main
struct ZeusChecks {
    static func main() {
        var checks = CheckRunner()
        checks.run(
            "TerminalMarkdownParser parses common blocks",
            ZeusCoreChecks.testCommonMarkdownBlocks
        )
        checks.run(
            "TerminalMarkdownParser stops paragraphs at block starts",
            ZeusCoreChecks.testParagraphBoundaries
        )
        checks.run("ToolMetadata maps known tools", ZeusCoreChecks.testToolMetadata)
        checks.run("ToolMetadata maps display names", ZeusCoreChecks.testToolDisplayNames)
        checks.run("ToolMetadata summarizes arguments", ZeusCoreChecks.testToolTargets)
        checks.run("AgentServerEvent decodes typed events", ZeusCoreChecks.testAgentServerEvents)
        checks.run("Response cache stats format compactly", ZeusCoreChecks.testResponseCacheStatsDisplay)
        checks.run(
            "rust-agent API contract fixtures decode",
            ZeusCoreChecks.testRustAgentAPIContractFixtures
        )
        checks.run("PathDisplay abbreviates home paths", ZeusCoreChecks.testPathDisplay)
        checks.run("PromptHistory navigates like a terminal", ZeusCoreChecks.testPromptHistoryNavigation)
        checks.run("PromptHistory resets and restores entries", ZeusCoreChecks.testPromptHistoryResetAndReplace)
        checks.finish()
    }
}

private struct CheckRunner {
    private var failures: [String] = []

    mutating func run(_ name: String, _ body: () throws -> Void) {
        do {
            try body()
            print("PASS \(name)")
        } catch {
            failures.append("\(name): \(error.localizedDescription)")
            print("FAIL \(name): \(error.localizedDescription)")
        }
    }

    func finish() -> Never {
        if failures.isEmpty {
            print("All checks passed.")
            exit(0)
        }

        for failure in failures {
            fputs("\(failure)\n", stderr)
        }
        exit(1)
    }
}
