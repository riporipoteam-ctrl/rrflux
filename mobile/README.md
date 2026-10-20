# Flux Rec mobile delivery

This directory contains the launcher, installer, and native runtime packaging paths:

- `android`: the native Ripo Team Launcher APK, with Firebase account login, guest mode, split-XAPK installation/update, and installed-game launch.
- `ios/RipoTeamLauncher`: the native SwiftUI launcher, including iOS 26 Liquid Glass, installed-game detection, and a fallback for older iOS versions.
- `sideloader`: a Windows installer that discovers paired iPhones over USB or Wi-Fi and installs a signed launcher IPA.
- `runtime/android`: the ARM64 IL2CPP runtime hook and split-XAPK patch/sign tool.
- `runtime/ios`: the ARM64 IL2CPP runtime hook and decrypted-IPA patch/sign tool.

## Game runtime

The Windows executable cannot run on a phone. The mobile runtime instead patches an authorized native ARM64 game package. It resolves the IL2CPP API after startup, redirects RecNet traffic to the Flux service, and installs the Flux Photon Realtime and Voice IDs used by desktop. The Android launcher installs every XAPK split in one PackageInstaller session. The iOS pipeline injects the dylib into a decrypted IPA, adds `fluxrec://`, and re-signs the result for the target device.

Game packages are intentionally excluded from the repository and launcher artifacts. Supply a package you are authorized to use. iOS also requires a decrypted executable and a certificate/provisioning profile that covers the target device.

## CI signing configuration

The mobile workflow always builds the Android debug APK, the iOS simulator target, and the Windows sideloader. A device-installable, ad hoc signed IPA is also produced when these GitHub Actions secrets are configured. Ad hoc profiles only install on devices registered in the profile; broader iOS distribution requires TestFlight or App Store Connect signing and release setup.

- `IOS_CERTIFICATE_P12_BASE64`
- `IOS_CERTIFICATE_PASSWORD`
- `IOS_PROVISIONING_PROFILE_BASE64`
- `IOS_TEAM_ID`
- `IOS_BUNDLE_ID` (optional; defaults to `com.ripoteam.launcher`)
- `FIREBASE_WEB_API_KEY`

To let the Windows installer download the current signed IPA automatically, also configure:

- `RIPO_LAUNCHER_IPA_URL`
- `RIPO_LAUNCHER_IPA_SHA256_URL`
