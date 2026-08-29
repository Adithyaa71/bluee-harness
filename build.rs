fn main() {
    // Tauri's build step reads tauri.conf.json and generates the context that
    // `tauri::generate_context!()` expands to.
    tauri_build::build();
}
