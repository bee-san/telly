# Building Telly for Android

Telly for Android is planned in [bee-san/telly#24](https://github.com/bee-san/telly/issues/24): the PC gets shows ready, and the phone plays copies of them offline, looks words up and adds cards to AnkiDroid. This repository is Telly with the Android app added; `upstream` is Telly itself, merged in to keep up.

You need the Android SDK with an NDK, and a Rust toolchain with the Android targets (`rustup target add aarch64-linux-android x86_64-linux-android`). Point `ANDROID_HOME`, `ANDROID_SDK_ROOT` (the same folder, or Gradle refuses) and `NDK_HOME` at them, then:

```sh
npm ci
npx tauri android build --debug --target x86_64 --apk   # for an x86_64 emulator
npx tauri android build --target aarch64 --apk          # for a phone
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
```

On the phone, Telly keeps its library in the app's own storage. FireRedVAD (subtitle sync), the single-instance plugin and Hachidori's windows are left out of the Android build, as the phone doesn't do those jobs.
