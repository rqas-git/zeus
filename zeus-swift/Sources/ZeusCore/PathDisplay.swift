import Foundation

public enum PathDisplay {
    public static func abbreviatingHome(
        in path: String,
        homeDirectory: String = FileManager.default.homeDirectoryForCurrentUser.path
    ) -> String {
        guard path == homeDirectory || path.hasPrefix(homeDirectory + "/") else { return path }
        return "~" + path.dropFirst(homeDirectory.count)
    }
}
