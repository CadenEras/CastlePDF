fn main() {
    // Listing the commands makes Tauri generate one permission per command;
    // capabilities must then grant each of them explicitly.
    let attributes =
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "worker_run",
            "worker_cancel",
            "current_document",
            "begin_view",
            "page_size",
            "render_tile",
            "page_text",
            "search_page",
        ]));
    if let Err(error) = tauri_build::try_build(attributes) {
        eprintln!("tauri build script failed: {error:#}");
        std::process::exit(1);
    }
}
