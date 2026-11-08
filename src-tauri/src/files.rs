//! Files the user saves, opens, imports and exports.
//!
//! The window can only read or write files the user chose: picked in a
//! native Open/Save dialog this session, opened before (the recent list), or
//! passed on the command line (double-clicking a .fbfactory). Each operation
//! also checks the extension, so the webview can't be used to touch other
//! files. Writes go to a temp file first and are then renamed into place.

use base64::Engine;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_dialog::DialogExt;

/// Biggest file the window may read (a large factory is well under 1 MB).
const MAX_READ: u64 = 32 * 1024 * 1024;

#[derive(Default)]
pub struct Allowed(Mutex<HashSet<PathBuf>>);

impl Allowed {
    pub fn allow(&self, p: &Path) {
        self.0.lock().unwrap().insert(normal(p));
    }
    fn has(&self, p: &Path) -> bool {
        self.0.lock().unwrap().contains(&normal(p))
    }
}

fn normal(p: &Path) -> PathBuf {
    PathBuf::from(p.to_string_lossy().to_lowercase())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Factory,
    Png,
    Json,
    Txt,
}

impl Kind {
    fn ext(self) -> &'static str {
        match self {
            Kind::Factory => "fbfactory",
            Kind::Png => "png",
            Kind::Json => "json",
            Kind::Txt => "txt",
        }
    }
    fn filter(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Kind::Factory => ("Factory Builder factory", &["fbfactory"]),
            Kind::Png => ("PNG image", &["png"]),
            Kind::Json => ("Factory JSON", &["json"]),
            Kind::Txt => ("Text", &["txt"]),
        }
    }
}

fn has_ext(p: &Path, allowed: &[&str]) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| allowed.iter().any(|a| e.eq_ignore_ascii_case(a)))
}

/// `Documents\Factory Builder`, created on first use.
pub fn default_dir<R: Runtime>(app: &AppHandle<R>) -> Option<PathBuf> {
    let dir = app.path().document_dir().ok()?.join("Factory Builder");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// A file passed on the command line (double-clicking a .fbfactory).
pub fn startup_file_from_args() -> Option<PathBuf> {
    std::env::args_os().skip(1).map(PathBuf::from).find(|p| has_ext(p, &["fbfactory", "json"]) && p.is_file())
}

fn chosen(fp: Option<tauri_plugin_dialog::FilePath>) -> Option<PathBuf> {
    fp.and_then(|f| f.as_path().map(Path::to_path_buf))
}

/// Native Open dialog for factories (.fbfactory, or exported factory JSON).
pub fn pick_open<R: Runtime>(app: &AppHandle<R>, allowed: &Allowed) -> Option<String> {
    let mut d = app.dialog().file().set_title("Open factory").add_filter("Factories", &["fbfactory", "json"]);
    if let Some(dir) = default_dir(app) {
        d = d.set_directory(dir);
    }
    let path = chosen(d.blocking_pick_file())?;
    allowed.allow(&path);
    Some(path.to_string_lossy().into_owned())
}

/// Native Save dialog; the chosen path always gets the kind's extension.
pub fn pick_save<R: Runtime>(app: &AppHandle<R>, allowed: &Allowed, kind: Kind, suggested: &str) -> Option<String> {
    let (name, exts) = kind.filter();
    let stem: String = suggested.chars().filter(|c| !matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    let stem = if stem.trim().is_empty() { "Factory".to_string() } else { stem.trim().to_string() };
    let mut d = app
        .dialog()
        .file()
        .set_title(match kind {
            Kind::Factory => "Save factory",
            Kind::Png => "Export image",
            Kind::Json => "Export factory JSON",
            Kind::Txt => "Save text summary",
        })
        .add_filter(name, exts)
        .set_file_name(format!("{stem}.{}", kind.ext()));
    if let Some(dir) = default_dir(app) {
        d = d.set_directory(dir);
    }
    let mut path = chosen(d.blocking_save_file())?;
    if !has_ext(&path, exts) {
        path.set_extension(kind.ext());
    }
    allowed.allow(&path);
    Some(path.to_string_lossy().into_owned())
}

pub fn read_factory(allowed: &Allowed, recent: &[String], path: &str) -> Result<String, String> {
    let p = PathBuf::from(path);
    let known = allowed.has(&p) || recent.iter().any(|r| normal(Path::new(r)) == normal(&p));
    if !known || !has_ext(&p, &["fbfactory", "json"]) {
        return Err("That file wasn't chosen in Factory Builder.".into());
    }
    let meta = std::fs::metadata(&p).map_err(|e| format!("Couldn't open {}: {e}", p.display()))?;
    if meta.len() > MAX_READ {
        return Err("That file is too big to be a factory.".into());
    }
    std::fs::read_to_string(&p).map_err(|e| format!("Couldn't read {}: {e}", p.display()))
}

/// Writes text or base64-encoded bytes to a file the user chose.
pub fn write(allowed: &Allowed, path: &str, kind: Kind, text: Option<String>, base64: Option<String>) -> Result<(), String> {
    let p = PathBuf::from(path);
    if !allowed.has(&p) {
        return Err("That file wasn't chosen in Factory Builder.".into());
    }
    if !has_ext(&p, &[kind.ext()]) {
        return Err(format!("Expected a .{} file.", kind.ext()));
    }
    let bytes = match (text, base64) {
        (Some(t), None) => t.into_bytes(),
        (None, Some(b)) => base64::engine::general_purpose::STANDARD.decode(b).map_err(|e| e.to_string())?,
        _ => return Err("Nothing to write.".into()),
    };
    let tmp = p.with_extension(format!("{}.tmp", kind.ext()));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("Couldn't write {}: {e}", p.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("Couldn't save {}: {e}", p.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("fb-files-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_chosen_files_can_be_written_or_read() {
        let dir = tmpdir();
        let allowed = Allowed::default();
        let f = dir.join("a.fbfactory");
        let p = f.to_string_lossy().to_string();
        assert!(write(&allowed, &p, Kind::Factory, Some("{}".into()), None).is_err(), "not chosen yet");
        allowed.allow(&f);
        write(&allowed, &p, Kind::Factory, Some("{\"x\":1}".into()), None).unwrap();
        assert_eq!(read_factory(&allowed, &[], &p).unwrap(), "{\"x\":1}");
        // A chosen path with the wrong kind is refused.
        assert!(write(&allowed, &p, Kind::Png, None, Some("AAAA".into())).is_err());
        // Recent files can be read again in a later session.
        let fresh = Allowed::default();
        assert!(read_factory(&fresh, &[], &p).is_err());
        assert!(read_factory(&fresh, &[p.to_uppercase()], &p).is_ok(), "recent paths match case-insensitively");
        // Other extensions are never readable, even if somehow allowed.
        let other = dir.join("secret.txt");
        std::fs::write(&other, "x").unwrap();
        allowed.allow(&other);
        assert!(read_factory(&allowed, &[], &other.to_string_lossy()).is_err());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn binary_writes_decode_base64() {
        let dir = tmpdir().join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        let allowed = Allowed::default();
        let f = dir.join("pic.png");
        allowed.allow(&f);
        write(&allowed, &f.to_string_lossy(), Kind::Png, None, Some(base64::engine::general_purpose::STANDARD.encode([137u8, 80, 78, 71]))).unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), vec![137, 80, 78, 71]);
        assert!(!f.with_extension("png.tmp").exists(), "temp file cleaned up");
        std::fs::remove_dir_all(dir).ok();
    }
}
