public struct PromptHistory {
    private var entries: [String]
    private var navigationIndex: Int?
    private var draftBeforeNavigation: String

    public init(entries: [String] = []) {
        self.entries = entries
        self.navigationIndex = nil
        self.draftBeforeNavigation = ""
    }

    public mutating func record(_ entry: String) {
        entries.append(entry)
        reset()
    }

    public mutating func replace(with entries: [String]) {
        self.entries = entries
        reset()
    }

    public mutating func previous(currentDraft: String) -> String? {
        guard !entries.isEmpty else { return nil }

        let nextIndex: Int
        if let navigationIndex {
            nextIndex = max(0, navigationIndex - 1)
        } else {
            draftBeforeNavigation = currentDraft
            nextIndex = entries.count - 1
        }

        navigationIndex = nextIndex
        return entries[nextIndex]
    }

    public mutating func next() -> String? {
        guard let navigationIndex else { return nil }

        let nextIndex = navigationIndex + 1
        guard nextIndex < entries.count else {
            self.navigationIndex = nil
            let restoredDraft = draftBeforeNavigation
            draftBeforeNavigation = ""
            return restoredDraft
        }

        self.navigationIndex = nextIndex
        return entries[nextIndex]
    }

    public mutating func reset() {
        navigationIndex = nil
        draftBeforeNavigation = ""
    }
}
