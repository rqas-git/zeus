import Foundation

enum RustAgentLocator {
    static func rootURL() -> URL {
        let environment = ProcessInfo.processInfo.environment
        if let configured = environment["RUST_AGENT_ROOT"], !configured.isEmpty {
            return URL(fileURLWithPath: configured).standardizedFileURL
        }

        let current = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .standardizedFileURL
        var candidates = [
            current.deletingLastPathComponent().appendingPathComponent("rust-agent"),
            current.appendingPathComponent("rust-agent")
        ]

        if let executable = Bundle.main.executableURL?.standardizedFileURL {
            var directory = executable.deletingLastPathComponent()
            for _ in 0..<8 {
                candidates.append(directory.appendingPathComponent("rust-agent"))
                directory.deleteLastPathComponent()
            }
        }

        return candidates
            .map(\.standardizedFileURL)
            .first(where: isRustAgentRoot(_:))
            ?? candidates[0].standardizedFileURL
    }

    static func configure(_ process: Process, rootURL: URL, arguments: [String]) {
        let debugBinary = rootURL.appendingPathComponent("target/debug/rust-agent")
        if FileManager.default.isExecutableFile(atPath: debugBinary.path) {
            process.executableURL = debugBinary
            process.arguments = arguments
        } else {
            process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
            process.arguments = ["cargo", "run", "--"] + arguments
        }
        process.currentDirectoryURL = rootURL
    }

    private static func isRustAgentRoot(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.appendingPathComponent("Cargo.toml").path)
    }
}
