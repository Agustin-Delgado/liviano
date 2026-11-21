//! Signs an update package and writes the manifest the updater reads.
//!
//!     cargo run --example sign -- <package> <private key file> [options]
//!
//! Options:
//!   --version <x.y.z>   version being published (default: this crate's)
//!   --url <address>     where the package will be served; absolute, or
//!                       relative to the manifest (default: the file name)
//!   --out <file>        manifest path (default: latest.json beside the package)

#[path = "../src/update_sign.rs"]
mod update_sign;

use std::{fs, path::PathBuf, process::ExitCode};

use ed25519_compact::{KeyPair, PublicKey, Seed};

fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut positional = Vec::new();
    let mut version = env!("CARGO_PKG_VERSION").to_string();
    let mut url = None;
    let mut out = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" => version = args.next().unwrap_or_default(),
            "--url" => url = args.next(),
            "--out" => out = args.next().map(PathBuf::from),
            _ => positional.push(arg),
        }
    }
    let [package_path, key_path] = positional.as_slice() else {
        return fail(
            "usage: cargo run --example sign -- <package> <private key> [--version x.y.z] [--url url] [--out latest.json]",
        );
    };
    let version = version.trim().trim_start_matches(['v', 'V']).to_string();
    if version.is_empty() || !version.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())) {
        return fail(&format!("invalid version: {version} (expected 1.2.3)"));
    }
    let seed = match fs::read_to_string(key_path).ok().and_then(|t| update_sign::from_hex(&t)) {
        Some(bytes) => match Seed::from_slice(&bytes) {
            Ok(seed) => seed,
            Err(_) => return fail("the private key must be 32 bytes in hexadecimal"),
        },
        None => return fail(&format!("cannot read the private key {key_path}")),
    };
    let keys = KeyPair::from_seed(seed);
    let package = match fs::read(package_path) {
        Ok(bytes) => bytes,
        Err(error) => return fail(&format!("cannot read {package_path}: {error}")),
    };
    let mut message = update_sign::signed_prefix(&version);
    message.extend_from_slice(&package);
    let signature = keys.sk.sign(&message, None);
    if keys.pk.verify(&message, &signature).is_err() {
        return fail("the new signature does not verify");
    }

    let package_path = PathBuf::from(package_path);
    let file_name = package_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let manifest = serde_json::json!({
        "version": version,
        "url": url.unwrap_or(file_name),
        "size": package.len(),
        "signature": update_sign::to_hex(signature.as_ref()),
    });
    let out = out.unwrap_or_else(|| package_path.with_file_name("latest.json"));
    let text = serde_json::to_string_pretty(&manifest).unwrap_or_default();
    if let Err(error) = fs::write(&out, format!("{text}\n")) {
        return fail(&format!("cannot write {}: {error}", out.display()));
    }
    println!("{}", out.display());

    let embedded =
        update_sign::from_hex(include_str!("../keys/update-public.hex")).and_then(|b| PublicKey::from_slice(&b).ok());
    if embedded.as_ref() != Some(&keys.pk) {
        eprintln!(
            "warning: this key is not the key in keys/update-public.hex; a build with that file rejects the package"
        );
    }
    ExitCode::SUCCESS
}
