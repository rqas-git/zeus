import SwiftUI

struct TerminalBackground: View {
    var body: some View {
        ZStack {
            TerminalPalette.background
            LinearGradient(
                colors: [
                    TerminalPalette.backgroundHighlight.opacity(0.75),
                    TerminalPalette.backgroundLow
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
            Rectangle()
                .strokeBorder(TerminalPalette.border.opacity(0.24), lineWidth: 1)
        }
        .ignoresSafeArea()
    }
}

struct HeaderBar: View {
    let isLoggedIn: Bool
    let onLogin: () -> Void
    let onLoginStatus: () -> Void
    @State private var isShowingSettings = false

    var body: some View {
        HStack {
            Text("zeus")
                .font(CodexTypography.chat)
                .foregroundStyle(TerminalPalette.dimText)
                .padding(.leading, 66)
                .allowsHitTesting(false)

            Spacer()

            Button {
                isShowingSettings.toggle()
            } label: {
                Image(systemName: "gearshape")
                    .font(.system(size: 10, weight: .regular))
                    .foregroundStyle(
                        isShowingSettings ? TerminalPalette.cyan : TerminalPalette.dimText
                    )
                    .frame(width: 18, height: 16)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .help("Settings")
        }
        .frame(height: 18)
        .overlay(alignment: .topTrailing) {
            if isShowingSettings {
                SettingsDropdown(isLoggedIn: isLoggedIn) {
                    isShowingSettings = false
                    if isLoggedIn {
                        onLoginStatus()
                    } else {
                        onLogin()
                    }
                }
                .offset(y: 20)
                .zIndex(20)
            }
        }
        .zIndex(20)
    }
}

private struct SettingsDropdown: View {
    let isLoggedIn: Bool
    let onLoginAction: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Button(action: onLoginAction) {
                HStack(spacing: 7) {
                    Image(systemName: "person.crop.circle")
                        .font(.system(size: 10, weight: .regular))
                        .foregroundStyle(TerminalPalette.cyan)
                        .frame(width: 12)

                    Text(isLoggedIn ? "Login Status" : "Login")
                        .font(CodexTypography.chatSmall)
                        .foregroundStyle(TerminalPalette.primaryText)
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(TerminalMenuButtonStyle())
        }
        .fixedSize(horizontal: true, vertical: true)
        .background(
            Rectangle()
                .fill(TerminalPalette.background)
        )
        .overlay(
            Rectangle()
                .stroke(TerminalPalette.border.opacity(0.45), lineWidth: 1)
        )
        .shadow(color: TerminalPalette.shadow.opacity(0.18), radius: 8, x: 0, y: 6)
    }
}

struct TerminalMenuButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .background(
                Rectangle()
                    .fill(configuration.isPressed ? TerminalPalette.cyan.opacity(0.12) : .clear)
            )
    }
}
