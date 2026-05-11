public struct ServerSentEventLineDecoder {
    private var bytes: [UInt8] = []

    public init() {}

    public mutating func append(_ byte: UInt8) -> String? {
        guard byte == 10 else {
            bytes.append(byte)
            return nil
        }

        return finishLine()
    }

    public mutating func finish() -> String? {
        guard !bytes.isEmpty else { return nil }
        return finishLine()
    }

    private mutating func finishLine() -> String {
        if bytes.last == 13 {
            bytes.removeLast()
        }
        let line = String(decoding: bytes, as: UTF8.self)
        bytes.removeAll(keepingCapacity: true)
        return line
    }
}

public struct ServerSentEventDataParser {
    private static let previewLimit = 1_000

    private var dataLines: [String] = []
    public private(set) var preview = ""

    public init() {}

    public mutating func append(line rawLine: String) -> [String]? {
        let line = normalized(rawLine)
        appendPreview(line)

        guard !line.isEmpty else {
            return flush()
        }

        guard line.hasPrefix("data:") else {
            return nil
        }

        var value = String(line.dropFirst(5))
        if value.first == " " {
            value.removeFirst()
        }
        dataLines.append(value)
        return nil
    }

    public mutating func finish() -> [String]? {
        flush()
    }

    private mutating func flush() -> [String]? {
        guard !dataLines.isEmpty else { return nil }
        let lines = dataLines
        dataLines.removeAll(keepingCapacity: true)
        return lines
    }

    private func normalized(_ line: String) -> String {
        line.hasSuffix("\r") ? String(line.dropLast()) : line
    }

    private mutating func appendPreview(_ line: String) {
        guard preview.count < Self.previewLimit else { return }
        preview += line
        preview += "\n"
        if preview.count > Self.previewLimit {
            preview = String(preview.prefix(Self.previewLimit))
        }
    }
}
