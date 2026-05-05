import AppKit
import SwiftUI

struct PromptTextField: NSViewRepresentable {
    @Binding var text: String
    let placeholder: String
    let onSubmit: () -> Void

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
        textField.font = NSFont.monospacedSystemFont(ofSize: 12, weight: .regular)
        textField.textColor = NSColor(TerminalPalette.primaryText)
        textField.placeholderString = placeholder
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

        if textField.stringValue != text {
            textField.stringValue = text
        }
        if textField.placeholderString != placeholder {
            textField.placeholderString = placeholder
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(text: $text, onSubmit: onSubmit)
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

    final class Coordinator: NSObject, NSTextFieldDelegate {
        var text: Binding<String>
        var onSubmit: () -> Void
        var didApplyInitialFocus = false

        init(text: Binding<String>, onSubmit: @escaping () -> Void) {
            self.text = text
            self.onSubmit = onSubmit
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
    }
}
