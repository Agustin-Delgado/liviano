// Embeds the application manifest (common controls v6, per-monitor DPI,
// UTF-8) through the linker, and the icon through the Windows SDK's resource
// compiler (liviano.rc).
fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = root.join("liviano.manifest");
    println!("cargo:rerun-if-changed=liviano.manifest");
    println!("cargo:rerun-if-changed=liviano.rc");
    println!("cargo:rerun-if-changed=assets/logo/liviano.ico");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Only the binaries carry the icon; tests and examples don't need it.
        embed_resource::compile(root.join("liviano.rc"), embed_resource::NONE)
            .manifest_required()
            .expect("liviano.rc did not compile: is the Windows SDK (rc.exe) installed?");
    }
}
