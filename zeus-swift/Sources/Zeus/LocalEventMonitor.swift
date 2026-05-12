import AppKit
import SwiftUI

struct LocalEventMonitor: NSViewRepresentable {
    let onEvent: (NSEvent) -> Bool

    func makeCoordinator() -> Coordinator {
        Coordinator(onEvent: onEvent)
    }

    func makeNSView(context: Context) -> NSView {
        context.coordinator.start()
        return NSView(frame: .zero)
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        context.coordinator.onEvent = onEvent
    }

    static func dismantleNSView(_ nsView: NSView, coordinator: Coordinator) {
        coordinator.stop()
    }

    final class Coordinator {
        var onEvent: (NSEvent) -> Bool
        private var monitor: Any?

        init(onEvent: @escaping (NSEvent) -> Bool) {
            self.onEvent = onEvent
        }

        deinit {
            stop()
        }

        func start() {
            guard monitor == nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(
                matching: [.keyDown, .leftMouseUp, .rightMouseUp]
            ) { [weak self] event in
                guard let self else { return event }
                return self.onEvent(event) ? nil : event
            }
        }

        func stop() {
            if let monitor {
                NSEvent.removeMonitor(monitor)
            }
            monitor = nil
        }
    }
}

