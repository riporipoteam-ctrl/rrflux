# Flux Rec iOS runtime

This builds the same runtime endpoint hook used by Android as an arm64 iOS dylib and injects it into a user-provided **decrypted** 2025 iOS game IPA. It does not store or redistribute game files, certificates, private keys, or provisioning profiles.

## Build on macOS

```sh
brew install cmake ninja
cmake -S mobile/runtime/ios -B build/fluxrec-ios \
  -G Xcode \
  -DCMAKE_SYSTEM_NAME=iOS \
  -DCMAKE_OSX_ARCHITECTURES=arm64 \
  -DCMAKE_OSX_DEPLOYMENT_TARGET=16.0
cmake --build build/fluxrec-ios --config Release
```

## Patch and sign a device IPA

```sh
python3 -m pip install lief==0.17.3
python3 mobile/runtime/ios/patch_ipa.py \
  RecRoomBeta-2025071801-decrypted.ipa \
  build/fluxrec-ios/Release-iphoneos/libfluxrec_mobile.dylib \
  FluxRec-iOS.ipa \
  --identity "Apple Development: Your Name (TEAMID)" \
  --profile FluxRec.mobileprovision \
  --bundle-id com.ripoteam.fluxrec
```

The patched app registers `fluxrec://`, so the Ripo Team Launcher can detect and open it. Apple requires the output to be signed by a certificate and provisioning profile covering the target device. The Windows iPhone Installer in `mobile/sideloader` can then install that signed IPA over USB or a previously paired Wi-Fi connection.
