# Building Telly for Android

Telly for Android ([bee-san/telly#24](https://github.com/bee-san/telly/issues/24)) is the same app built for Android: the PC gets shows ready, and the phone plays copies of them offline, looks words up and adds cards to AnkiDroid ([the manual](manual.md#-telly-on-your-phone) says what it does). Its Android project is `src-tauri/gen/android`.

## What you need

- The Android SDK with an NDK, and `ANDROID_HOME`, `ANDROID_SDK_ROOT` (the same folder, or Gradle refuses) and `NDK_HOME` pointing at them.
- Rust with the Android targets: `rustup target add aarch64-linux-android x86_64-linux-android`.
- JDK 17.

## Building

```sh
npm ci
npm run prepare:hachidori    # the pinned Hachidori, whose dictionary engine the phone uses
npm run prepare:hoshidicts   # copies that engine (hoshidicts.mjs and .wasm) into static/hoshi
npx tauri android build --target aarch64 --apk           # for a phone
npx tauri android build --debug --target x86_64 --apk    # for an x86_64 emulator
```

`npm run android` does the first three steps and the build.

A release APK comes out unsigned (`src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release-unsigned.apk`). Sign it with `apksigner` from the SDK's build-tools before installing it:

```sh
apksigner sign --ks <your keystore> --out telly.apk app-universal-release-unsigned.apk
adb install -r telly.apk
```

## Clips on cards: FFmpegKit

Cards' pictures and audio are cut with [FFmpegKitNext](https://github.com/arthenica/ffmpeg-kit-next), which publishes no packages. Build its Android `.aar` with SVT-AV1 and Opus (its `android.sh`, for arm64-v8a and x86_64) and put it in `src-tauri/gen/android/app/libs/`; the next build includes it. Without it, cards are added without their picture and audio.

## What's different on the phone

- The backend starts only what the phone does: copying episodes from the PC (`phone.rs`) and line search. Nothing is converted, synced, downloaded or premined there.
- FireRedVAD (subtitle sync), the single-instance plugin and Hachidori's windows are left out of the Android build.
- `TellyPlugin.kt` is what the pages ask of Android: the player's whole screen, FFmpegKit, and AnkiDroid's API.
- Lookups run Hoshidicts in the page (`src/lib/lookup.svelte.ts`), with your dictionaries kept in its IndexedDB.

## Trying it against a PC

`npm run devserver -- <data dir>` runs Telly's backend as the PC would: turn on Settings → Your phone (`setPhoneSharing`), and an emulator reaches it at `10.0.2.2:8775`.
