import AppKit

enum KeyCode {
    static let returnKey: UInt16 = 36
    static let escape: UInt16 = 53
    static let keypadEnter: UInt16 = 76
    static let downArrow: UInt16 = 125
    static let upArrow: UInt16 = 126
    static let leftArrow: UInt16 = 123
    static let rightArrow: UInt16 = 124
}

extension NSEvent {
    var independentModifierFlags: ModifierFlags {
        modifierFlags.intersection(.deviceIndependentFlagsMask)
    }

    func hasNoModifiers(_ disallowed: ModifierFlags = [.command, .control, .option, .shift]) -> Bool {
        independentModifierFlags.intersection(disallowed).isEmpty
    }
}
