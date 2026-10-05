// App commands go through Tauri's permission system like plugin commands: only the
// capabilities in capabilities/ grant them (security review, Slice C).
const COMMANDS: &[&str] = &[
    "daemon_state",
    "daemon_start",
    "session_list",
    "session_create",
    "session_attach",
    "session_detach",
    "session_input",
    "session_resize",
    "session_kill",
];

fn main() {
    let manifest = tauri_build::AppManifest::new().commands(COMMANDS);
    if let Err(e) = tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest)) {
        panic!("tauri build step failed: {e:#}");
    }
}
