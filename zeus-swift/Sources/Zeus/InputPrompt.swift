import SwiftUI
import ZeusCore

struct InputPrompt: View {
    @Binding var text: String
    @Binding var selectionLocation: Int?
    let prompt: String
    let placeholder: String
    let pathCompletion: PromptPathCompletionState?
    let onSubmit: () -> Void
    let onHistoryPrevious: () -> Bool
    let onHistoryNext: () -> Bool
    let onMoveDownFromCurrent: () -> Void
    let onTextEdited: (String, Int) -> Void
    let onCompletionTrigger: (Int) -> Bool
    let onCompletionMove: (Int) -> Bool
    let onCompletionAccept: () -> Bool
    let onCompletionCancel: () -> Bool
    let onCompletionHighlight: (Int) -> Void
    let onCompletionSelect: (Int) -> Void
    let isCancelVisible: Bool
    let onCancel: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: TerminalLayout.markerTextSpacing) {
            Text(prompt)
                .foregroundStyle(TerminalPalette.cyan)
                .frame(width: TerminalLayout.markerWidth, alignment: .leading)

            PromptTextField(
                text: $text,
                selectionLocation: $selectionLocation,
                placeholder: placeholder,
                onSubmit: onSubmit,
                onHistoryPrevious: onHistoryPrevious,
                onHistoryNext: onHistoryNext,
                onTextEdited: onTextEdited,
                onCompletionTrigger: onCompletionTrigger,
                onCompletionMove: onCompletionMove,
                onCompletionAccept: onCompletionAccept,
                onCompletionCancel: onCompletionCancel,
                onMoveDownFromCurrent: {
                    onMoveDownFromCurrent()
                    return true
                }
            )
                .frame(height: TerminalLayout.controlHeight)
                .frame(maxWidth: .infinity, alignment: .leading)

            if isCancelVisible {
                Button(action: onCancel) {
                    Image(systemName: "xmark.circle")
                        .font(.system(size: 11, weight: .regular))
                        .foregroundStyle(TerminalPalette.red)
                        .frame(width: 20, height: 20)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Cancel Turn")
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 5)
        .terminalPanelChrome()
        .overlay(alignment: .topLeading) {
            if let pathCompletion {
                PathCompletionDropdown(
                    completion: pathCompletion,
                    onHighlight: onCompletionHighlight,
                    onSelect: onCompletionSelect
                )
                .alignmentGuide(.top) { dimensions in
                    dimensions[.bottom] + 6
                }
                .offset(x: 8 + TerminalLayout.markerWidth + TerminalLayout.markerTextSpacing)
                .zIndex(40)
            }
        }
        .zIndex(pathCompletion == nil ? 0 : 40)
    }
}

private struct PathCompletionDropdown: View {
    let completion: PromptPathCompletionState
    let onHighlight: (Int) -> Void
    let onSelect: (Int) -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                ForEach(completion.suggestions.indices, id: \.self) { index in
                    row(completion.suggestions[index], index: index)
                }
            }
        }
        .frame(width: 560, alignment: .topLeading)
        .frame(maxHeight: 280, alignment: .topLeading)
        .terminalDropdownChrome()
        .font(TerminalTypography.chatSmall)
        .fixedSize(horizontal: true, vertical: false)
    }

    private func row(_ suggestion: PathCompletionSuggestion, index: Int) -> some View {
        let isSelected = completion.selectedIndex == index
        return Button {
            onSelect(index)
        } label: {
            HStack(spacing: 8) {
                Image(systemName: suggestion.isDirectory ? "folder" : "doc.text")
                    .foregroundStyle(suggestion.isDirectory ? TerminalPalette.green : TerminalPalette.dimText)
                    .frame(width: 14)

                Text(suggestion.label)
                    .foregroundStyle(TerminalPalette.primaryText)
                    .lineLimit(1)
                    .truncationMode(.middle)

                Spacer(minLength: 10)

                Text(suggestion.detail)
                    .foregroundStyle(suggestion.isExternal ? TerminalPalette.amber : TerminalPalette.dimText)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .padding(.horizontal, 9)
            .padding(.vertical, 5)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(isSelected ? TerminalPalette.cyan.opacity(0.16) : Color.clear)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering in
            if hovering {
                onHighlight(index)
            }
        }
    }
}
