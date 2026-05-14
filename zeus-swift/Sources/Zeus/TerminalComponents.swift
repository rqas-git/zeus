import SwiftUI

struct TerminalIconButton: View {
    let systemName: String
    let help: String
    var color: Color = TerminalPalette.dimText
    var size: CGFloat = 10
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: size, weight: .medium))
                .foregroundStyle(color)
                .frame(width: 18, height: 18)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(help)
        .accessibilityLabel(help)
    }
}

extension View {
    func terminalFocusBackground(_ isFocused: Bool, color: Color = TerminalPalette.cyan) -> some View {
        background(Rectangle().fill(isFocused ? color.opacity(0.12) : .clear))
    }

    func terminalPanelChrome(background fill: Color = TerminalPalette.backgroundLow) -> some View {
        self
            .background(Rectangle().fill(fill))
            .overlay(Rectangle().stroke(TerminalPalette.border.opacity(0.40), lineWidth: 1))
    }

    func terminalDropdownChrome() -> some View {
        background(Rectangle().fill(TerminalPalette.background))
            .overlay(
                Rectangle()
                    .stroke(TerminalPalette.border.opacity(0.45), lineWidth: 1)
            )
            .shadow(color: TerminalPalette.shadow.opacity(0.18), radius: 8, x: 0, y: 6)
    }
}
