# Ripo Team iPhone Installer

Windows desktop installer for a signed Ripo Team Launcher IPA. It includes its iPhone device helper, discovers USB and paired Wi-Fi iPhones, validates that an IPA contains an app bundle and provisioning profile, and installs it without shell interpolation.

Requirements:

- Apple Mobile Device Support from the desktop iTunes installer.
- A signed IPA whose provisioning profile includes the target iPhone.
- One initial USB trust/pairing. Wireless discovery then uses the existing pairing record and Wi-Fi sync.

Build on Windows:

```powershell
cd mobile\sideloader
npm install
npm run build
```

The release workflow builds and packages `pymobiledevice3` 11.38.0 as the device helper and includes its corresponding source archive and license. See `THIRD_PARTY_NOTICES.md`.

Set `RIPO_LAUNCHER_IPA_URL` and `RIPO_LAUNCHER_IPA_SHA256_URL` while building to enable the **Download official** button. The app never asks for or stores an Apple ID password.
