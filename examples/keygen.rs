//! Generates an update signing key pair.
//!
//!     cargo run --example keygen -- <private key file> [public key file]
//!
//! The private file holds the 32-byte seed in hex and must never be
//! committed (keys/*.key is in .gitignore). The public key is printed, and
//! written to the second path when given — keys/update-public.hex is the one
//! built into the app.

#[path = "../src/update_sign.rs"]
#[allow(dead_code)]
mod update_sign;

use std::{fs, io::Write, process::ExitCode};

use ed25519_compact::{KeyPair, Seed};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(private_path) = args.first() else {
        eprintln!("uso: cargo run --example keygen -- <clave privada> [clave pública]");
        return ExitCode::from(2);
    };
    let keys = KeyPair::from_seed(Seed::generate());
    // create_new: an existing private key is never overwritten by accident.
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(private_path)
        .and_then(|mut file| writeln!(file, "{}", update_sign::to_hex(&keys.sk.seed()[..])));
    if let Err(error) = written {
        eprintln!("no se pudo crear {private_path}: {error}");
        return ExitCode::FAILURE;
    }
    let public = update_sign::to_hex(&keys.pk[..]);
    if let Some(public_path) = args.get(1)
        && let Err(error) = fs::write(public_path, format!("{public}\n"))
    {
        eprintln!("no se pudo escribir {public_path}: {error}");
        return ExitCode::FAILURE;
    }
    println!("{public}");
    ExitCode::SUCCESS
}
