import AppKit
import SwiftUI

enum CodexTypography {
    static let chatSize: CGFloat = 11.2
    static let chatSmallSize: CGFloat = 9.6
    static let chatXSmallSize: CGFloat = 8.8
    static let codeSize: CGFloat = 9.6
    static let codeSmallSize: CGFloat = 8.8

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
            return .system(size: 12, weight: .semibold, design: .monospaced)
        case 2:
            return .system(size: 11.6, weight: .semibold, design: .monospaced)
        default:
            return .system(size: chatSize, weight: .semibold, design: .monospaced)
        }
    }
}
