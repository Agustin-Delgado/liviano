// Embeds the application manifest (common controls v6, per-monitor DPI,
// UTF-8) through the linker, and the icon through the Windows SDK's resource
// compiler (nea.rc, plus a version block written here from Cargo.toml).
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
        let rc = versioned_rc(root);
        embed_resource::compile(&rc, embed_resource::NONE)
            .manifest_required()
            .expect("nea.rc did not compile: is the Windows SDK (rc.exe) installed?");
    }
}

/// nea.rc with a VERSIONINFO block after it, written to OUT_DIR. Windows
/// shows it in the file's Properties, and antivirus heuristics weigh an
/// executable that says nothing about itself as a risk.
fn versioned_rc(root: &std::path::Path) -> std::path::PathBuf {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let mut parts: Vec<u16> = version.split(['.', '-', '+']).filter_map(|p| p.parse().ok()).take(3).collect();
    parts.resize(4, 0);
    let numbers = parts.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    let icon = root.join("assets").join("logo").join("nea.ico");
    let source = std::fs::read_to_string(root.join("nea.rc"))
        .unwrap()
        .replace("\"assets/logo/nea.ico\"", &format!("\"{}\"", icon.display().to_string().replace('\\', "\\\\")));
    let text = format!(
        r#"{source}
1 VERSIONINFO
FILEVERSION {numbers}
PRODUCTVERSION {numbers}
FILEFLAGSMASK 0x3F
FILEFLAGS 0x0
FILEOS 0x40004
FILETYPE 0x1
FILESUBTYPE 0x0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Nea"
      VALUE "FileDescription", "Nea"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "nea"
      VALUE "LegalCopyright", "Copyright (c) 2026 The Nea authors. MIT License."
      VALUE "OriginalFilename", "nea.exe"
      VALUE "ProductName", "Nea"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("nea.rc");
    std::fs::write(&out, text).unwrap();
    out
}
