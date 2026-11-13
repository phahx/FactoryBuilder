//! The Live map add-on, host side (off unless turned on in Settings):
//! following a dedicated server through the game's official HTTPS API
//! (CommunityResources\DedicatedServerAPIDocs.md).
//!
//! Security:
//! - Only the server in Settings is ever contacted, and only for the
//!   functions below; the window can't send its own requests.
//! - A dedicated server's certificate is normally self-signed, so, as the
//!   game does, the user confirms its SHA-256 fingerprint once and it's
//!   pinned: any other certificate is refused until confirmed again.
//! - The server's API token (`server.GenerateAPIToken`) is kept encrypted
//!   for this Windows user (DPAPI) in the data folder. It never goes to the
//!   window, and is only ever sent to the pinned server.
//! - Replies are size-limited and parsed as data.

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

const CONNECT: Duration = Duration::from_secs(10);
const JSON_TIMEOUT: Duration = Duration::from_secs(30);
const FILE_TIMEOUT: Duration = Duration::from_secs(600);
const JSON_LIMIT: usize = 64 << 20;
/// Late-game saves are tens of MB.
const SAVE_LIMIT: usize = 1 << 30;
const TOKEN_FILE: &str = "server-token.bin";
const MAX_TOKEN: usize = 4096;

// ------------------------------------------------------------ connection

/// A certificate fingerprint: SHA-256 as upper-case hex pairs with colons,
/// as certificate viewers show it.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// Accepts only the pinned certificate (when there is one), and notes the
/// one the server showed.
#[derive(Debug)]
struct Pinned {
    pin: Option<String>,
    seen: Arc<Mutex<Option<String>>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        let fp = fingerprint(end_entity.as_ref());
        *self.seen.lock().unwrap() = Some(fp.clone());
        match &self.pin {
            Some(p) if *p == fp => Ok(ServerCertVerified::assertion()),
            _ => Err(rustls::Error::General("certificate not trusted".into())),
        }
    }
    fn verify_tls12_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_signature(message, cert, dss, &self.provider, false)
    }
    fn verify_tls13_signature(&self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_signature(message, cert, dss, &self.provider, true)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// Checks the server's handshake signature with its certificate's key.
///
/// rustls's own helpers go through webpki, which only reads X.509 v3, and
/// the dedicated server's self-signed certificate is v1 ("unsupported
/// certificate version"). The certificate itself is trusted by its pinned
/// fingerprint, so all that's needed from it is the public key.
fn verify_signature(message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct, provider: &CryptoProvider, tls13: bool) -> Result<HandshakeSignatureValid, rustls::Error> {
    let unadvertised = || rustls::Error::PeerMisbehaved(rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme);
    // TLS 1.3 allows no PKCS#1 v1.5 or SHA-1 signatures (RFC 8446 4.2.3).
    use SignatureScheme as S;
    let tls13_scheme = matches!(dss.scheme, S::ECDSA_NISTP256_SHA256 | S::ECDSA_NISTP384_SHA384 | S::ECDSA_NISTP521_SHA512 | S::RSA_PSS_SHA256 | S::RSA_PSS_SHA384 | S::RSA_PSS_SHA512 | S::ED25519 | S::ED448);
    if tls13 && !tls13_scheme {
        return Err(unadvertised());
    }
    let algs = provider.signature_verification_algorithms.mapping.iter().find(|(s, _)| *s == dss.scheme).map(|(_, a)| *a).ok_or_else(unadvertised)?;
    let (key_alg, key) = public_key(cert.as_ref()).ok_or(rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding))?;
    let alg = algs.iter().find(|a| a.public_key_alg_id().as_ref() == key_alg).ok_or(rustls::Error::InvalidCertificate(rustls::CertificateError::BadSignature))?;
    alg.verify_signature(key, message, dss.signature())
        .map(|_| HandshakeSignatureValid::assertion())
        .map_err(|_| rustls::Error::InvalidCertificate(rustls::CertificateError::BadSignature))
}

/// One DER element: (tag, contents, what follows).
fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        (rest[..n].iter().fold(0usize, |l, &b| l << 8 | b as usize), &rest[n..])
    };
    (rest.len() >= len).then(|| (tag, &rest[..len], &rest[len..]))
}

/// A certificate's public key (any X.509 version): the algorithm
/// identifier's contents and the key bits.
fn public_key(cert: &[u8]) -> Option<(&[u8], &[u8])> {
    const SEQUENCE: u8 = 0x30;
    let (SEQUENCE, cert, _) = der(cert)? else { return None };
    let (SEQUENCE, mut tbs, _) = der(cert)? else { return None };
    if tbs.first() == Some(&0xA0) {
        tbs = der(tbs)?.2; // [0] version: absent in v1
    }
    for expected in [0x02, SEQUENCE, SEQUENCE, SEQUENCE, SEQUENCE] {
        // serial, signature algorithm, issuer, validity, subject
        let (tag, _, rest) = der(tbs)?;
        if tag != expected {
            return None;
        }
        tbs = rest;
    }
    let (SEQUENCE, spki, _) = der(tbs)? else { return None };
    let (SEQUENCE, alg, rest) = der(spki)? else { return None };
    let (0x03, bits, _) = der(rest)? else { return None };
    let (&0, key) = bits.split_first()? else { return None }; // no unused bits
    Some((alg, key))
}

/// A host name or IP address, nothing else (no scheme, path or port).
pub fn valid_host(h: &str) -> bool {
    !h.is_empty() && h.len() <= 253 && h.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
}

pub struct Reply {
    pub status: u16,
    /// A file attachment (Content-Disposition), not a JSON reply.
    pub file: bool,
    pub body: Bytes,
}

pub enum Fail {
    /// The certificate isn't the pinned one (or none is pinned yet): what it is.
    Untrusted(Option<String>),
    Other(String),
}

impl From<String> for Fail {
    fn from(s: String) -> Self {
        Fail::Other(s)
    }
}

async fn exchange<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(io: S, req: hyper::Request<Full<Bytes>>, limit: usize) -> Result<Reply, String> {
    let (mut tx, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(io)).await.map_err(|e| format!("The server didn't answer properly: {e}"))?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let resp = tx.send_request(req).await.map_err(|e| format!("The server didn't answer: {e}"))?;
    let status = resp.status().as_u16();
    let file = resp.headers().contains_key(hyper::header::CONTENT_DISPOSITION);
    let body = Limited::new(resp.into_body(), limit).collect().await.map_err(|_| "The server's answer was too big or cut off.".to_string())?.to_bytes();
    Ok(Reply { status, file, body })
}

/// One HTTPS request to `host:port`, accepting only certificate `pin`.
pub async fn https(host: &str, port: u16, pin: Option<&str>, req: hyper::Request<Full<Bytes>>, limit: usize, timeout: Duration) -> Result<Reply, Fail> {
    if !valid_host(host) {
        return Err(Fail::Other("That server address isn't valid.".into()));
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let seen = Arc::new(Mutex::new(None));
    let verifier = Arc::new(Pinned { pin: pin.map(str::to_string), seen: seen.clone(), provider: provider.clone() });
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    let name = ServerName::try_from(host.trim_matches(['[', ']']).to_string()).map_err(|_| "That server address isn't valid.".to_string())?;
    let tcp = tokio::time::timeout(CONNECT, tokio::net::TcpStream::connect((host.trim_matches(['[', ']']), port)))
        .await
        .map_err(|_| format!("Couldn't reach {host}:{port} (no answer)."))?
        .map_err(|e| format!("Couldn't reach {host}:{port}: {e}"))?;
    let tls = match tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name, tcp).await {
        Ok(t) => t,
        Err(e) => {
            let shown = seen.lock().unwrap().clone();
            return Err(match shown {
                Some(fp) if pin != Some(fp.as_str()) => Fail::Untrusted(Some(fp)),
                _ => Fail::Other(format!("Couldn't connect securely to {host}:{port}: {e}")),
            });
        }
    };
    tokio::time::timeout(timeout, exchange(tls, req, limit)).await.map_err(|_| Fail::Other("The server took too long to answer.".into()))?.map_err(Fail::Other)
}

// --------------------------------------------------------------- the API

/// The official server API: POST /api/v1 with {"function", "data"}.
fn api_request(host: &str, port: u16, token: Option<&str>, function: &str, data: Value) -> Result<hyper::Request<Full<Bytes>>, String> {
    let body = serde_json::to_vec(&json!({ "function": function, "data": data })).map_err(|e| e.to_string())?;
    let mut b = hyper::Request::post("/api/v1")
        .header(hyper::header::HOST, format!("{host}:{port}"))
        .header(hyper::header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(hyper::header::USER_AGENT, "FactoryBuilder");
    if let Some(t) = token {
        b = b.header(hyper::header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(Full::new(Bytes::from(body))).map_err(|e| e.to_string())
}

/// A function's data, or what went wrong (the docs' error response:
/// errorCode, errorMessage).
pub fn api_data(r: &Reply) -> Result<Value, String> {
    if r.status == 204 || (r.body.is_empty() && (200..300).contains(&r.status)) {
        return Ok(Value::Null);
    }
    let v: Value = serde_json::from_slice(&r.body).map_err(|_| format!("The server's answer wasn't readable (HTTP {}).", r.status))?;
    if let Some(code) = v.get("errorCode").and_then(Value::as_str) {
        let msg = v.get("errorMessage").and_then(Value::as_str).unwrap_or("");
        return Err(match (r.status, code) {
            (401 | 403, _) => "The server refused the API token. Make a new one: in Satisfactory, Server Manager › this server › Console, type server.GenerateAPIToken.".to_string(),
            _ if msg.is_empty() => format!("The server said: {code}"),
            _ => format!("The server said: {msg} ({code})"),
        });
    }
    if r.status == 401 || r.status == 403 {
        return Err("The server refused the API token. Make a new one: in Satisfactory, Server Manager › this server › Console, type server.GenerateAPIToken.".into());
    }
    if !(200..300).contains(&r.status) {
        return Err(format!("The server answered HTTP {}.", r.status));
    }
    Ok(v.get("data").cloned().unwrap_or(Value::Null))
}

pub async fn call(host: &str, port: u16, pin: Option<&str>, token: Option<&str>, function: &str, data: Value) -> Result<Value, Fail> {
    let req = api_request(host, port, token, function, data)?;
    let r = https(host, port, pin, req, JSON_LIMIT, JSON_TIMEOUT).await?;
    Ok(api_data(&r)?)
}

/// A field of a server API reply. The docs name fields `Sessions`,
/// `SaveHeaders` and so on, but the server sends them camelCase
/// (`sessions`, `saveHeaders`), so the case isn't minded.
pub fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    let o = v.as_object()?;
    o.get(key).or_else(|| o.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v))
}

/// The save the server is on: of the current session, the one played
/// longest (the newest; PlayDurationSeconds only grows).
pub fn newest_save(sessions: &Value) -> Option<(String, i64)> {
    let list = field(sessions, "Sessions")?.as_array()?;
    let current = field(sessions, "CurrentSessionIndex").and_then(Value::as_i64).filter(|i| *i >= 0).map(|i| i as usize);
    let session = current.and_then(|i| list.get(i))?;
    field(session, "SaveHeaders")?
        .as_array()?
        .iter()
        .filter_map(|h| Some((field(h, "SaveName")?.as_str()?.to_string(), field(h, "PlayDurationSeconds")?.as_i64()?)))
        .max_by_key(|(_, t)| *t)
}

/// Ficsit Remote Monitoring through the dedicated server's own API: no
/// token, `endpoint` at the top level, and the endpoint's JSON comes back
/// as is (a file reply), as FRM's FRM_Controller.cpp does it.
///
/// `None` when the server has no `frm` function: FRM registers it when its
/// server subsystem starts, and on some servers it never shows up (seen
/// with FRM 1.5.5 after loading another save), so FRM's own web server is
/// the way in then.
pub async fn frm_via_server(host: &str, port: u16, pin: &str, endpoint: &str) -> Result<Option<Value>, Fail> {
    let body = serde_json::to_vec(&json!({ "function": "frm", "endpoint": endpoint })).map_err(|e| e.to_string())?;
    let req = hyper::Request::post("/api/v1")
        .header(hyper::header::HOST, format!("{host}:{port}"))
        .header(hyper::header::CONTENT_TYPE, "application/json; charset=utf-8")
        .header(hyper::header::USER_AGENT, "FactoryBuilder")
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| e.to_string())?;
    let r = https(host, port, Some(pin), req, JSON_LIMIT, JSON_TIMEOUT).await?;
    if !r.file {
        if no_function(&r) {
            return Ok(None);
        }
        // An error (unknown endpoint, say).
        api_data(&r)?;
        return Err(Fail::Other("The server doesn't answer for Ficsit Remote Monitoring. Is the mod installed on it?".into()));
    }
    serde_json::from_slice(&r.body).map(Some).map_err(|_| Fail::Other("Ficsit Remote Monitoring's answer wasn't readable.".into()))
}

/// The server's "no such function" answer (errorCode bad_function).
fn no_function(r: &Reply) -> bool {
    serde_json::from_slice::<Value>(&r.body).ok().and_then(|v| v.get("errorCode")?.as_str().map(|c| c == "bad_function")).unwrap_or(false)
}

/// Ficsit Remote Monitoring's own web server: plain HTTP GET, reads only.
pub async fn frm_direct(host: &str, port: u16, endpoint: &str) -> Result<Value, String> {
    if !valid_host(host) {
        return Err("That address isn't valid.".into());
    }
    let req = hyper::Request::get(format!("/{endpoint}"))
        .header(hyper::header::HOST, format!("{host}:{port}"))
        .header(hyper::header::USER_AGENT, "FactoryBuilder")
        .body(Full::new(Bytes::new()))
        .map_err(|e| e.to_string())?;
    let h = host.trim_matches(['[', ']']);
    let tcp = tokio::time::timeout(CONNECT, tokio::net::TcpStream::connect((h, port)))
        .await
        .map_err(|_| format!("Ficsit Remote Monitoring didn't answer at {host}:{port}."))?
        .map_err(|_| format!("Ficsit Remote Monitoring isn't answering at {host}:{port}. Type /frm http start in the game's chat, or turn on its web server's autostart in the mod's options."))?;
    let r = tokio::time::timeout(JSON_TIMEOUT, exchange(tcp, req, JSON_LIMIT)).await.map_err(|_| "Ficsit Remote Monitoring took too long to answer.".to_string())??;
    if r.status != 200 {
        return Err(format!("Ficsit Remote Monitoring answered HTTP {}.", r.status));
    }
    serde_json::from_slice(&r.body).map_err(|_| "Ficsit Remote Monitoring's answer wasn't readable.".to_string())
}

/// Players from FRM's getPlayer: name (empty while offline), location in
/// cm (world space, as saves), and `rotation`, which FRM turns so 0 is
/// north: the game's yaw is `rotation` - 90.
pub fn players(v: &Value) -> Vec<fb_model::world::PlayerSpot> {
    let num = |o: &Value, k: &str| o.get(k).and_then(Value::as_f64).filter(|f| f.is_finite());
    v.as_array()
        .into_iter()
        .flatten()
        .take(256)
        .filter_map(|p| {
            let loc = p.get("location")?;
            let (x, y, z) = (num(loc, "x")?, num(loc, "y")?, num(loc, "z").unwrap_or(0.0));
            if x.abs() > 1e8 || y.abs() > 1e8 {
                return None;
            }
            let name = p.get("Name").and_then(Value::as_str).map(|n| n.chars().filter(|c| !c.is_control()).take(48).collect::<String>()).filter(|n| !n.trim().is_empty());
            let yaw = num(loc, "rotation").map(|r| (r - 90.0).rem_euclid(360.0)).unwrap_or(0.0);
            Some(fb_model::world::PlayerSpot { name, x: x as f32, y: y as f32, z: z as f32, yaw: yaw as f32, online: p.get("Online").and_then(Value::as_bool) })
        })
        .collect()
}

/// Machines (getFactory, getExtractor, getGenerators) as FRM 1.5.5 writes
/// them: ClassName, location {x, y, z, rotation}, RecipeClassName,
/// ManuSpeed (%), Productivity (%, factories) or production[].ProdPercent
/// (extractors), IsProducing, IsPaused.
pub fn machines(v: &Value) -> Vec<fb_model::LiveMachine> {
    let num = |o: &Value, k: &str| o.get(k).and_then(Value::as_f64).filter(|f| f.is_finite());
    let text = |o: &Value, k: &str| o.get(k).and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 200 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).map(str::to_string);
    v.as_array()
        .into_iter()
        .flatten()
        .take(200_000)
        .filter_map(|m| {
            let class = text(m, "ClassName").filter(|c| !c.starts_with(fb_model::HUB_BURNER_CLASS))?;
            let loc = m.get("location")?;
            let (x, y) = (num(loc, "x")?, num(loc, "y")?);
            if x.abs() > 1e8 || y.abs() > 1e8 {
                return None;
            }
            let productivity = num(m, "Productivity").or_else(|| m.get("production")?.as_array()?.first().and_then(|p| num(p, "ProdPercent")));
            Some(fb_model::LiveMachine {
                class,
                recipe: text(m, "RecipeClassName"),
                x: x as f32,
                y: y as f32,
                z: num(loc, "z").unwrap_or(0.0) as f32,
                yaw: num(loc, "rotation").map(|r| (r - 90.0).rem_euclid(360.0)).unwrap_or(0.0) as f32,
                clock: num(m, "ManuSpeed").map(|s| (s / 100.0) as f32),
                productivity: productivity.map(|p| p.clamp(0.0, 1000.0) as f32),
                producing: m.get("IsProducing").and_then(Value::as_bool),
                paused: m.get("IsPaused").and_then(Value::as_bool),
                id: m.get("ID").and_then(Value::as_str).filter(|s| s.len() <= 300).map(str::to_string),
                inputs: items(m.get("ingredients")),
                outputs: items(m.get("production")),
                fuel: items(m.get("FuelInventory")).into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i),
            })
        })
        .collect()
}

/// Belts (getBelts) or pipes (getPipes): ClassName, SplineData (points
/// every 75 cm by default) and location1 (the true far end), thinned to a
/// point every 2 m for the map.
pub fn carriers(v: &Value, fluid: bool) -> Vec<fb_model::LiveCarrier> {
    let xy = |p: &Value| Some([p.get("x")?.as_f64().filter(|f| f.is_finite() && f.abs() < 1e8)? as f32, p.get("y")?.as_f64().filter(|f| f.is_finite() && f.abs() < 1e8)? as f32]);
    v.as_array()
        .into_iter()
        .flatten()
        .take(200_000)
        .filter_map(|c| {
            let class = c.get("ClassName")?.as_str().filter(|s| s.len() <= 200)?.to_string();
            // Its height span, from every point given.
            let z_of = |p: &Value| p.get("z").and_then(Value::as_f64).filter(|f| f.is_finite() && f.abs() < 1e8).map(|f| f as f32);
            let zs: Vec<f32> = c.get("SplineData").and_then(Value::as_array).into_iter().flatten().take(20_000).chain(c.get("location0")).chain(c.get("location1")).filter_map(z_of).collect();
            let z = if zs.is_empty() { [0.0, 0.0] } else { [zs.iter().copied().fold(f32::MAX, f32::min), zs.iter().copied().fold(f32::MIN, f32::max)] };
            let mut pts: Vec<[f32; 2]> = Vec::new();
            for p in c.get("SplineData").and_then(Value::as_array).into_iter().flatten().take(20_000).filter_map(xy) {
                if pts.last().is_none_or(|q| (q[0] - p[0]).hypot(q[1] - p[1]) >= 200.0) {
                    pts.push(p);
                }
            }
            if pts.is_empty() {
                pts.extend(c.get("location0").and_then(xy));
            }
            if let Some(end) = c.get("location1").and_then(xy) {
                if pts.last() != Some(&end) {
                    pts.push(end);
                }
            }
            (pts.len() >= 2).then_some(fb_model::LiveCarrier { class, fluid, points: pts, z })
        })
        .collect()
}

/// Map marks FRM reports, by endpoint: the building classes (prefixes) each
/// one returns all of (from FRM 1.5.5's source: the game class it lists, and
/// the game's class tree). Valves are left out: whether getPump lists them
/// isn't known, and a family's save buildings it doesn't list would vanish.
pub const FAMILIES: &[(&str, &[&str])] = &[
    ("getPowerPoles", &["Build_PowerPole", "Build_PowerTower"]),
    ("getSwitches", &["Build_PowerSwitch", "Build_PriorityPowerSwitch"]),
    ("getStorageInv", &["Build_Storage", "Build_CentralStorage"]),
    ("getFluidBuffer", &["Build_PipeStorageTank", "Build_IndustrialTank"]),
    ("getSplitterMerger", &["Build_ConveyorAttachment"]),
    ("getPipeJunctions", &["Build_PipelineJunction"]),
    ("getPump", &["Build_PipelinePump"]),
];

/// Power Storage come from getPowerUsage (they're factory buildings).
pub const BATTERIES: &str = "Build_PowerStorage";

fn num(o: &Value, k: &str) -> Option<f64> {
    o.get(k).and_then(Value::as_f64).filter(|f| f.is_finite())
}

/// A class name as FRM writes them (Build_..._C, Desc_..._C).
fn class_name(o: &Value, k: &str) -> Option<String> {
    o.get(k).and_then(Value::as_str).filter(|s| !s.is_empty() && s.len() <= 200 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')).map(str::to_string)
}

/// ClassName, and location x, y and facing (the game's yaw).
fn placed(m: &Value) -> Option<(String, f32, f32, f32)> {
    let class = class_name(m, "ClassName")?;
    let loc = m.get("location")?;
    let (x, y) = (num(loc, "x")?, num(loc, "y")?);
    if x.abs() > 1e8 || y.abs() > 1e8 {
        return None;
    }
    let yaw = num(loc, "rotation").map(|r| (r - 90.0).rem_euclid(360.0)).unwrap_or(0.0);
    Some((class, x as f32, y as f32, yaw as f32))
}

/// A building's height (world Z, cm), when its location gives one.
fn height_of(m: &Value) -> f32 {
    m.get("location").and_then(|l| num(l, "z")).filter(|z| z.abs() < 1e8).unwrap_or(0.0) as f32
}

/// The game's circuit id (-1: none).
fn circuit_id(v: Option<f64>) -> Option<i64> {
    v.map(|c| c as i64).filter(|c| *c >= 0)
}

/// Items as FRM lists them: ClassName and Amount (fluids in m3).
fn items(v: Option<&Value>) -> Vec<(String, f32)> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(1000)
        .filter_map(|i| Some((class_name(i, "ClassName")?, num(i, "Amount")? as f32)))
        .filter(|(_, n)| *n > 0.0)
        .collect()
}

/// One family's buildings (`prefixes`) from its endpoint: poles' CircuitID,
/// switches' Primary side, storage's Inventory and slots, tanks' fluid.
pub fn structures(v: &Value, prefixes: &[&str]) -> Vec<fb_model::LiveStructure> {
    v.as_array()
        .into_iter()
        .flatten()
        .take(200_000)
        .filter_map(|m| {
            let (class, x, y, yaw) = placed(m)?;
            if !prefixes.iter().any(|p| class.starts_with(p)) {
                return None;
            }
            let circuit = circuit_id(num(m, "CircuitID").or_else(|| num(m, "Primary")));
            let slots = match (num(m, "SlotsUsed"), num(m, "Slots")) {
                (Some(u), Some(t)) if t > 0.0 => Some((u.max(0.0) as u32, t as u32)),
                _ => None,
            };
            let fluid = num(m, "Capacity").filter(|c| *c > 0.0).map(|cap| (class_name(m, "FluidClassName"), num(m, "Content").unwrap_or(0.0).max(0.0) as f32, cap as f32));
            Some(fb_model::LiveStructure { class, x, y, z: height_of(m), yaw, circuit, items: items(m.get("Inventory")), slots, fluid })
        })
        .collect()
}

/// Power lines (getCables): location0 and location1, their two ends.
pub fn lines(v: &Value) -> Vec<[[f32; 2]; 2]> {
    let xy = |p: &Value| Some([num(p, "x").filter(|f| f.abs() < 1e8)? as f32, num(p, "y").filter(|f| f.abs() < 1e8)? as f32]);
    v.as_array().into_iter().flatten().take(500_000).filter_map(|c| Some([xy(c.get("location0")?)?, xy(c.get("location1")?)?])).collect()
}

/// Circuits (getPower): one per circuit group, with the game's figures.
pub fn circuits(v: &Value) -> Vec<fb_model::LiveCircuit> {
    let f = |o: &Value, k: &str| num(o, k).unwrap_or(0.0).max(0.0) as f32;
    v.as_array()
        .into_iter()
        .flatten()
        .take(100_000)
        .map(|c| {
            let ids = c.get("AssociatedCircuits").and_then(Value::as_array).into_iter().flatten().take(10_000).filter_map(|i| circuit_id(i.as_f64())).collect();
            let battery_mwh = f(c, "BatteryCapacity");
            fb_model::LiveCircuit {
                ids,
                production: f(c, "PowerProduction"),
                consumption: f(c, "PowerConsumed"),
                capacity: f(c, "PowerCapacity"),
                max_consumption: f(c, "PowerMaxConsumed"),
                battery_mwh,
                stored_mwh: battery_mwh * (f(c, "BatteryPercent") / 100.0).clamp(0.0, 1.0),
                fuse: c.get("FuseTriggered").and_then(Value::as_bool).unwrap_or(false),
            }
        })
        .collect()
}

/// Every powered building (getPowerUsage), with what generators
/// (getGenerators, by ID) make: their capacity times the game's load, and
/// what they can make now (dynamic capacity, or what they make).
pub fn powered(usage: &Value, generators: &Value) -> Vec<fb_model::LivePowered> {
    let gens: std::collections::HashMap<&str, &Value> = generators.as_array().into_iter().flatten().filter_map(|g| Some((g.get("ID")?.as_str()?, g))).collect();
    usage
        .as_array()
        .into_iter()
        .flatten()
        .take(500_000)
        .filter_map(|m| {
            let id = m.get("ID")?.as_str().filter(|s| s.len() <= 300)?.to_string();
            let (class, x, y, _) = placed(m)?;
            let p = m.get("PowerInfo")?;
            let (makes, can_make) = match gens.get(id.as_str()) {
                Some(g) => {
                    let makes = (num(g, "ProductionCapacity").unwrap_or(0.0) * num(g, "LoadPercentage").unwrap_or(0.0) / 100.0).max(0.0) as f32;
                    (makes, (num(g, "DynamicProdCapacity").unwrap_or(0.0).max(0.0) as f32).max(makes))
                }
                None => (0.0, 0.0),
            };
            let fuel = gens.get(id.as_str()).and_then(|g| items(g.get("FuelInventory")).into_iter().max_by(|a, b| a.1.total_cmp(&b.1)));
            Some(fb_model::LivePowered {
                id,
                class,
                x,
                y,
                circuit: circuit_id(num(p, "CircuitID")),
                now: num(p, "PowerConsumed").unwrap_or(0.0).max(0.0) as f32,
                max: num(p, "MaxPowerConsumed").unwrap_or(0.0).max(0.0) as f32,
                makes,
                can_make,
                fuel,
            })
        })
        .collect()
}

pub async fn download(host: &str, port: u16, pin: &str, token: &str, save: &str) -> Result<Bytes, Fail> {
    let req = api_request(host, port, Some(token), "DownloadSaveGame", json!({ "SaveName": save }))?;
    let r = https(host, port, Some(pin), req, SAVE_LIMIT, FILE_TIMEOUT).await?;
    if !r.file {
        api_data(&r)?;
        return Err(Fail::Other("The server didn't send the save.".into()));
    }
    Ok(r.body)
}

// ----------------------------------------------------------------- token

pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join("live").join(TOKEN_FILE)
}

/// An API token as the server makes it: printable, no spaces.
pub fn valid_token(t: &str) -> bool {
    !t.is_empty() && t.len() <= MAX_TOKEN && t.chars().all(|c| c.is_ascii_graphic())
}

pub fn save_token(data_dir: &Path, token: &str) -> Result<(), String> {
    if !valid_token(token) {
        return Err("That doesn't look like an API token (paste what server.GenerateAPIToken printed).".into());
    }
    let sealed = protect(token.as_bytes())?;
    let p = token_path(data_dir);
    std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(&p, sealed).map_err(|e| format!("Couldn't save the token: {e}"))
}

pub fn load_token(data_dir: &Path) -> Option<String> {
    let sealed = std::fs::read(token_path(data_dir)).ok()?;
    String::from_utf8(unprotect(&sealed).ok()?).ok().filter(|t| valid_token(t))
}

pub fn forget_token(data_dir: &Path) {
    let _ = std::fs::remove_file(token_path(data_dir));
}

#[cfg(windows)]
fn blob_bytes(b: &windows::Win32::Security::Cryptography::CRYPT_INTEGER_BLOB) -> Vec<u8> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    // SAFETY: the API filled pbData with cbData bytes it allocated, which
    // are copied out and then freed once.
    unsafe {
        let v = std::slice::from_raw_parts(b.pbData, b.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(b.pbData as _)));
        v
    }
}

/// Encrypts for this Windows user (DPAPI).
#[cfg(windows)]
fn protect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Security::Cryptography::{CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: input points at `data` for the call; out is filled by the API.
    unsafe { CryptProtectData(&input, windows::core::w!("Factory Builder server token"), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.map_err(|e| format!("Couldn't protect the token: {e}"))?;
    Ok(blob_bytes(&out))
}

#[cfg(windows)]
fn unprotect(data: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Security::Cryptography::{CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`.
    unsafe { CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out) }.map_err(|e| e.to_string())?;
    Ok(blob_bytes(&out))
}

#[cfg(not(windows))]
fn protect(_: &[u8]) -> Result<Vec<u8>, String> {
    Err("Keeping a server token safely needs Windows.".into())
}

#[cfg(not(windows))]
fn unprotect(_: &[u8]) -> Result<Vec<u8>, String> {
    Err("unsupported".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_only_names_or_addresses() {
        for ok in ["localhost", "192.168.1.20", "my-server.example.com", "[::1]"] {
            assert!(valid_host(ok), "{ok}");
        }
        for bad in ["", "https://x", "x/y", "a b", "x?y=1", "user@host"] {
            assert!(!valid_host(bad), "{bad}");
        }
    }

    #[test]
    fn replies_are_read_as_the_docs_describe() {
        let r = |status: u16, body: &str| Reply { status, file: false, body: Bytes::from(body.to_string()) };
        assert_eq!(api_data(&r(200, r#"{"data":{"Health":"healthy"}}"#)).unwrap(), json!({ "Health": "healthy" }));
        assert_eq!(api_data(&r(204, "")).unwrap(), Value::Null);
        assert!(api_data(&r(200, r#"{"errorCode":"file_not_found"}"#)).unwrap_err().contains("file_not_found"));
        assert!(api_data(&r(401, r#"{"errorCode":"invalid_token"}"#)).unwrap_err().contains("GenerateAPIToken"));
        assert!(api_data(&r(500, "<html>")).is_err());
    }

    #[test]
    fn frm_players_are_read_as_its_source_writes_them() {
        // FRM 1.5.5 PlayerLibrary.cpp: rotation 0 = north (the game's -90).
        let v = json!([
            { "ID": "Char_Player_C_1", "Name": "Ada", "location": { "x": 1000.5, "y": -2000.0, "z": 30.0, "rotation": 90.0 }, "Online": true },
            { "ID": "Char_Player_C_2", "Name": "", "location": { "x": 5.0, "y": 6.0, "z": 0.0, "rotation": 0.0 }, "Online": false },
            { "ID": "broken" }
        ]);
        let p = players(&v);
        assert_eq!(p.len(), 2);
        assert_eq!((p[0].name.as_deref(), p[0].x, p[0].yaw, p[0].online), (Some("Ada"), 1000.5, 0.0, Some(true)), "facing east is the game's yaw 0");
        assert_eq!((p[1].name.as_deref(), p[1].yaw, p[1].online), (None, 270.0, Some(false)), "north is the game's -90");
    }

    #[test]
    fn frm_buildings_are_read_as_its_source_writes_them() {
        let v = json!([
            { "ClassName": "Build_ConstructorMk1_C", "RecipeClassName": "Recipe_IronPlate_C", "location": { "x": 100.0, "y": 200.0, "z": 0.0, "rotation": 180.0 }, "ManuSpeed": 150.0, "Productivity": 87.5, "IsProducing": true, "IsPaused": false },
            { "ClassName": "Build_MinerMk1_C", "location": { "x": 1.0, "y": 2.0, "z": 3.0, "rotation": 90.0 }, "production": [{ "ProdPercent": 50.0 }], "IsProducing": true },
            { "ClassName": "../evil", "location": { "x": 0.0, "y": 0.0 } }
        ]);
        let m = machines(&v);
        assert_eq!(m.len(), 2, "names are only class names");
        assert_eq!((m[0].recipe.as_deref(), m[0].yaw, m[0].clock, m[0].productivity), (Some("Recipe_IronPlate_C"), 90.0, Some(1.5), Some(87.5)));
        assert_eq!((m[1].productivity, m[1].yaw), (Some(50.0), 0.0));
        let b = json!([{ "ClassName": "Build_ConveyorBeltMk1_C", "SplineData": [{ "x": 0.0, "y": 0.0 }, { "x": 75.0, "y": 0.0 }, { "x": 150.0, "y": 0.0 }, { "x": 225.0, "y": 0.0 }, { "x": 300.0, "y": 0.0 }], "location0": { "x": 0.0, "y": 0.0 }, "location1": { "x": 320.0, "y": 0.0 } }]);
        let c = carriers(&b, false);
        assert_eq!(c[0].points, vec![[0.0, 0.0], [225.0, 0.0], [320.0, 0.0]], "a point every 2 m, and the true end");
    }

    #[test]
    fn a_server_without_frms_function_is_told_apart() {
        let r = |status: u16, body: &str| Reply { status, file: false, body: Bytes::from(body.to_string()) };
        assert!(no_function(&r(404, r#"{"errorCode":"bad_function","errorMessage":"Payload JSON function 'frm' not found"}"#)));
        assert!(!no_function(&r(404, r#"{"errorCode":"endpoint_not_found"}"#)));
        assert!(!no_function(&r(500, "<html>")));
    }

    #[test]
    fn the_newest_save_of_the_current_session() {
        let s = json!({
            "CurrentSessionIndex": 1,
            "Sessions": [
                { "SessionName": "Old", "SaveHeaders": [{ "SaveName": "old_autosave_0", "PlayDurationSeconds": 99999 }] },
                { "SessionName": "Base", "SaveHeaders": [
                    { "SaveName": "Base_autosave_1", "PlayDurationSeconds": 7200 },
                    { "SaveName": "Base_autosave_2", "PlayDurationSeconds": 7500 },
                    { "SaveName": "Base_autosave_0", "PlayDurationSeconds": 6900 }
                ] }
            ]
        });
        assert_eq!(newest_save(&s), Some(("Base_autosave_2".to_string(), 7500)));
        // As the server really sends it: camelCase.
        let camel = json!({ "currentSessionIndex": 0, "sessions": [{ "sessionName": "test", "saveHeaders": [
            { "saveName": "test_autosave_1", "playDurationSeconds": 100 },
            { "saveName": "MAP_TEST", "playDurationSeconds": 160 }
        ] }] });
        assert_eq!(newest_save(&camel), Some(("MAP_TEST".to_string(), 160)));
        assert_eq!(newest_save(&json!({ "CurrentSessionIndex": -1, "Sessions": [] })), None);
    }

    #[cfg(windows)]
    #[test]
    fn tokens_are_kept_encrypted() {
        let dir = std::env::temp_dir().join(format!("fb-live-test-{}", std::process::id()));
        let token = "ewoJInBsIjogIkFQSVRva2VuIgp9.ABCDEF0123";
        save_token(&dir, token).unwrap();
        let raw = std::fs::read(token_path(&dir)).unwrap();
        assert!(!raw.windows(token.len()).any(|w| w == token.as_bytes()), "not stored as plain text");
        assert_eq!(load_token(&dir).as_deref(), Some(token));
        assert!(save_token(&dir, "not a token").is_err());
        forget_token(&dir);
        assert_eq!(load_token(&dir), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A local TLS server with a self-signed certificate: unknown, it's
    /// refused and its fingerprint reported; pinned, it's used.
    #[tokio::test]
    async fn only_the_pinned_certificate_is_accepted() {
        let Ok(cert) = test_cert("test-cert.der", "test-key.der") else { return };
        pinned_server(cert, rustls::DEFAULT_VERSIONS).await;
    }

    /// The dedicated server's own certificate is X.509 v1 (no extensions),
    /// which webpki refuses ("unsupported certificate version").
    #[tokio::test]
    async fn version_1_certificates_like_the_dedicated_servers_work() {
        let Ok(cert) = test_cert("test-cert-v1.der", "test-key-v1.der") else { return };
        assert_eq!(cert.0[..].windows(3).position(|w| w == [0xA0, 0x03, 0x02]), None, "the test certificate is v1");
        pinned_server(cert.clone(), &[&rustls::version::TLS13]).await;
        pinned_server(cert, &[&rustls::version::TLS12]).await;
    }

    #[test]
    fn public_keys_are_read_from_any_certificate_version() {
        for (file, len) in [("test-cert.der", 65), ("test-cert-v1.der", 270)] {
            let Ok((der, _)) = test_cert(file, "test-key.der") else { return };
            let (_, key) = public_key(&der).unwrap_or_else(|| panic!("{file}"));
            assert_eq!(key.len(), len, "{file}");
        }
        assert_eq!(public_key(&[0x30, 0x05, 0x30]), None, "cut off");
    }

    async fn pinned_server((cert_der, key_der): (Vec<u8>, Vec<u8>), versions: &[&'static rustls::SupportedProtocolVersion]) {
        let fp = fingerprint(&cert_der);
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(versions)
            .unwrap()
            .with_no_client_auth()
            // Not with_single_cert: its key check goes through webpki, which
            // refuses v1 certificates just like the client side did.
            .with_cert_resolver(Arc::new(rustls::sign::SingleCertAndKey::from(rustls::sign::CertifiedKey::new(
                vec![CertificateDer::from(cert_der.clone())],
                rustls::crypto::ring::sign::any_supported_type(&rustls::pki_types::PrivateKeyDer::try_from(key_der).unwrap()).unwrap(),
            ))));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else { return };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let Ok(mut tls) = acceptor.accept(tcp).await else { return };
                    let mut buf = [0u8; 4096];
                    let _ = tls.read(&mut buf).await;
                    let body = r#"{"data":{"Health":"healthy"}}"#;
                    let _ = tls.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        match call("127.0.0.1", port, None, None, "HealthCheck", json!({ "ClientCustomData": "" })).await {
            Err(Fail::Untrusted(Some(seen))) => assert_eq!(seen, fp),
            _ => panic!("an unknown certificate must be refused"),
        }
        match call("127.0.0.1", port, Some("00:11"), None, "HealthCheck", json!({})).await {
            Err(Fail::Untrusted(Some(seen))) => assert_eq!(seen, fp, "a different pin is refused too"),
            _ => panic!("a changed certificate must be refused"),
        }
        let ok = call("127.0.0.1", port, Some(&fp), None, "HealthCheck", json!({ "ClientCustomData": "" })).await;
        assert!(matches!(ok, Ok(v) if v == json!({ "Health": "healthy" })));
    }

    /// A self-signed certificate and key for 127.0.0.1, for tests only
    /// (tests/README.md).
    fn test_cert(cert: &str, key: &str) -> Result<(Vec<u8>, Vec<u8>), ()> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let cert = std::fs::read(dir.join(cert)).map_err(|_| ())?;
        let key = std::fs::read(dir.join(key)).map_err(|_| ())?;
        Ok((cert, key))
    }
}
