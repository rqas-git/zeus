import SwiftUI

struct TerminalMenuButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(
                Rectangle()
                    .fill(configuration.isPressed ? TerminalPalette.cyan.opacity(0.12) : .clear)
            )
    }
}
