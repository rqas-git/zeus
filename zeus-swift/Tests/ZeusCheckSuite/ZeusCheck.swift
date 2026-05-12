public struct ZeusCheck {
    public let name: String
    public let body: () throws -> Void

    public init(_ name: String, _ body: @escaping () throws -> Void) {
        self.name = name
        self.body = body
    }
}
