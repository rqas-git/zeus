import AppKit
import SwiftUI

enum CodexTypography {
    static let chatSize: CGFloat = 12.5
    static let chatSmallSize: CGFloat = 11
    static let chatXSmallSize: CGFloat = 10
    static let codeSize: CGFloat = 11
    static let codeSmallSize: CGFloat = 10

    private static let fontName = "JetBrainsMono-Regular"
    private static let fontNameSemiBold = "JetBrainsMono-SemiBold"

    static let chat = Font.custom(fontName, size: chatSize)
    static let chatSmall = Font.custom(fontName, size: chatSmallSize)
    static let chatSmallBold = Font.custom(fontNameSemiBold, size: chatSmallSize)
    static let chatXSmall = Font.custom(fontName, size: chatXSmallSize)
    static let code = Font.custom(fontName, size: codeSize)
    static let codeSmall = Font.custom(fontName, size: codeSmallSize)

    static let chatNSFont = NSFont(name: fontName, size: chatSize)
        ?? NSFont.monospacedSystemFont(ofSize: chatSize, weight: .regular)
    static let chatSmallNSFont = NSFont(name: fontName, size: chatSmallSize)
        ?? NSFont.monospacedSystemFont(ofSize: chatSmallSize, weight: .regular)

    static func heading(level: Int) -> Font {
        switch level {
        case 1:
            return .custom(fontNameSemiBold, size: 14)
        case 2:
            return .custom(fontNameSemiBold, size: 13)
        default:
            return .custom(fontNameSemiBold, size: chatSize)
        }
    }
}
