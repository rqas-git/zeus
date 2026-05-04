import Foundation

enum RustAgentLocator {
    static func launchDirectoryURL() -> URL {
        let environment = ProcessInfo.processInfo.environment
        if let pwd = environment["PWD"], !pwd.isEmpty {
            return URL(fileURLWithPath: pwd).standardizedFileURL
        }

        return URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
            .standardizedFileURL
    }

    static func rootURL() -> URL {
        let environment = ProcessInfo.processInfo.environment
        if let configured = environment["RUST_AGENT_ROOT"], !configured.isEmpty {
            return URL(fileURLWithPath: configured).standardizedFileURL
        }

        let current = launchDirectoryURL()
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

    static func configure(
        _ process: Process,
        rootURL: URL,
        arguments: [String],
        workingDirectoryURL: URL? = nil
    ) {
        let workingDirectoryURL = workingDirectoryURL ?? rootURL
        let debugBinary = rootURL.appendingPathComponent("target/debug/rust-agent")
        if FileManager.default.isExecutableFile(atPath: debugBinary.path) {
            process.executableURL = debugBinary
            process.arguments = arguments
        } else {
            process.executableURL = URL(fileURLWithPath: "/usr/bin/env")
            process.arguments = [
                "cargo",
                "run",
                "--manifest-path",
                rootURL.appendingPathComponent("Cargo.toml").path,
                "--"
            ] + arguments
        }
        process.currentDirectoryURL = workingDirectoryURL
    }

    private static func isRustAgentRoot(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.appendingPathComponent("Cargo.toml").path)
    }
}
