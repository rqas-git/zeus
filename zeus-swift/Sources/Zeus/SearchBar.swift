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
            .frame(height: 18)
            .frame(maxWidth: .infinity, alignment: .leading)

            Text(resultSummary)
                .foregroundStyle(TerminalPalette.dimText)
                .lineLimit(1)
                .frame(minWidth: 82, alignment: .trailing)

            searchButton(systemName: "chevron.up", help: "Previous Match", action: onPrevious)
            searchButton(systemName: "chevron.down", help: "Next Match", action: onNext)
            searchButton(systemName: "xmark", help: "Close Search", action: onClose)
        }
        .font(.system(size: 11, weight: .regular, design: .monospaced))
        .frame(height: 20)
    }

    private func searchButton(
        systemName: String,
        help: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: systemName)
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(TerminalPalette.dimText)
                .frame(width: 18, height: 18)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help(help)
    }
}

