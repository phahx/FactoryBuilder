fn main() {
    // CI's build number (packaged releases), baked in for the updater.
    println!("cargo:rerun-if-env-changed=FB_BUILD");
    tauri_build::build()
}
