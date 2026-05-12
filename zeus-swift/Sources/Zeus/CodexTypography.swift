import AppKit
import SwiftUI

enum CodexTypography {
    static let chatSize: CGFloat = 11.2
    static let chatSmallSize: CGFloat = 9.6
    static let chatXSmallSize: CGFloat = 8.8
    static let codeSize: CGFloat = 9.6
    static let codeSmallSize: CGFloat = 8.8

    private static let fontName = "ShareTechMono-Regular"

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
            return .custom(fontName, size: 12)
        case 2:
            return .custom(fontName, size: 11.6)
        default:
            return .custom(fontName, size: chatSize)
        }
    }
}
