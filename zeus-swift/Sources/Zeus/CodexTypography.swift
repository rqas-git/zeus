import AppKit
import SwiftUI

enum CodexTypography {
    static let chatSize: CGFloat = 14
    static let chatSmallSize: CGFloat = 12
    static let chatXSmallSize: CGFloat = 11
    static let codeSize: CGFloat = 12
    static let codeSmallSize: CGFloat = 11

    static let chat = Font.system(size: chatSize, weight: .regular, design: .monospaced)
    static let chatSmall = Font.system(size: chatSmallSize, weight: .regular, design: .monospaced)
    static let chatXSmall = Font.system(size: chatXSmallSize, weight: .regular, design: .monospaced)
    static let code = Font.system(size: codeSize, weight: .regular, design: .monospaced)
    static let codeSmall = Font.system(size: codeSmallSize, weight: .regular, design: .monospaced)

    static let chatNSFont = NSFont.monospacedSystemFont(ofSize: chatSize, weight: .regular)
    static let chatSmallNSFont = NSFont.monospacedSystemFont(ofSize: chatSmallSize, weight: .regular)

    static func heading(level: Int) -> Font {
        switch level {
        case 1:
            return .system(size: 15, weight: .semibold, design: .monospaced)
        case 2:
            return .system(size: 14.5, weight: .semibold, design: .monospaced)
        default:
            return .system(size: chatSize, weight: .semibold, design: .monospaced)
        }
    }
}
