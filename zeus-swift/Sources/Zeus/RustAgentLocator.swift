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
            current.appendingPathComponent("rust-agent"),
            current.deletingLastPathComponent().appendingPathComponent("rust-agent")
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
        if let bundledBinary = bundledRustAgentBinaryURL() {
            process.executableURL = bundledBinary
            process.arguments = arguments
            process.currentDirectoryURL = workingDirectoryURL
            return
        }

        let debugBinary = rootURL.appendingPathComponent("target/debug/rust-agent")
        if shouldUseDebugBinary(debugBinary, rootURL: rootURL) {
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

    private static func bundledRustAgentBinaryURL() -> URL? {
        guard let executableDirectory = Bundle.main.executableURL?
            .standardizedFileURL
            .deletingLastPathComponent()
        else {
            return nil
        }

        let binaryURL = executableDirectory.appendingPathComponent("rust-agent")
        return FileManager.default.isExecutableFile(atPath: binaryURL.path) ? binaryURL : nil
    }

    private static func isRustAgentRoot(_ url: URL) -> Bool {
        FileManager.default.fileExists(atPath: url.appendingPathComponent("Cargo.toml").path)
    }

    private static func shouldUseDebugBinary(_ binaryURL: URL, rootURL: URL) -> Bool {
        guard FileManager.default.isExecutableFile(atPath: binaryURL.path),
              let binaryDate = modificationDate(of: binaryURL) else {
            return false
        }

        return !rustAgentInputs(rootURL: rootURL).contains { inputURL in
            guard let inputDate = modificationDate(of: inputURL) else { return false }
            return inputDate > binaryDate
        }
    }

    private static func rustAgentInputs(rootURL: URL) -> [URL] {
        var inputs = [
            rootURL.appendingPathComponent("Cargo.toml"),
            rootURL.appendingPathComponent("Cargo.lock")
        ]

        let sourceRoot = rootURL.appendingPathComponent("src")
        let enumerator = FileManager.default.enumerator(
            at: sourceRoot,
            includingPropertiesForKeys: [.isRegularFileKey],
            options: [.skipsHiddenFiles]
        )

        while let fileURL = enumerator?.nextObject() as? URL {
            guard fileURL.pathExtension == "rs",
                  (try? fileURL.resourceValues(forKeys: [.isRegularFileKey]).isRegularFile) == true else {
                continue
            }
            inputs.append(fileURL)
        }

        return inputs
    }

    private static func modificationDate(of url: URL) -> Date? {
        try? url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate
    }
}
