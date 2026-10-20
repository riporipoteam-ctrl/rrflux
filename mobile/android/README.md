# Ripo Team Launcher for Android

Build the APK with Android SDK 35 and Java 17:

```sh
cd mobile/android
gradle :app:assembleDebug
```

Set `FIREBASE_WEB_API_KEY` in the build environment to enable email sign-in/sign-up. Guest mode works without it.

The APK currently provides the native launcher shell and the `fluxrec://play` integration seam. The repository does not contain an Android Unity player, native libraries, or game data that can be compiled into the app. Those files must be supplied as an authorized Android game module before Flux Rec itself can run inside this single APK.
