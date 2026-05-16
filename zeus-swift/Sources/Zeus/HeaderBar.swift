import SwiftUI

struct TerminalBackground: View {
    var body: some View {
        ZStack {
            TerminalPalette.background
            Rectangle()
                .strokeBorder(TerminalPalette.border.opacity(0.24), lineWidth: 1)
        }
        .ignoresSafeArea()
    }
}

struct HeaderBar: View {
    let isLoggedIn: Bool
    let canClearContext: Bool
    let canCompactContext: Bool
    let onClearContext: () -> Void
    let onCompactContext: () -> Void
    let onLogin: () -> Void
    let onLoginStatus: () -> Void
    @State private var isShowingSettings = false

    var body: some View {
        HStack {
            Text("zeus")
                .font(TerminalTypography.chat)
                .foregroundStyle(TerminalPalette.dimText)
                .padding(.leading, TerminalLayout.headerTitleLeadingPadding)
                .allowsHitTesting(false)

            Spacer()

            HStack(spacing: 6) {
                HeaderActionButton(
                    title: "compact",
                    isEnabled: canCompactContext,
                    help: "Compact Context"
                ) {
                    isShowingSettings = false
                    onCompactContext()
                }

                HeaderActionButton(
                    title: "new session",
                    isEnabled: canClearContext,
                    help: "New Session"
                ) {
                    isShowingSettings = false
                    onClearContext()
                }

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

private struct HeaderActionButton: View {
    let title: String
    let isEnabled: Bool
    let help: String
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Text(title)
                .font(TerminalTypography.chatSmallBold)
                .foregroundStyle(
                    isEnabled
                        ? TerminalPalette.dimText
                        : TerminalPalette.dimText.opacity(0.35)
                )
                .padding(.horizontal, 6)
                .frame(height: 16)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(TerminalPalette.backgroundLow)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .stroke(
                            isEnabled
                                ? TerminalPalette.border.opacity(0.55)
                                : TerminalPalette.border.opacity(0.25),
                            lineWidth: 1
                        )
                )
                .contentShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
        }
        .buttonStyle(.plain)
        .disabled(!isEnabled)
        .help(help)
        .accessibilityLabel(help)
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
                        .font(TerminalTypography.chatSmall)
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
        .terminalDropdownChrome()
    }
}
