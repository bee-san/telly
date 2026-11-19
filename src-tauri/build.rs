fn main() {
    // The commands of Telly's own Android plugin (TellyPlugin.kt), which its pages may call.
    let telly = tauri_build::InlinedPlugin::new()
        .commands(&["immersive", "ankiState", "ankiAllow", "addCard", "clip"])
        .default_permission(tauri_build::DefaultPermissionRule::AllowAllCommands);
    tauri_build::try_build(tauri_build::Attributes::new().plugin("telly", telly)).expect("failed to run the build script");
}
