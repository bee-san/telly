# Building Telly for Android

Telly for Android ([bee-san/telly#24](https://github.com/bee-san/telly/issues/24)) is the same app built for Android: the PC gets shows ready, and the phone plays copies of them offline, looks words up and adds cards to AnkiDroid ([the manual](manual.md#-telly-on-your-phone) says what it does). Its Android project is `src-tauri/gen/android`.

## What you need

- The Android SDK with an NDK, and `ANDROID_HOME`, `ANDROID_SDK_ROOT` (the same folder, or Gradle refuses) and `NDK_HOME` pointing at them.
- Rust with the Android targets: `rustup target add aarch64-linux-android x86_64-linux-android`.
- JDK 17.
- CMake and git, for Hoshidicts (the dictionary engine, C++), which the build compiles into the app.

## Building

```sh
npm ci
npm run prepare:hachidori    # the pinned Hachidori
npm run prepare:hoshidicts   # the source of its dictionary engine, into src-tauri/vendor/hoshidicts
npx tauri android build --target aarch64 --apk           # for a phone
npx tauri android build --debug --target x86_64 --apk    # for an x86_64 emulator
```

`npm run android` does the first three steps and the build.

A release APK is signed with the key in `src-tauri/gen/android/keystore.properties` (kept out of git):

```properties
storeFile=/path/to/telly.jks
storePassword=…
keyAlias=…
keyPassword=…
```

Without that file it comes out unsigned (`app-universal-release-unsigned.apk`), and you sign it with `apksigner` from the SDK's build-tools before installing it.

## Releases and updates

Publishing a release builds the APK on GitHub Actions (`.github/workflows/release.yml`) and attaches it as `Telly_<version>_android.apk`, signed with the key in the repository secrets `ANDROID_KEYSTORE` (the `.jks` file, base64), `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD`. That has to be the key every Telly on a phone was signed with: Android installs an update only over an app signed with the same key. The phone finds the APK on the latest release (`update.rs`) and hands it to Android's installer (`installApk` in `TellyPlugin.kt`).

## Clips on cards: FFmpegKit

Cards' pictures and audio are cut with [FFmpegKitNext](https://github.com/arthenica/ffmpeg-kit-next), which publishes no packages. Build its Android `.aar` with SVT-AV1 and Opus (its `android.sh`, for arm64-v8a and x86_64) and commit it in `src-tauri/gen/android/app/libs/`; every build after, the release's included, has it. Without it, cards are added without their picture and audio.

## What's different on the phone

- The backend starts only what the phone does: copying episodes from the PC (`phone.rs`) and line search. Nothing is converted, synced, downloaded or premined there.
- FireRedVAD (subtitle sync), the single-instance plugin and Hachidori's windows are left out of the Android build.
- `TellyPlugin.kt` is what the pages ask of Android: the player's whole screen, FFmpegKit, and AnkiDroid's API.
- On a device with a second screen, `SecondScreen.kt` shows Telly's page `/second` there in a `Presentation` while the activity is started. That page is in a plain WebView whose requests are answered by the main WebView's client, so it has Telly's pages, fonts and covers but can't call the backend: the pages on the main screen say what it shows and are told what's touched (`src/lib/second.svelte.ts`). Its window doesn't take the focus while the main screen has it, so a touch there leaves the controller with the main screen; while the main screen hasn't (a screen just plugged in takes the focus for its own launcher), it takes the keys and hands them on.
- A controller's buttons reach `MainActivity` as keys and stick movements; `Pad.kt` names them and tells the page (`src/lib/pad.svelte.ts`), which moves the ring and repeats a held direction. Nothing depends on what the WebView does with a controller.
- Lookups are the backend's (`lookups.rs`): Hoshidicts is compiled into the app (`build.rs`), and each of your dictionaries is installed into a folder on the phone and read from there. `TELLY_NATIVE_LOOKUPS=1` builds it into Telly on a PC too, which is how `lookups.rs`'s tests run: `TELLY_NATIVE_LOOKUPS=1 cargo test --manifest-path src-tauri/Cargo.toml --lib lookups`.

## Trying it against a PC

`npm run devserver -- <data dir>` runs Telly's backend as the PC would: turn on Settings → Your phone (`setPhoneSharing`), and an emulator reaches it at `10.0.2.2:8775`.
