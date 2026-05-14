import SwiftUI

struct SearchBar: View {
    @Binding var text: String
    let resultSummary: String
    let onPrevious: () -> Void
    let onNext: () -> Void
    let onClose: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 10, weight: .regular))
                .foregroundStyle(TerminalPalette.cyan)
                .frame(width: TerminalLayout.markerWidth, alignment: .leading)

            PromptTextField(
                text: $text,
                placeholder: "search transcript...",
                onSubmit: onNext,
                onHistoryPrevious: { false },
                onHistoryNext: { false }
            )
            .frame(height: TerminalLayout.controlHeight)
            .frame(maxWidth: .infinity, alignment: .leading)

            Text(resultSummary)
                .foregroundStyle(TerminalPalette.dimText)
                .lineLimit(1)
                .frame(minWidth: 82, alignment: .trailing)

            TerminalIconButton(systemName: "chevron.up", help: "Previous Match", action: onPrevious)
            TerminalIconButton(systemName: "chevron.down", help: "Next Match", action: onNext)
            TerminalIconButton(systemName: "xmark", help: "Close Search", action: onClose)
        }
        .font(TerminalTypography.chatSmall)
        .frame(height: TerminalLayout.searchHeight)
        .padding(.horizontal, 8)
        .terminalPanelChrome()
    }
}
