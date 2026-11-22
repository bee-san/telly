use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    hoshidicts();
    // The commands of Telly's own Android plugin (TellyPlugin.kt), which its pages may call.
    let telly = tauri_build::InlinedPlugin::new()
        .commands(&["immersive", "ankiState", "ankiAllow", "addCard", "clip"])
        .default_permission(tauri_build::DefaultPermissionRule::AllowAllCommands);
    tauri_build::try_build(tauri_build::Attributes::new().plugin("telly", telly)).expect("failed to run the build script");
}

/// Builds Hoshidicts, the dictionary engine inside Hachidori, into Telly for
/// Android, whose lookups it does (src/lookups.rs). `npm run prepare:hoshidicts`
/// gets its source. Elsewhere it's built only when TELLY_NATIVE_LOOKUPS is
/// set, to try the phone's lookups on a PC.
fn hoshidicts() {
    println!("cargo:rustc-check-cfg=cfg(telly_lookups)");
    println!("cargo:rerun-if-env-changed=TELLY_NATIVE_LOOKUPS");
    let env = |name: &str| std::env::var(name).unwrap_or_default();
    let android = env("CARGO_CFG_TARGET_OS") == "android";
    if !android && std::env::var_os("TELLY_NATIVE_LOOKUPS").is_none() {
        return;
    }
    let source = PathBuf::from(env("CARGO_MANIFEST_DIR")).join("vendor").join("hoshidicts");
    println!("cargo:rerun-if-changed={}", source.join(".telly-commit").display());
    assert!(source.join(".telly-commit").is_file(), "Hoshidicts' source isn't in {}: run `npm run prepare:hoshidicts` first.", source.display());
    let out = PathBuf::from(env("OUT_DIR")).join("hoshidicts");

    let mut configure = Command::new("cmake");
    configure.arg("-S").arg(&source).arg("-B").arg(&out).args(["-DCMAKE_BUILD_TYPE=Release", "-DCMAKE_POSITION_INDEPENDENT_CODE=ON"]);
    if Command::new("ninja").arg("--version").output().is_ok_and(|o| o.status.success()) {
        configure.args(["-G", "Ninja"]);
    }
    if android {
        let ndk = ["NDK_HOME", "ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"]
            .iter()
            .find_map(|name| std::env::var_os(name))
            .expect("NDK_HOME says where the Android NDK is, and it isn't set");
        let abi = match env("CARGO_CFG_TARGET_ARCH").as_str() {
            "aarch64" => "arm64-v8a",
            "arm" => "armeabi-v7a",
            "x86" => "x86",
            _ => "x86_64",
        };
        configure
            .arg(format!("-DCMAKE_TOOLCHAIN_FILE={}", Path::new(&ndk).join("build/cmake/android.toolchain.cmake").display()))
            .arg(format!("-DANDROID_ABI={abi}"))
            .args(["-DANDROID_PLATFORM=android-24", "-DANDROID_STL=c++_static"]);
    }
    run(&mut configure, "configuring Hoshidicts");
    run(Command::new("cmake").arg("--build").arg(&out).args(["--target", "hoshidicts", "--parallel"]), "building Hoshidicts");

    // Hoshidicts first: a linker that reads archives in order finds what it needs in those after it.
    let mut archives = Vec::new();
    find_archives(&out, &mut archives);
    archives.sort_by_key(|a| a.file_name().is_none_or(|n| n != "libhoshidicts.a"));
    for archive in &archives {
        let name = archive.file_stem().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("lib")).expect("an archive's name");
        println!("cargo:rustc-link-search=native={}", archive.parent().expect("an archive's folder").display());
        println!("cargo:rustc-link-lib=static={name}");
    }
    // The C++ library it was compiled against: the NDK's own, or this computer's.
    if android {
        println!("cargo:rustc-link-lib=c++_static");
        println!("cargo:rustc-link-lib=c++abi");
    } else if env("CARGO_CFG_TARGET_VENDOR") == "apple" {
        println!("cargo:rustc-link-lib=c++");
    } else {
        println!("cargo:rustc-link-lib=stdc++");
    }
    println!("cargo:rustc-cfg=telly_lookups");
}

fn run(command: &mut Command, doing: &str) {
    let status = command.status().unwrap_or_else(|e| panic!("{doing} needs cmake, and it didn't start: {e}"));
    assert!(status.success(), "{doing} failed");
}

/// Every static library under `dir`.
fn find_archives(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_archives(&path, found);
        } else if path.extension().is_some_and(|e| e == "a") {
            found.push(path);
        }
    }
}
