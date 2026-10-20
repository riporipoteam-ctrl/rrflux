import SwiftUI

struct LauncherBackdrop: View {
    var body: some View {
        ZStack {
            Color(uiColor: .systemBackground).ignoresSafeArea()
            Circle()
                .fill(.blue.opacity(0.24))
                .frame(width: 430)
                .blur(radius: 70)
                .offset(x: -170, y: -280)
            Circle()
                .fill(.orange.opacity(0.22))
                .frame(width: 380)
                .blur(radius: 80)
                .offset(x: 190, y: 310)
        }
    }
}

struct BrandMark: View {
    var compact = false

    var body: some View {
        HStack(spacing: 12) {
            Image("ripo-team-logo")
                .resizable()
                .scaledToFill()
                .clipShape(Circle())
                .shadow(color: .blue.opacity(0.18), radius: 10, y: 5)
            .frame(width: compact ? 44 : 72, height: compact ? 44 : 72)
            VStack(alignment: .leading, spacing: 2) {
                Text("Ripo Team")
                    .font(compact ? .headline : .title.bold())
                Text("Launcher")
                    .font(compact ? .caption : .subheadline)
                    .foregroundStyle(.secondary)
            }
        }
    }
}

extension View {
    @ViewBuilder
    func launcherGlass() -> some View {
        #if compiler(>=6.2)
        if #available(iOS 26.0, *) {
            self.glassEffect(.regular, in: RoundedRectangle(cornerRadius: 28, style: .continuous))
        } else {
            self.background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 28, style: .continuous))
        }
        #else
        self.background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 28, style: .continuous))
        #endif
    }

    func launcherField() -> some View {
        self
            .padding(.horizontal, 16)
            .frame(height: 52)
            .background(.thinMaterial, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
    }
}

struct PrimaryLauncherButton: ButtonStyle {
    var compact = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.headline)
            .foregroundStyle(.white)
            .padding(.horizontal, compact ? 20 : 24)
            .frame(minHeight: compact ? 46 : 52)
            .background(Color.accentColor.gradient, in: Capsule())
            .scaleEffect(configuration.isPressed ? 0.96 : 1)
            .animation(.spring(response: 0.25), value: configuration.isPressed)
    }
}

struct SecondaryLauncherButton: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.headline)
            .frame(maxWidth: .infinity, minHeight: 50)
            .background(.thinMaterial, in: Capsule())
            .scaleEffect(configuration.isPressed ? 0.98 : 1)
    }
}
