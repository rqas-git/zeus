import AppKit
import CoreText
import SwiftUI

@main
struct ZeusApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate

    init() {
        NSApplication.shared.setActivationPolicy(.regular)
        if let fontsDir = Bundle.module.url(forResource: "Fonts", withExtension: nil) {
            let files = (try? FileManager.default.contentsOfDirectory(at: fontsDir, includingPropertiesForKeys: nil)) ?? []
            for url in files where url.pathExtension == "ttf" {
                CTFontManagerRegisterFontsForURL(url as CFURL, .process, nil)
            }
        }
    }

    var body: some Scene {
        WindowGroup {
            ChatWindowScene()
        }
        .windowStyle(.hiddenTitleBar)
        .windowResizability(.contentMinSize)
    }
}

private struct ChatWindowScene: View {
    @State private var viewModel = ChatViewModel()

    var body: some View {
        ChatWindow(viewModel: viewModel)
            .frame(minWidth: 860, minHeight: 560)
            .task {
                await viewModel.start()
            }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApplication.shared.setActivationPolicy(.regular)
        NSApplication.shared.activate(ignoringOtherApps: true)
    }
}
