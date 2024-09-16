const COMMANDS: &[&str] = &[
    "create",
    "delete",
    "get",
    "rename",
    "force_uninstall_mod",
    "force_toggle_mod",
    "queue_install",
    "launch",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS).build();
}
