import SwiftUI
import UIKit

struct LibraryView: View {
    @EnvironmentObject private var session: SessionStore
    @Environment(\.openURL) private var openURL
    @State private var message = "Checking Flux Rec…"
    @State private var isLaunching = false
    @State private var isInstalled = false

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
                                    if isLaunching {
                                        ProgressView()
                                    } else {
                                        Label(isInstalled ? "Play" : "Install with Ripo Team iPhone Installer",
                                              systemImage: isInstalled ? "play.fill" : "iphone.and.arrow.forward")
                                    }
                                }
                                .buttonStyle(PrimaryLauncherButton(compact: true))
                                .disabled(isLaunching)
                            }
                        }
                        .padding(18)
                        .launcherGlass()

                        Text("Install the signed Flux Rec IPA from the Ripo Team iPhone Installer on Windows or macOS. Once installed, this launcher detects and opens it automatically.")
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    .padding(20)
                }
            }
            .toolbar(.hidden, for: .navigationBar)
            .onAppear(perform: refreshInstallState)
            .onReceive(NotificationCenter.default.publisher(for: UIApplication.didBecomeActiveNotification)) { _ in
                refreshInstallState()
            }
        }
    }

    private func refreshInstallState() {
        guard let url = URL(string: "fluxrec://play") else { return }
        isInstalled = UIApplication.shared.canOpenURL(url)
        message = isInstalled ? "Installed and ready" : "Flux Rec is not installed on this iPhone or iPad"
    }

    private func play() {
        guard let url = URL(string: "fluxrec://play") else { return }
        guard UIApplication.shared.canOpenURL(url) else {
            isInstalled = false
            message = "Install the signed Flux Rec IPA with Ripo Team iPhone Installer first."
            return
        }
        isLaunching = true
        openURL(url) { accepted in
            message = accepted ? "Launching Flux Rec…" : "iOS refused to open the installed game."
            isLaunching = false
        }
    }
}
