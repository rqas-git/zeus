import AppKit
import SwiftUI

struct PromptTextField: NSViewRepresentable {
    private static let textFont = TerminalTypography.chatSmallNSFont

    @Binding var text: String
    @Binding var selectionLocation: Int?
    let placeholder: String
    let onSubmit: () -> Void
    let onHistoryPrevious: () -> Bool
    let onHistoryNext: () -> Bool
    let onTextEdited: (String, Int) -> Void
    let onCompletionTrigger: (Int) -> Bool
    let onCompletionMove: (Int) -> Bool
    let onCompletionAccept: () -> Bool
    let onCompletionCancel: () -> Bool
    var onMoveDownFromCurrent: () -> Bool = { false }

    init(
        text: Binding<String>,
        selectionLocation: Binding<Int?> = .constant(nil),
        placeholder: String,
        onSubmit: @escaping () -> Void,
        onHistoryPrevious: @escaping () -> Bool,
        onHistoryNext: @escaping () -> Bool,
        onTextEdited: @escaping (String, Int) -> Void = { _, _ in },
        onCompletionTrigger: @escaping (Int) -> Bool = { _ in false },
        onCompletionMove: @escaping (Int) -> Bool = { _ in false },
        onCompletionAccept: @escaping () -> Bool = { false },
        onCompletionCancel: @escaping () -> Bool = { false },
        onMoveDownFromCurrent: @escaping () -> Bool = { false }
    ) {
        _text = text
        _selectionLocation = selectionLocation
        self.placeholder = placeholder
        self.onSubmit = onSubmit
        self.onHistoryPrevious = onHistoryPrevious
        self.onHistoryNext = onHistoryNext
        self.onTextEdited = onTextEdited
        self.onCompletionTrigger = onCompletionTrigger
        self.onCompletionMove = onCompletionMove
        self.onCompletionAccept = onCompletionAccept
        self.onCompletionCancel = onCompletionCancel
        self.onMoveDownFromCurrent = onMoveDownFromCurrent
    }

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
        context.coordinator.selectionLocation = $selectionLocation
        context.coordinator.onSubmit = onSubmit
        context.coordinator.onHistoryPrevious = onHistoryPrevious
        context.coordinator.onHistoryNext = onHistoryNext
        context.coordinator.onTextEdited = onTextEdited
        context.coordinator.onCompletionTrigger = onCompletionTrigger
        context.coordinator.onCompletionMove = onCompletionMove
        context.coordinator.onCompletionAccept = onCompletionAccept
        context.coordinator.onCompletionCancel = onCompletionCancel
        context.coordinator.onMoveDownFromCurrent = onMoveDownFromCurrent

        if textField.stringValue != text {
            textField.stringValue = text
        }
        if textField.placeholderAttributedString?.string != placeholder {
            textField.placeholderAttributedString = attributedPlaceholder
        }
        if let selectionLocation {
            applySelection(selectionLocation, to: textField)
            let selection = $selectionLocation
            DispatchQueue.main.async {
                selection.wrappedValue = nil
            }
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(
            text: $text,
            selectionLocation: $selectionLocation,
            onSubmit: onSubmit,
            onHistoryPrevious: onHistoryPrevious,
            onHistoryNext: onHistoryNext,
            onTextEdited: onTextEdited,
            onCompletionTrigger: onCompletionTrigger,
            onCompletionMove: onCompletionMove,
            onCompletionAccept: onCompletionAccept,
            onCompletionCancel: onCompletionCancel,
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

    private func applySelection(_ location: Int, to textField: NSTextField) {
        guard let editor = textField.currentEditor() as? NSTextView else { return }
        let clamped = max(0, min(location, textField.stringValue.utf16.count))
        editor.setSelectedRange(NSRange(location: clamped, length: 0))
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
        var selectionLocation: Binding<Int?>
        var onSubmit: () -> Void
        var onHistoryPrevious: () -> Bool
        var onHistoryNext: () -> Bool
        var onTextEdited: (String, Int) -> Void
        var onCompletionTrigger: (Int) -> Bool
        var onCompletionMove: (Int) -> Bool
        var onCompletionAccept: () -> Bool
        var onCompletionCancel: () -> Bool
        var onMoveDownFromCurrent: () -> Bool
        var didApplyInitialFocus = false

        init(
            text: Binding<String>,
            selectionLocation: Binding<Int?>,
            onSubmit: @escaping () -> Void,
            onHistoryPrevious: @escaping () -> Bool,
            onHistoryNext: @escaping () -> Bool,
            onTextEdited: @escaping (String, Int) -> Void,
            onCompletionTrigger: @escaping (Int) -> Bool,
            onCompletionMove: @escaping (Int) -> Bool,
            onCompletionAccept: @escaping () -> Bool,
            onCompletionCancel: @escaping () -> Bool,
            onMoveDownFromCurrent: @escaping () -> Bool
        ) {
            self.text = text
            self.selectionLocation = selectionLocation
            self.onSubmit = onSubmit
            self.onHistoryPrevious = onHistoryPrevious
            self.onHistoryNext = onHistoryNext
            self.onTextEdited = onTextEdited
            self.onCompletionTrigger = onCompletionTrigger
            self.onCompletionMove = onCompletionMove
            self.onCompletionAccept = onCompletionAccept
            self.onCompletionCancel = onCompletionCancel
            self.onMoveDownFromCurrent = onMoveDownFromCurrent
        }

        @objc func submit() {
            onSubmit()
        }

        func controlTextDidChange(_ notification: Notification) {
            guard let textField = notification.object as? NSTextField else { return }
            let value = textField.stringValue
            text.wrappedValue = value
            selectionLocation.wrappedValue = nil
            onTextEdited(value, currentCursorLocation(in: textField))
        }

        func control(
            _ control: NSControl,
            textView: NSTextView,
            doCommandBy commandSelector: Selector
        ) -> Bool {
            if commandSelector == #selector(NSResponder.cancelOperation(_:)),
               onCompletionCancel() {
                return true
            }

            if commandSelector == #selector(NSResponder.insertTab(_:)) {
                if onCompletionAccept() {
                    applyBoundText(to: textView)
                    return true
                }
                return onCompletionTrigger(textView.selectedRange().location)
            }

            if commandSelector == #selector(NSResponder.moveUp(_:)),
               hasNoNavigationModifiers {
                if onCompletionMove(-1) {
                    return true
                }
                guard onHistoryPrevious() else { return false }
                applyBoundText(to: textView)
                return true
            }

            if commandSelector == #selector(NSResponder.moveDown(_:)),
               hasNoNavigationModifiers {
                if onCompletionMove(1) {
                    return true
                }
                guard onHistoryNext() else {
                    return onMoveDownFromCurrent()
                }
                applyBoundText(to: textView)
                return true
            }

            if commandSelector == #selector(NSResponder.insertNewline(_:)),
               hasNoNavigationModifiers,
               onCompletionAccept() {
                applyBoundText(to: textView)
                return true
            }

            guard commandSelector == #selector(NSResponder.insertNewline(_:)),
                  NSApp.currentEvent?.modifierFlags
                    .intersection(.deviceIndependentFlagsMask)
                    .contains(.control) == true
            else { return false }

            textView.insertNewlineIgnoringFieldEditor(nil)
            text.wrappedValue = textView.string
            selectionLocation.wrappedValue = nil
            onTextEdited(textView.string, textView.selectedRange().location)
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
            let location = selectionLocation.wrappedValue ?? value.utf16.count
            let clamped = max(0, min(location, value.utf16.count))
            textView.setSelectedRange(NSRange(location: clamped, length: 0))
            selectionLocation.wrappedValue = nil
        }

        private func currentCursorLocation(in textField: NSTextField) -> Int {
            guard let editor = textField.currentEditor() as? NSTextView else {
                return textField.stringValue.utf16.count
            }
            return editor.selectedRange().location
        }
    }
}
