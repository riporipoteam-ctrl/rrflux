# Ripo Team Launcher for iOS

This is the native SwiftUI launcher target. It provides Firebase email sign-in/sign-up, guest mode, a Flux Rec library page, light/dark appearance, and iOS 26 Liquid Glass with a Material fallback for older iOS releases.

Generate the Xcode project on macOS:

```sh
brew install xcodegen
cd mobile/ios/RipoTeamLauncher
xcodegen generate
open RipoTeamLauncher.xcodeproj
```

Set `FIREBASE_WEB_API_KEY` as an Xcode build setting. A device IPA additionally needs the Ripo Team Apple Developer team, signing certificate, and provisioning profile. The launcher detects the signed iOS runtime through `fluxrec://play`; build and inject that runtime with `mobile/runtime/ios` before installing it from the Windows iPhone Installer.
