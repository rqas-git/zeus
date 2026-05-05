import Foundation

struct BashCommandResult {
    let output: String
    let exitCode: Int32
}

enum BashPassthrough {
    private static let maxOutputBytes = 64 * 1024

    static func run(_ command: String, at url: URL) throws -> BashCommandResult {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/bash")
        process.arguments = ["-lc", command]
        process.currentDirectoryURL = url

        let output = Pipe()
        let buffer = BoundedOutputBuffer(maxBytes: maxOutputBytes)
        process.standardOutput = output
        process.standardError = output
        output.fileHandleForReading.readabilityHandler = { handle in
            buffer.append(handle.availableData)
        }

        do {
            try process.run()
            process.waitUntilExit()
        } catch {
            output.fileHandleForReading.readabilityHandler = nil
            throw BashPassthroughError.launchFailed(error)
        }

        output.fileHandleForReading.readabilityHandler = nil
        buffer.append(output.fileHandleForReading.availableData)
        return BashCommandResult(
            output: buffer.text,
            exitCode: process.terminationStatus
        )
    }
}

enum BashPassthroughError: LocalizedError {
    case launchFailed(Error)

    var errorDescription: String? {
        switch self {
        case let .launchFailed(error):
            return "Failed to run bash: \(error.localizedDescription)"
        }
    }
}

private final class BoundedOutputBuffer {
    private let lock = NSLock()
    private let maxBytes: Int
    private var data = Data()
    private var isTruncated = false

    init(maxBytes: Int) {
        self.maxBytes = maxBytes
    }

    func append(_ chunk: Data) {
        guard !chunk.isEmpty else { return }
        lock.lock()
        defer { lock.unlock() }

        guard data.count < maxBytes else {
            isTruncated = true
            return
        }

        let remaining = maxBytes - data.count
        if chunk.count <= remaining {
            data.append(chunk)
        } else {
            data.append(chunk.prefix(remaining))
            isTruncated = true
        }
    }

    var text: String {
        lock.lock()
        defer { lock.unlock() }

        var text = String(data: data, encoding: .utf8) ?? ""
        text = text.trimmingCharacters(in: .newlines)
        if isTruncated {
            if !text.isEmpty {
                text += "\n"
            }
            text += "[output truncated]"
        }
        return text
    }
}
