import AppKit
import SwiftUI

struct WindowConfigurator: NSViewRepresentable {
    private let backgroundColor = NSColor(
        calibratedRed: 0.025,
        green: 0.032,
        blue: 0.034,
        alpha: 1
    )

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async {
            configure(window: view.window)
        }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        DispatchQueue.main.async {
            configure(window: nsView.window)
        }
    }

    private func configure(window: NSWindow?) {
        guard let window else { return }
        window.styleMask.insert(.fullSizeContentView)
        window.titleVisibility = .hidden
        window.titlebarAppearsTransparent = true
        window.titlebarSeparatorStyle = .none
        window.toolbar = nil
        window.backgroundColor = backgroundColor
        window.isOpaque = true
        window.appearance = NSAppearance(named: .darkAqua)
        window.contentView?.wantsLayer = true
        window.contentView?.layer?.backgroundColor = backgroundColor.cgColor
        NSApp.activate(ignoringOtherApps: true)
    }
}
