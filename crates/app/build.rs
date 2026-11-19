fn main() {
    // `rust-embed` and Tauri both read `ui/dist` at compile time. An empty directory keeps a
    // build that runs before the UI build compiling, and the rerun line picks up the real files.
    let dist = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir"))
        .join("../../ui/dist");
    let _ = std::fs::create_dir_all(&dist);
    println!("cargo:rerun-if-changed=../../ui/dist");
    #[cfg(feature = "desktop")]
    tauri_build::build();
}
