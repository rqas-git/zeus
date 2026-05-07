import AppKit
import SwiftUI

struct PromptTextField: NSViewRepresentable {
    private static let textFont = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)

    @Binding var text: String
    let placeholder: String
    let onSubmit: () -> Void
    let onHistoryPrevious: () -> Bool
    let onHistoryNext: () -> Bool
    var onMoveDownFromCurrent: () -> Bool = { false }

    func makeNSView(context: Context) -> NSTextField {
        let textField = NSTextField()
        textField.delegate = context.coordinator
        textField.target = context.coordinator
        textField.action = #selector(Coordinator.submit)
        textField.isEditable = true
        textField.isSelectable = true
        textField.isEnabled = true
        textField.isBordered = false
        textField.drawsBackground = false
        textField.focusRingType = .none
        textField.font = Self.textFont
        textField.textColor = NSColor(TerminalPalette.primaryText)
        textField.placeholderAttributedString = attributedPlaceholder
        textField.cell?.sendsActionOnEndEditing = false
        textField.lineBreakMode = .byTruncatingTail
        textField.setContentHuggingPriority(.defaultLow, for: .horizontal)
        textField.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)

        DispatchQueue.main.async {
            focus(textField, coordinator: context.coordinator)
        }

        return textField
    }

    func updateNSView(_ textField: NSTextField, context: Context) {
        context.coordinator.text = $text
        context.coordinator.onSubmit = onSubmit
        context.coordinator.onHistoryPrevious = onHistoryPrevious
        context.coordinator.onHistoryNext = onHistoryNext
        context.coordinator.onMoveDownFromCurrent = onMoveDownFromCurrent

        if textField.stringValue != text {
            textField.stringValue = text
        }
        if textField.placeholderAttributedString?.string != placeholder {
            textField.placeholderAttributedString = attributedPlaceholder
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(
            text: $text,
            onSubmit: onSubmit,
            onHistoryPrevious: onHistoryPrevious,
            onHistoryNext: onHistoryNext,
            onMoveDownFromCurrent: onMoveDownFromCurrent
        )
    }

    private func focus(_ textField: NSTextField, coordinator: Coordinator) {
        guard !coordinator.didApplyInitialFocus, let window = textField.window else { return }
        coordinator.didApplyInitialFocus = true
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(textField)

        if let editor = window.fieldEditor(false, for: textField) as? NSTextView {
            let end = textField.stringValue.utf16.count
            editor.setSelectedRange(NSRange(location: end, length: 0))
        }
    }

    private var attributedPlaceholder: NSAttributedString {
        NSAttributedString(
            string: placeholder,
            attributes: [
                .foregroundColor: NSColor(TerminalPalette.dimText),
                .font: Self.textFont
            ]
        )
    }

    final class Coordinator: NSObject, NSTextFieldDelegate {
        var text: Binding<String>
        var onSubmit: () -> Void
        var onHistoryPrevious: () -> Bool
        var onHistoryNext: () -> Bool
        var onMoveDownFromCurrent: () -> Bool
        var didApplyInitialFocus = false

        init(
            text: Binding<String>,
            onSubmit: @escaping () -> Void,
            onHistoryPrevious: @escaping () -> Bool,
            onHistoryNext: @escaping () -> Bool,
            onMoveDownFromCurrent: @escaping () -> Bool
        ) {
            self.text = text
            self.onSubmit = onSubmit
            self.onHistoryPrevious = onHistoryPrevious
            self.onHistoryNext = onHistoryNext
            self.onMoveDownFromCurrent = onMoveDownFromCurrent
        }

        @objc func submit() {
            onSubmit()
        }

        func controlTextDidChange(_ notification: Notification) {
            guard let textField = notification.object as? NSTextField else { return }
            text.wrappedValue = textField.stringValue
        }

        func control(
            _ control: NSControl,
            textView: NSTextView,
            doCommandBy commandSelector: Selector
        ) -> Bool {
            if commandSelector == #selector(NSResponder.moveUp(_:)),
               hasNoNavigationModifiers {
                guard onHistoryPrevious() else { return false }
                applyBoundText(to: textView)
                return true
            }

            if commandSelector == #selector(NSResponder.moveDown(_:)),
               hasNoNavigationModifiers {
                guard onHistoryNext() else {
                    return onMoveDownFromCurrent()
                }
                applyBoundText(to: textView)
                return true
            }

            guard commandSelector == #selector(NSResponder.insertNewline(_:)),
                  NSApp.currentEvent?.modifierFlags
                    .intersection(.deviceIndependentFlagsMask)
                    .contains(.control) == true
            else {
                return false
            }

            textView.insertNewlineIgnoringFieldEditor(nil)
            text.wrappedValue = textView.string
            return true
        }

        private var hasNoNavigationModifiers: Bool {
            let flags = NSApp.currentEvent?.modifierFlags
                .intersection(.deviceIndependentFlagsMask) ?? []
            let disallowed: NSEvent.ModifierFlags = [.command, .control, .option, .shift]
            return flags.intersection(disallowed).isEmpty
        }

        private func applyBoundText(to textView: NSTextView) {
            let value = text.wrappedValue
            textView.string = value
            textView.setSelectedRange(NSRange(location: value.utf16.count, length: 0))
        }
    }
}
