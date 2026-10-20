import Foundation
import SwiftUI

struct AuthenticationView: View {
    enum Mode: String, CaseIterable { case signIn = "Sign in", signUp = "Sign up" }

    @EnvironmentObject private var session: SessionStore
    @State private var mode: Mode = .signIn
    @State private var email = ""
    @State private var password = ""
    @State private var isWorking = false
    @State private var error: String?

    var body: some View {
        ZStack {
            LauncherBackdrop()

            ScrollView {
                VStack(spacing: 24) {
                    Spacer(minLength: 44)
                    BrandMark()

                    VStack(spacing: 18) {
                        Picker("Account action", selection: $mode) {
                            ForEach(Mode.allCases, id: \.self) { Text($0.rawValue).tag($0) }
                        }
                        .pickerStyle(.segmented)

                        TextField("Email", text: $email)
                            .textContentType(.emailAddress)
                            .textInputAutocapitalization(.never)
                            .keyboardType(.emailAddress)
                            .launcherField()

                        SecureField("Password", text: $password)
                            .textContentType(mode == .signIn ? .password : .newPassword)
                            .launcherField()

                        if let error {
                            Text(error)
                                .font(.footnote)
                                .foregroundStyle(.red)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }

                        Button(action: authenticate) {
                            Group {
                                if isWorking { ProgressView() } else { Text(mode.rawValue) }
                            }
                            .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(PrimaryLauncherButton())
                        .disabled(isWorking || email.isEmpty || password.count < 6)

                        Button("Continue as guest") { session.continueAsGuest() }
                            .buttonStyle(SecondaryLauncherButton())
                    }
                    .padding(22)
                    .launcherGlass()
                    .frame(maxWidth: 480)

                    Text("Guest play is available, but account sync, private rooms, and cloud photos require a Flux Rec account.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 420)
                    Spacer(minLength: 32)
                }
                .padding(.horizontal, 20)
            }
        }
    }

    private func authenticate() {
        isWorking = true
        error = nil
        Task {
            do {
                let result = try await FirebaseAuthClient.authenticate(
                    email: email.trimmingCharacters(in: .whitespacesAndNewlines),
                    password: password,
                    createAccount: mode == .signUp
                )
                session.user = LauncherUser(id: result.localId, name: result.displayName ?? email.components(separatedBy: "@").first ?? "Player", isGuest: false)
            } catch {
                self.error = error.localizedDescription
            }
            isWorking = false
        }
    }
}

struct AuthResponse: Decodable {
    let localId: String
    let displayName: String?
}

enum FirebaseAuthClient {
    static func authenticate(email: String, password: String, createAccount: Bool) async throws -> AuthResponse {
        guard let key = Bundle.main.object(forInfoDictionaryKey: "FirebaseWebAPIKey") as? String,
              !key.isEmpty,
              !key.contains("$(") else {
            throw AuthError.configuration
        }
        let operation = createAccount ? "signUp" : "signInWithPassword"
        guard let url = URL(string: "https://identitytoolkit.googleapis.com/v1/accounts:\(operation)?key=\(key)") else {
            throw AuthError.configuration
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: [
            "email": email,
            "password": password,
            "returnSecureToken": true
        ])
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            let message = (try? JSONSerialization.jsonObject(with: data) as? [String: Any])
                .flatMap { $0["error"] as? [String: Any] }?["message"] as? String
            throw AuthError.remote(message ?? "Account request failed")
        }
        return try JSONDecoder().decode(AuthResponse.self, from: data)
    }
}

enum AuthError: LocalizedError {
    case configuration
    case remote(String)

    var errorDescription: String? {
        switch self {
        case .configuration: return "Account sign-in is not configured in this build."
        case .remote(let message): return message.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }
}
