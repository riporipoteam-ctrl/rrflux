import SwiftUI

@main
struct RipoTeamLauncherApp: App {
    @StateObject private var session = SessionStore()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(session)
                .preferredColorScheme(session.prefersDarkMode ? .dark : .light)
        }
    }
}

@MainActor
final class SessionStore: ObservableObject {
    @Published var user: LauncherUser?
    @Published var prefersDarkMode = false

    func continueAsGuest() {
        user = LauncherUser(id: UUID().uuidString, name: "Guest", isGuest: true)
    }

    func signOut() {
        user = nil
    }
}

struct LauncherUser: Equatable {
    let id: String
    let name: String
    let isGuest: Bool
}

struct RootView: View {
    @EnvironmentObject private var session: SessionStore

    var body: some View {
        Group {
            if session.user == nil {
                AuthenticationView()
            } else {
                LibraryView()
            }
        }
        .animation(.spring(response: 0.35), value: session.user)
    }
}
