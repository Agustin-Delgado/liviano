// Embeds the application manifest (common controls v6, per-monitor DPI,
// UTF-8) through the linker, and the icon through the Windows SDK's resource
// compiler (nea.rc).
//
// It also sets the `nea_tests` cfg when the test suite is mounted at `tests/`.
// The suite is a private repository (a git submodule). Without it, each
// module drops its test hook and the build has 0 tests.
fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    println!("cargo::rustc-check-cfg=cfg(nea_tests)");
    println!("cargo::rerun-if-changed=tests/README.md");
    if root.join("tests").join("README.md").is_file() {
        println!("cargo::rustc-cfg=nea_tests");
    }
    let manifest = root.join("nea.manifest");
    println!("cargo:rerun-if-changed=nea.manifest");
    println!("cargo:rerun-if-changed=nea.rc");
    println!("cargo:rerun-if-changed=assets/logo/nea.ico");
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Only the binaries carry the icon; tests and examples don't need it.
        embed_resource::compile(root.join("nea.rc"), embed_resource::NONE)
            .manifest_required()
            .expect("nea.rc did not compile: is the Windows SDK (rc.exe) installed?");
    }
}
