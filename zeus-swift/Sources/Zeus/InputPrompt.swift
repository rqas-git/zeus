import SwiftUI

struct InputPrompt: View {
    @Binding var text: String
    let prompt: String
    let placeholder: String
    let onSubmit: () -> Void
    let onHistoryPrevious: () -> Bool
    let onHistoryNext: () -> Bool
    let onMoveDownFromCurrent: () -> Void
    let isCancelVisible: Bool
    let onCancel: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: TerminalLayout.markerTextSpacing) {
            Text(prompt)
                .foregroundStyle(TerminalPalette.cyan)
                .frame(width: TerminalLayout.markerWidth, alignment: .leading)

            PromptTextField(
                text: $text,
                placeholder: placeholder,
                onSubmit: onSubmit,
                onHistoryPrevious: onHistoryPrevious,
                onHistoryNext: onHistoryNext,
                onMoveDownFromCurrent: {
                    onMoveDownFromCurrent()
                    return true
                }
            )
                .frame(height: 18)
                .frame(maxWidth: .infinity, alignment: .leading)

            if isCancelVisible {
                Button(action: onCancel) {
                    Image(systemName: "xmark.circle")
                        .font(.system(size: 11, weight: .regular))
                        .foregroundStyle(TerminalPalette.red)
                        .frame(width: 18, height: 18)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Cancel Turn")
            }
        }
    }
}

