import Foundation

public struct ToolMetadata: Equatable {
    public let name: String
    public let action: String
    public let iconName: String

    public static func forName(_ rawName: String?) -> ToolMetadata {
        let name = rawName ?? "tool"
        let definition = definitions[name] ?? ToolDefinition(
            action: "running",
            iconName: "wrench.and.screwdriver"
        )
        return ToolMetadata(
            name: name,
            action: definition.action,
            iconName: definition.iconName
        )
    }

    public func target(fromArgumentsJSON arguments: String?) -> String? {
        guard let arguments,
              let data = arguments.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data),
              let json = object as? [String: Any] else {
            return nil
        }

        let target = primaryToolTarget(json: json)
        return target?.isEmpty == true ? nil : target
    }

    private func primaryToolTarget(json: [String: Any]) -> String? {
        switch name {
        case "read_file", "read_file_range", "list_dir", "git_diff":
            return stringValue(json["path"])
        case "search_files", "search_text":
            return quoted(stringValue(json["query"]))
        case "exec_command":
            return quoted(stringValue(json["command"]))
        case "apply_patch":
            return patchSummary(stringValue(json["patch"]))
        case "git_add", "git_restore":
            return pathsSummary(json["paths"])
        case "git_log":
            return stringValue(json["path"]) ?? maxCountSummary(json["max_count"])
        case "git_query":
            return stringArray(json["args"])?.joined(separator: " ")
        default:
            return stringValue(json["path"])
                ?? stringValue(json["query"])
                ?? pathsSummary(json["paths"])
        }
    }

    private func stringValue(_ value: Any?) -> String? {
        value as? String
    }

    private func stringArray(_ value: Any?) -> [String]? {
        value as? [String]
    }

    private func quoted(_ value: String?) -> String? {
        guard let value, !value.isEmpty else { return nil }
        return "\"\(value)\""
    }

    private func pathsSummary(_ value: Any?) -> String? {
        guard let paths = stringArray(value), !paths.isEmpty else { return nil }
        if paths.count == 1 {
            return paths[0]
        }
        return "\(paths[0]) +\(paths.count - 1)"
    }

    private func maxCountSummary(_ value: Any?) -> String? {
        guard let value else { return nil }
        return "max \(value)"
    }

    private func patchSummary(_ patch: String?) -> String? {
        guard let patch, !patch.isEmpty else { return nil }
        var files: [String] = []
        for line in patch.split(separator: "\n") {
            let text = String(line)
            for prefix in ["*** Add File: ", "*** Update File: ", "*** Delete File: "] {
                guard text.hasPrefix(prefix) else { continue }
                files.append(String(text.dropFirst(prefix.count)))
            }
        }
        guard let first = files.first else { return "workspace" }
        if files.count == 1 {
            return first
        }
        return "\(first) +\(files.count - 1)"
    }

    private static let definitions: [String: ToolDefinition] = [
        "read_file": ToolDefinition(action: "reading", iconName: "doc.text"),
        "read_file_range": ToolDefinition(action: "reading", iconName: "doc.text"),
        "list_dir": ToolDefinition(action: "listing", iconName: "folder"),
        "search_files": ToolDefinition(action: "searching", iconName: "magnifyingglass"),
        "search_text": ToolDefinition(action: "searching", iconName: "magnifyingglass"),
        "apply_patch": ToolDefinition(action: "patching", iconName: "square.and.pencil"),
        "exec_command": ToolDefinition(action: "running", iconName: "terminal"),
        "git_add": ToolDefinition(action: "staging", iconName: "plus.square"),
        "git_restore": ToolDefinition(action: "restoring", iconName: "arrow.uturn.backward.square"),
        "git_diff": ToolDefinition(action: "diffing", iconName: "arrow.left.arrow.right"),
        "git_log": ToolDefinition(action: "reading log", iconName: "clock"),
        "git_query": ToolDefinition(action: "checking", iconName: "checklist"),
        "git_status": ToolDefinition(action: "checking", iconName: "checklist"),
        "git_commit": ToolDefinition(action: "committing", iconName: "arrow.trianglehead.branch")
    ]
}

private struct ToolDefinition {
    let action: String
    let iconName: String
}
