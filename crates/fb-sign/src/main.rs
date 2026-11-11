//! Signs Factory Builder release zips for the in-app updater.
//!
//! - `fb-sign keygen <key file>`: makes a signing key (kept secret, in a file
//!   outside the repository; CI has it as the UPDATE_SIGNING_KEY secret) and
//!   prints the public key the app checks against (src-tauri update.rs).
//! - `fb-sign sign <zip> <build>`: signs the zip as build `build` with the
//!   key in the UPDATE_SIGNING_KEY environment variable; writes `<zip>.sig`.
//! - `fb-sign verify <zip> <build> <public key>`: checks a signature.
//!
//! What's signed is `fb_model::update::signed_text`: the build number and
//! the zip's SHA-256.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};
use std::path::Path;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn text_for(zip: &Path, build: u32) -> Result<String, String> {
    let bytes = std::fs::read(zip).map_err(|e| format!("Can't read {}: {e}", zip.display()))?;
    Ok(fb_model::update::signed_text(build, &hex(&Sha256::digest(&bytes))))
}

fn key_from(text: &str) -> Result<SigningKey, String> {
    let bytes = B64.decode(text.trim()).map_err(|_| "The signing key isn't base64.".to_string())?;
    let seed: [u8; 32] = bytes.try_into().map_err(|_| "The signing key isn't 32 bytes.".to_string())?;
    Ok(SigningKey::from_bytes(&seed))
}

fn run(args: &[String]) -> Result<(), String> {
    match args {
        [cmd, file] if cmd == "keygen" => {
            let path = Path::new(file);
            if path.exists() {
                return Err(format!("{} already exists; not overwriting a key.", path.display()));
            }
            let mut seed = [0u8; 32];
            getrandom::getrandom(&mut seed).map_err(|e| format!("No randomness: {e}"))?;
            let key = SigningKey::from_bytes(&seed);
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            }
            std::fs::write(path, B64.encode(seed)).map_err(|e| format!("Can't write {}: {e}", path.display()))?;
            println!("{}", B64.encode(key.verifying_key().to_bytes()));
            Ok(())
        }
        [cmd, zip, build] if cmd == "sign" => {
            let build: u32 = build.parse().map_err(|_| "The build isn't a number.".to_string())?;
            let key = key_from(&std::env::var("UPDATE_SIGNING_KEY").map_err(|_| "UPDATE_SIGNING_KEY isn't set.".to_string())?)?;
            let zip = Path::new(zip);
            let sig = key.sign(text_for(zip, build)?.as_bytes());
            let out = format!("{}.sig", zip.display());
            std::fs::write(&out, B64.encode(sig.to_bytes())).map_err(|e| format!("Can't write {out}: {e}"))?;
            println!("Signed {} as build {build} (public key {})", zip.display(), B64.encode(key.verifying_key().to_bytes()));
            Ok(())
        }
        [cmd, zip, build, public] if cmd == "verify" => {
            let build: u32 = build.parse().map_err(|_| "The build isn't a number.".to_string())?;
            let zip = Path::new(zip);
            let key: [u8; 32] = B64.decode(public.trim()).ok().and_then(|b| b.try_into().ok()).ok_or("The public key isn't 32 base64 bytes.")?;
            let key = VerifyingKey::from_bytes(&key).map_err(|e| e.to_string())?;
            let sig_text = std::fs::read_to_string(format!("{}.sig", zip.display())).map_err(|e| e.to_string())?;
            let sig: [u8; 64] = B64.decode(sig_text.trim()).ok().and_then(|b| b.try_into().ok()).ok_or("The signature isn't 64 base64 bytes.")?;
            key.verify(text_for(zip, build)?.as_bytes(), &Signature::from_bytes(&sig)).map_err(|_| "The signature doesn't match.".to_string())?;
            println!("Good signature for build {build}.");
            Ok(())
        }
        _ => Err("Use: fb-sign keygen <key file> | sign <zip> <build> | verify <zip> <build> <public key>".into()),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(e) = run(&args) {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
