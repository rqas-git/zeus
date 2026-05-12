import Foundation
import ZeusCheckSuite

@main
struct ZeusChecks {
    static func main() {
        var checks = CheckRunner()
        for check in ZeusCoreChecks.all {
            checks.run(check)
        }
        checks.finish()
    }
}

private struct CheckRunner {
    private var failures: [String] = []

    mutating func run(_ check: ZeusCheck) {
        do {
            try check.body()
            print("PASS \(check.name)")
        } catch {
            failures.append("\(check.name): \(error.localizedDescription)")
            print("FAIL \(check.name): \(error.localizedDescription)")
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
