# Flux Rec mobile delivery

This directory contains three independent deliverables:

- `android`: the native Ripo Team Launcher APK shell, with Firebase account login, guest mode, and the Flux Rec library. CI's APK is debug-signed and intended for testing; stable installs and updates need a release signing key.
- `ios/RipoTeamLauncher`: the native SwiftUI launcher, including iOS 26 Liquid Glass and a fallback for older iOS versions.
- `sideloader`: a Windows installer that discovers paired iPhones over USB or Wi-Fi and installs a signed launcher IPA.

## Game runtime boundary

The repository contains a Windows Rec Room executable and a Windows x64 DLL patch. Neither binary can execute on Android or iOS. The launcher projects expose a `fluxrec://play` integration seam for an authorized native mobile game runtime. Cross-play requires that runtime to use the same Flux Rec API and Photon configuration as desktop.

An Android or iOS Rec Room package downloaded from a third party cannot be redistributed as part of Ripo Team Launcher without the necessary rights, and an App Store IPA cannot simply be nested inside another iOS app. iOS additionally validates the app signature and provisioning profile at install and launch.

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
