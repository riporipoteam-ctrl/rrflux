import SwiftUI

struct LibraryView: View {
    @EnvironmentObject private var session: SessionStore
    @Environment(\.openURL) private var openURL
    @State private var message = "Ready"
    @State private var isLaunching = false

    var body: some View {
        NavigationStack {
            ZStack {
                LauncherBackdrop()
                ScrollView {
                    VStack(alignment: .leading, spacing: 22) {
                        HStack {
                            BrandMark(compact: true)
                            Spacer()
                            Menu {
                                Toggle("Dark mode", isOn: $session.prefersDarkMode)
                                Button("Sign out", role: .destructive) { session.signOut() }
                            } label: {
                                Image(systemName: "person.crop.circle.fill")
                                    .font(.title)
                            }
                        }

                        Text("Your library")
                            .font(.largeTitle.bold())

                        VStack(alignment: .leading, spacing: 18) {
                            ZStack(alignment: .bottomLeading) {
                                LinearGradient(colors: [.orange, .pink, .purple], startPoint: .topLeading, endPoint: .bottomTrailing)
                                VStack(alignment: .leading, spacing: 8) {
                                    Text("FLUX REC")
                                        .font(.system(size: 38, weight: .black, design: .rounded))
                                    Text("Explore, play, and create together.")
                                        .font(.headline)
                                }
                                .foregroundStyle(.white)
                                .padding(24)
                            }
                            .frame(height: 230)
                            .clipShape(RoundedRectangle(cornerRadius: 28, style: .continuous))

                            HStack(spacing: 14) {
                                VStack(alignment: .leading, spacing: 4) {
                                    Text("Flux Rec")
                                        .font(.title2.bold())
                                    Text(message)
                                        .foregroundStyle(.secondary)
                                }
                                Spacer()
                                Button(action: play) {
                                    if isLaunching { ProgressView() } else { Label("Play", systemImage: "play.fill") }
                                }
                                .buttonStyle(PrimaryLauncherButton(compact: true))
                                .disabled(isLaunching)
                            }
                        }
                        .padding(18)
                        .launcherGlass()

                        Text("The game uses the same Flux Rec account and multiplayer services as desktop. A native iOS game runtime must be installed in this signed build before Play can start it.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    .padding(20)
                }
            }
            .toolbar(.hidden, for: .navigationBar)
        }
    }

    private func play() {
        guard let url = URL(string: "fluxrec://play") else { return }
        isLaunching = true
        openURL(url) { accepted in
            message = accepted ? "Launching Flux Rec…" : "Flux Rec mobile runtime is not installed in this build."
            isLaunching = false
        }
    }
}
