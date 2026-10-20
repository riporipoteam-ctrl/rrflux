# Flux Rec Android runtime patch

This folder builds the native ARM64 hook used by a user-supplied 2025 Android Rec Room package. No Rec Room executable or asset is stored in this repository.

The hook resolves IL2CPP classes and methods at runtime, so it does not depend on the encrypted 2025 `global-metadata.dat` file. It performs the three changes required to reach Flux Rec:

- redirects `ns.rec.net` requests to `ns.ripo-ripoteam.workers.dev`;
- supplies the Flux Rec Photon Realtime and Voice application IDs and fixes the region to `us`;
- accepts the self-hosted RecNet TLS chain in BestHTTP.

The Android launcher accepts a signed XAPK/ZIP containing all split APKs, installs them in one PackageInstaller session, and launches `com.AgainstGravity.RecRoom`.

## Build the native library

Requirements: Android NDK r27 or newer, CMake 3.22 or newer, Ninja, and Git.

```powershell
cmake -S . -B build -G Ninja `
  -DCMAKE_TOOLCHAIN_FILE="$env:ANDROID_NDK/build/cmake/android.toolchain.cmake" `
  -DANDROID_ABI=arm64-v8a -DANDROID_PLATFORM=android-26
cmake --build build
```

The result is `build/libfluxrec_mobile.so`. The package step must place it in the ARM64 split next to `libmain.so`, add `libfluxrec_mobile.so` as a `DT_NEEDED` dependency of `libmain.so`, then align and sign every split with the same certificate. The launcher will reject a bundle whose splits are missing or signed with different keys.

## Patch a split XAPK

Install the one packaging dependency and run the patcher with your persistent signing key:

```powershell
python -m pip install lief==0.17.3
python patch_xapk.py `
  --xapk C:\path\to\2025071801.xapk `
  --hook build\libfluxrec_mobile.so `
  --output C:\path\to\FluxRec-Android-2025071801.xapk `
  --build-tools "$env:ANDROID_SDK_ROOT\build-tools\35.0.0" `
  --keystore C:\safe\path\fluxrec-mobile.keystore
```

Keep the keystore. Android updates must use the same signing certificate. Copy the resulting XAPK to the phone, open Ripo Team Launcher, choose **Install game**, and select it.

The hook is intended for packages the player is authorized to use. Distribution builds must not embed third-party game binaries.
