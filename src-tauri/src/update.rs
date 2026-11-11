//! In-app updates from GitHub Releases.
//!
//! Every push to main publishes a build (.github/workflows/release.yml) as
//! the latest release: `FactoryBuilder-<version>.zip`, tagged
//! `v<version>-build.<n>`, with `<zip>.sig` beside it. CI signs each zip
//! (crates/fb-sign) with a private key only it has; this app carries the
//! public key and installs nothing whose signature doesn't match. The
//! signature covers the build number and the zip's SHA-256
//! (`fb_model::update::signed_text`), so an older build can't be passed off
//! as a newer one.
//!
//! Only packaged builds update (CI bakes the build number in as FB_BUILD;
//! development builds have none), and only the installed copy: the new exe
//! and tools are copied over it (the running exe is renamed aside first, as
//! Windows allows), it's started, and this one exits. The next start removes
//! the old exe. Nothing happens without the user choosing to update.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use bytes::Bytes;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use fb_model::update::{build_of_tag, signed_text, UpdateInfo, REPO};
use http_body_util::{BodyExt, Empty, Limited};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The key CI signs releases with (its private half is the
/// UPDATE_SIGNING_KEY secret; see crates/fb-sign).
pub const PUBLIC_KEY: &str = "Y+XfG+Y7cyuMplAG6TtDX2LsgZ/V/NjdV4bt0jUPBag=";
const USER_AGENT: &str = "FactoryBuilder-updater";
const JSON_LIMIT: usize = 2 << 20;
const SIG_LIMIT: usize = 1024;
/// A release zip is tens of MB; unpacked, a little more.
const ZIP_LIMIT: usize = 400 << 20;
const UNPACKED_LIMIT: u64 = 1 << 30;
const MAX_FILES: usize = 200;
/// The running exe, set aside during an update (removed on the next start).
const OLD_EXE: &str = "Factory Builder.exe.old";

/// This build's number (packaged builds from CI), if it has one.
pub fn build() -> Option<u32> {
    option_env!("FB_BUILD").and_then(|b| b.parse().ok())
}

/// A newer release, and where its files are.
#[derive(Clone, Debug)]
pub struct Release {
    pub info: UpdateInfo,
    zip: String,
    sig: String,
}

/// Only GitHub's own hosts (the API, the release page, and where release
/// downloads redirect to).
fn allowed_host(host: &str) -> bool {
    host == "github.com" || host == "api.github.com" || host.ends_with(".githubusercontent.com")
}

/// An https:// address on an allowed host: (host, path and query).
fn split_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("https://")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let host = host.to_ascii_lowercase();
    if host.is_empty() || host.contains([':', '@']) || !allowed_host(&host) {
        return None;
    }
    Some((host, path.to_string()))
}

fn tls() -> Result<Arc<rustls::ClientConfig>, String> {
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// GET over HTTPS (certificates checked against the usual public roots),
/// following up to five redirects, all within GitHub's hosts.
async fn get(url: &str, limit: usize, accept: &str, timeout: Duration) -> Result<Bytes, String> {
    let mut url = url.to_string();
    for _ in 0..6 {
        let (host, path) = split_url(&url).ok_or("That isn't a GitHub address.")?;
        let tcp = tokio::time::timeout(Duration::from_secs(15), tokio::net::TcpStream::connect((host.as_str(), 443)))
            .await
            .map_err(|_| "GitHub didn't answer.".to_string())?
            .map_err(|e| format!("Couldn't reach GitHub: {e}"))?;
        let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|_| "That isn't a GitHub address.".to_string())?;
        let stream = tokio_rustls::TlsConnector::from(tls()?).connect(name, tcp).await.map_err(|e| format!("Couldn't connect securely to GitHub: {e}"))?;
        let (mut tx, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream)).await.map_err(|e| format!("GitHub didn't answer properly: {e}"))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let req = hyper::Request::get(path.as_str())
            .header(hyper::header::HOST, host.as_str())
            .header(hyper::header::USER_AGENT, USER_AGENT)
            .header(hyper::header::ACCEPT, accept)
            .body(Empty::<Bytes>::new())
            .map_err(|e| e.to_string())?;
        let body = async {
            let resp = tx.send_request(req).await.map_err(|e| format!("GitHub didn't answer: {e}"))?;
            let status = resp.status();
            if status.is_redirection() {
                let to = resp.headers().get(hyper::header::LOCATION).and_then(|v| v.to_str().ok()).ok_or("GitHub redirected nowhere.")?;
                return Ok(Err(to.to_string()));
            }
            if !status.is_success() {
                return Err(format!("GitHub answered HTTP {}.", status.as_u16()));
            }
            let bytes = Limited::new(resp.into_body(), limit).collect().await.map_err(|_| "The download was too big or cut off.".to_string())?.to_bytes();
            Ok(Ok(bytes))
        };
        match tokio::time::timeout(timeout, body).await.map_err(|_| "GitHub took too long.".to_string())?? {
            Ok(bytes) => return Ok(bytes),
            Err(next) => url = next,
        }
    }
    Err("Too many redirects.".into())
}

/// The latest release, when it's a newer build than `current`.
pub async fn latest(current: u32) -> Result<Option<Release>, String> {
    let body = get(&format!("https://api.github.com/repos/{REPO}/releases/latest"), JSON_LIMIT, "application/vnd.github+json", Duration::from_secs(20)).await?;
    let v: Value = serde_json::from_slice(&body).map_err(|_| "GitHub's answer wasn't readable.".to_string())?;
    let Some(build) = v.get("tag_name").and_then(Value::as_str).and_then(build_of_tag) else { return Ok(None) };
    if build <= current {
        return Ok(None);
    }
    let assets = v.get("assets").and_then(Value::as_array).cloned().unwrap_or_default();
    let named = |a: &Value| a.get("name").and_then(Value::as_str).map(str::to_string);
    let Some(zip) = assets.iter().find(|a| named(a).is_some_and(|n| n.starts_with("FactoryBuilder-") && n.ends_with(".zip"))) else { return Ok(None) };
    let zip_name = named(zip).unwrap_or_default();
    let Some(sig) = assets.iter().find(|a| named(a).as_deref() == Some(&format!("{zip_name}.sig"))) else { return Ok(None) };
    let url = |a: &Value| a.get("browser_download_url").and_then(Value::as_str).map(str::to_string);
    let (Some(zip_url), Some(sig_url)) = (url(zip), url(sig)) else { return Ok(None) };
    let title: String = v.get("name").and_then(Value::as_str).unwrap_or_default().chars().filter(|c| !c.is_control()).take(100).collect();
    let size = zip.get("size").and_then(Value::as_u64).unwrap_or(0);
    Ok(Some(Release { info: UpdateInfo { build, current, title, size }, zip: zip_url, sig: sig_url }))
}

/// Whether `zip` is build `build` as CI signed it.
pub fn verify(zip: &[u8], build: u32, sig_b64: &str, public_b64: &str) -> Result<(), String> {
    let bad = || "The update's signature doesn't match, so it wasn't installed.".to_string();
    let key: [u8; 32] = B64.decode(public_b64.trim()).ok().and_then(|b| b.try_into().ok()).ok_or_else(bad)?;
    let key = VerifyingKey::from_bytes(&key).map_err(|_| bad())?;
    let sig: [u8; 64] = B64.decode(sig_b64.trim()).ok().and_then(|b| b.try_into().ok()).ok_or_else(bad)?;
    let digest: String = Sha256::digest(zip).iter().map(|b| format!("{b:02x}")).collect();
    key.verify(signed_text(build, &digest).as_bytes(), &Signature::from_bytes(&sig)).map_err(|_| bad())
}

/// Unpacks a release zip into `into` (only paths that stay inside it) and
/// returns the folder holding the app's exe.
pub fn unpack(zip: &[u8], into: &Path) -> Result<PathBuf, String> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip)).map_err(|e| format!("The update isn't a readable zip: {e}"))?;
    if archive.len() > MAX_FILES {
        return Err("The update has too many files.".into());
    }
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut f = archive.by_index(i).map_err(|e| format!("The update's zip is damaged: {e}"))?;
        let Some(rel) = f.enclosed_name() else { return Err("The update's zip has a file outside its folder.".into()) };
        let out = into.join(rel);
        if f.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        total += f.size();
        if total > UNPACKED_LIMIT {
            return Err("The update unpacks too big.".into());
        }
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let mut file = std::fs::File::create(&out).map_err(|e| format!("Couldn't unpack the update: {e}"))?;
        std::io::copy(&mut f, &mut file).map_err(|e| format!("Couldn't unpack the update: {e}"))?;
    }
    // The zip holds the app's folder (or its files at the top).
    let top = into.join(crate::install::EXE_NAME);
    if top.is_file() {
        return Ok(into.to_path_buf());
    }
    std::fs::read_dir(into)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.join(crate::install::EXE_NAME).is_file())
        .ok_or_else(|| "The update has no app in it.".to_string())
}

/// Copies the new app from `from` over the installed one in `dir`: its
/// tools first, then the exe (the running one renamed aside; put back if
/// anything goes wrong).
pub fn swap_in(dir: &Path, from: &Path) -> Result<(), String> {
    let tools = dir.join("tools");
    std::fs::create_dir_all(&tools).map_err(|e| e.to_string())?;
    for t in crate::install::TOOLS {
        let src = from.join("tools").join(t);
        if src.is_file() {
            std::fs::copy(&src, tools.join(t)).map_err(|e| format!("Couldn't update tools\\{t}: {e}"))?;
        }
    }
    let exe = dir.join(crate::install::EXE_NAME);
    let old = dir.join(OLD_EXE);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("Couldn't set the running app aside: {e}"))?;
    if let Err(e) = std::fs::copy(from.join(crate::install::EXE_NAME), &exe) {
        let _ = std::fs::remove_file(&exe);
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("Couldn't copy the new app: {e}"));
    }
    Ok(())
}

/// After an update: the old exe goes (it's no longer running).
pub fn cleanup(exe: &Path) {
    if let Some(dir) = exe.parent() {
        let old = dir.join(OLD_EXE);
        for _ in 0..20 {
            if !old.exists() || std::fs::remove_file(&old).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Downloads, checks and unpacks `release` into `work` (emptied first);
/// returns the folder holding the new app.
pub async fn fetch(release: &Release, work: &Path) -> Result<PathBuf, String> {
    let sig = get(&release.sig, SIG_LIMIT, "application/octet-stream", Duration::from_secs(60)).await?;
    let zip = get(&release.zip, ZIP_LIMIT, "application/octet-stream", Duration::from_secs(900)).await?;
    verify(&zip, release.info.build, &String::from_utf8_lossy(&sig), PUBLIC_KEY)?;
    let _ = std::fs::remove_dir_all(work);
    std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
    unpack(&zip, work)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> (SigningKey, String) {
        let k = SigningKey::from_bytes(&[7u8; 32]);
        let public = B64.encode(k.verifying_key().to_bytes());
        (k, public)
    }

    fn sign(k: &SigningKey, zip: &[u8], build: u32) -> String {
        let digest: String = Sha256::digest(zip).iter().map(|b| format!("{b:02x}")).collect();
        B64.encode(k.sign(signed_text(build, &digest).as_bytes()).to_bytes())
    }

    /// GitHub over the network (run by hand: --ignored): the latest release
    /// is read; one without a signature is never offered.
    #[tokio::test]
    #[ignore]
    async fn github_answers() {
        let r = latest(0).await;
        eprintln!("latest: {:?}", r.as_ref().map(|o| o.as_ref().map(|x| &x.info)));
        assert!(r.is_ok(), "{r:?}");
        // A release download, through GitHub's redirect to its file host.
        let body = get(&format!("https://api.github.com/repos/{REPO}/releases/latest"), JSON_LIMIT, "application/vnd.github+json", Duration::from_secs(20)).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let url = v["assets"][0]["browser_download_url"].as_str().unwrap().to_string();
        let zip = get(&url, ZIP_LIMIT, "application/octet-stream", Duration::from_secs(300)).await.unwrap();
        eprintln!("downloaded {} bytes from {url}", zip.len());
        assert!(zip.starts_with(b"PK"), "a zip");
        let into = std::env::temp_dir().join(format!("fb-update-real-{}", std::process::id()));
        let app = unpack(&zip, &into).unwrap();
        eprintln!("unpacked the app at {}", app.display());
        assert!(app.join(crate::install::EXE_NAME).is_file());
        let _ = std::fs::remove_dir_all(&into);
    }

    #[test]
    fn only_a_matching_signature_is_accepted() {
        let (k, public) = key();
        let zip = b"the release zip".to_vec();
        let sig = sign(&k, &zip, 42);
        assert!(verify(&zip, 42, &sig, &public).is_ok());
        assert!(verify(&zip, 43, &sig, &public).is_err(), "an older build passed off as newer");
        assert!(verify(b"something else", 42, &sig, &public).is_err(), "another zip");
        let (other, _) = (SigningKey::from_bytes(&[9u8; 32]), ());
        assert!(verify(&zip, 42, &sign(&other, &zip, 42), &public).is_err(), "signed by someone else");
        assert!(verify(&zip, 42, "not base64!", &public).is_err());
    }

    #[test]
    fn only_github_addresses_are_followed() {
        assert!(split_url("https://api.github.com/repos/x/y/releases/latest").is_some());
        assert!(split_url("https://release-assets.githubusercontent.com/a?b=c").is_some());
        assert!(split_url("http://github.com/x").is_none(), "not plain HTTP");
        assert!(split_url("https://evil.example/x").is_none());
        assert!(split_url("https://github.com.evil.example/x").is_none());
        assert!(split_url("https://github.com:444/x").is_none());
        assert!(split_url("https://user@github.com/x").is_none());
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            for (name, data) in files {
                w.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn unpacks_the_app_folder_and_refuses_escapes() {
        let root = std::env::temp_dir().join(format!("fb-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let zip = zip_of(&[("Factory Builder/Factory Builder.exe", b"new exe"), ("Factory Builder/tools/IconExtractor.exe", b"tool")]);
        let app = unpack(&zip, &root.join("a")).unwrap();
        assert!(app.join(crate::install::EXE_NAME).is_file());
        let bad = zip_of(&[("../escape.txt", b"x")]);
        assert!(unpack(&bad, &root.join("b")).is_err(), "a path outside the folder");
        assert!(!root.join("escape.txt").exists());
        // Swapped in over an "installed" copy: the old exe set aside, the new one in.
        let dir = root.join("installed");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::install::EXE_NAME), b"old exe").unwrap();
        swap_in(&dir, &app).unwrap();
        assert_eq!(std::fs::read(dir.join(crate::install::EXE_NAME)).unwrap(), b"new exe");
        assert_eq!(std::fs::read(dir.join("tools").join("IconExtractor.exe")).unwrap(), b"tool");
        assert!(dir.join(OLD_EXE).exists());
        cleanup(&dir.join(crate::install::EXE_NAME));
        assert!(!dir.join(OLD_EXE).exists(), "removed on the next start");
        let _ = std::fs::remove_dir_all(&root);
    }
}
