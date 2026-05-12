import AppKit
import SwiftUI

enum CodexTypography {
    static let chatSize: CGFloat = 14
    static let chatSmallSize: CGFloat = 12.5
    static let chatXSmallSize: CGFloat = 11.5
    static let codeSize: CGFloat = 12.5
    static let codeSmallSize: CGFloat = 11.5

    private static let fontName = "JetBrainsMono-Regular"
    private static let fontNameSemiBold = "JetBrainsMono-SemiBold"

    static let chat = Font.custom(fontName, size: chatSize)
    static let chatSmall = Font.custom(fontName, size: chatSmallSize)
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
            return .custom(fontNameSemiBold, size: 15.5)
        case 2:
            return .custom(fontNameSemiBold, size: 14.5)
        default:
            return .custom(fontNameSemiBold, size: chatSize)
        }
    }
}
