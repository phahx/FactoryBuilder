//! Per-user install, like Discord or VS Code: no admin rights needed.
//!
//! First launch copies the app (and its `tools` folder) to
//! `%LOCALAPPDATA%\Programs\Factory Builder` or a folder the user picks, adds
//! Start menu and desktop shortcuts, an Apps & features entry with a working
//! uninstaller, and the `.fbfactory` double-click association, then restarts
//! from there. A copy started from anywhere else hands off to the installed
//! one. Settings, the icon cache and factories live elsewhere (the profile
//! folder and Documents), so uninstalling never touches the user's work.
//!
//! Development builds can use a throwaway test profile; it installs as
//! "Factory Builder (Test)" with its own folder, shortcuts and Apps &
//! features entry, and skips the file association so it can't take over the
//! real install's.

use fb_model::setup::{version_cmp, InstallInfo, InstallReport, InstalledCopy};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PRODUCT: &str = "Factory Builder";
pub const EXE_NAME: &str = "Factory Builder.exe";
/// The icon helper and its decoder, in `<install>\tools`.
pub const TOOLS: [&str; 2] = ["IconExtractor.exe", "oodle_shim.dll"];
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const MARKER: &str = "install.json";
const PROG_ID: &str = "FactoryBuilder.Factory";
/// File types that open here: extension, ProgID, type name in Explorer.
const FILE_TYPES: [(&str, &str, &str); 2] = [
    (".fbfactory", PROG_ID, "Factory Builder factory"),
    (".fbproject", "FactoryBuilder.Project", "Factory Builder project"),
];
const PUBLISHER: &str = "Factory Builder";
const DESCRIPTION: &str = "Plan Satisfactory factories";
/// Windows: start a console program without flashing a console window.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where an install puts things. Real installs use the user's folders and
/// HKCU\Software; tests point it all at a temp folder and a scratch key.
#[derive(Clone, Debug)]
pub struct Places {
    /// %LOCALAPPDATA%\Programs
    pub programs: PathBuf,
    /// The Start menu's Programs folder.
    pub start_menu: PathBuf,
    pub desktop: PathBuf,
    /// Registry path under HKEY_CURRENT_USER that stands for "Software".
    pub reg_software: String,
}

#[derive(Clone, Debug)]
pub struct Identity {
    pub product: String,
    /// Test profile folder: every launch of a test install gets it.
    pub test_profile: Option<PathBuf>,
    pub places: Places,
}

/// Written into the install folder; lists the shortcuts to remove later.
#[derive(Serialize, Deserialize, Default)]
struct Marker {
    product: String,
    version: String,
    shortcuts: Vec<PathBuf>,
    association: bool,
}

impl Identity {
    pub fn new(test_profile: Option<PathBuf>, places: Places) -> Self {
        let product = if test_profile.is_some() { format!("{PRODUCT} (Test)") } else { PRODUCT.to_string() };
        Self { product, test_profile, places }
    }

    pub fn is_test(&self) -> bool {
        self.test_profile.is_some()
    }

    fn uninstall_key(&self) -> String {
        format!(r"{}\Microsoft\Windows\CurrentVersion\Uninstall\{}", self.places.reg_software, self.product)
    }

    fn classes(&self) -> String {
        format!(r"{}\Classes", self.places.reg_software)
    }

    pub fn default_dir(&self) -> PathBuf {
        self.places.programs.join(&self.product)
    }

    /// Picking a parent folder installs into `<it>\Factory Builder`, so an
    /// install never spreads its files into a folder that has other things.
    pub fn resolve_dir(&self, chosen: &Path) -> PathBuf {
        if chosen.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&self.product)) {
            chosen.to_path_buf()
        } else {
            chosen.join(&self.product)
        }
    }

    /// Extra arguments every launch of this identity's installed copy gets.
    pub fn launch_args(&self) -> Vec<String> {
        match &self.test_profile {
            Some(p) => vec!["--test-profile".into(), p.to_string_lossy().into_owned()],
            None => Vec::new(),
        }
    }

    /// The same, as text for a shortcut or registry command line.
    fn launch_args_text(&self) -> String {
        self.launch_args().iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")
    }

    /// The installed copy, from its Apps & features entry, if its exe exists.
    pub fn installed(&self) -> Option<InstalledCopy> {
        let key = windows_registry::CURRENT_USER.open(self.uninstall_key()).ok()?;
        let dir = key.get_string("InstallLocation").ok()?;
        let version = key.get_string("DisplayVersion").unwrap_or_default();
        Path::new(&dir).join(EXE_NAME).is_file().then_some(InstalledCopy { dir, version })
    }

    /// `exe` is the installed copy's exe.
    pub fn is_installed_exe(&self, exe: &Path) -> bool {
        self.installed().is_some_and(|c| same_file(exe, &Path::new(&c.dir).join(EXE_NAME)))
    }

    pub fn info(&self, dev: bool) -> InstallInfo {
        let exe = std::env::current_exe().unwrap_or_default();
        InstallInfo {
            product: self.product.clone(),
            version: VERSION.into(),
            installed_copy: self.is_installed_exe(&exe),
            installed: self.installed(),
            default_dir: self.default_dir().to_string_lossy().into_owned(),
            helper_found: helper_paths(&exe).is_some(),
            dev,
            test_profile: self.test_profile.as_ref().map(|p| p.to_string_lossy().into_owned()),
        }
    }

    /// Installs (or updates) into `dir` from the app at `source_exe` and the
    /// `tools` folder beside it.
    pub fn install(&self, dir: &Path, source_exe: &Path, desktop: bool) -> Result<InstallReport, String> {
        if !dir.is_absolute() || dir.parent().is_none() {
            return Err("Pick a folder (not a whole drive).".into());
        }
        if dir.exists() {
            if !dir.is_dir() {
                return Err(format!("{} is a file, not a folder.", dir.display()));
            }
            let foreign = std::fs::read_dir(dir)
                .map_err(|e| format!("Can't open {}: {e}", dir.display()))?
                .filter_map(Result::ok)
                .any(|e| !is_ours(&e.file_name().to_string_lossy()));
            if foreign {
                return Err(format!("{} already has other files in it. Pick an empty folder, or a folder to put “{}” in.", dir.display(), self.product));
            }
        }
        let mut report = InstallReport { dir: dir.to_string_lossy().into_owned(), ..Default::default() };
        let tools = dir.join("tools");
        std::fs::create_dir_all(&tools).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;

        let exe = dir.join(EXE_NAME);
        copy_file(source_exe, &exe).map_err(|e| format!("Couldn't copy the app: {e}"))?;
        let source_tools = source_exe.parent().map(|d| d.join("tools")).unwrap_or_default();
        let mut tools_ok = true;
        for t in TOOLS {
            let src = source_tools.join(t);
            if src.is_file() {
                copy_file(&src, &tools.join(t)).map_err(|e| format!("Couldn't copy tools\\{t}: {e}"))?;
            } else if !tools.join(t).is_file() {
                tools_ok = false;
            }
        }
        report.done.push(format!("Copied {} to {}", self.product, dir.display()));
        if !tools_ok {
            report.problems.push("The icon helper wasn't next to this copy of the app, so icons can't be extracted.".into());
        }

        // Apps & features entry and uninstaller.
        let size_kb = folder_size(dir) / 1024;
        let uninstall = format!("{} --uninstall {}", quote(&exe.to_string_lossy()), self.launch_args_text());
        let reg = (|| -> windows_registry::Result<()> {
            let k = windows_registry::CURRENT_USER.create(self.uninstall_key())?;
            k.set_string("DisplayName", &self.product)?;
            k.set_string("DisplayVersion", VERSION)?;
            k.set_string("Publisher", PUBLISHER)?;
            k.set_string("DisplayIcon", format!("{},0", exe.to_string_lossy()))?;
            k.set_string("InstallLocation", dir.to_string_lossy())?;
            k.set_string("UninstallString", uninstall.trim())?;
            k.set_string("QuietUninstallString", format!("{} --quiet", uninstall.trim()))?;
            k.set_u32("EstimatedSize", size_kb.min(u32::MAX as u64) as u32)?;
            k.set_u32("NoModify", 1)?;
            k.set_u32("NoRepair", 1)?;
            Ok(())
        })();
        reg.map_err(|e| format!("Couldn't add {} to Apps & features: {e}", self.product))?;
        report.done.push("Added to Apps & features, with an uninstaller".into());

        // Shortcuts.
        let mut marker = Marker { product: self.product.clone(), version: VERSION.into(), ..Default::default() };
        let mut places = vec![(self.places.start_menu.clone(), "Start menu")];
        if desktop {
            places.push((self.places.desktop.clone(), "desktop"));
        }
        for (folder, name) in places {
            let lnk = folder.join(format!("{}.lnk", self.product));
            match shortcut::create(&lnk, &exe, &self.launch_args_text(), dir, DESCRIPTION) {
                Ok(()) => {
                    report.done.push(format!("Added a {name} shortcut"));
                    marker.shortcuts.push(lnk);
                }
                Err(e) => report.problems.push(format!("Couldn't add the {name} shortcut: {e}")),
            }
        }
        if !desktop {
            // Unticked on an update: remove the old one.
            let old = self.places.desktop.join(format!("{}.lnk", self.product));
            let _ = std::fs::remove_file(old);
        }

        // Double-clicking a .fbfactory opens it here (not for test installs).
        if !self.is_test() {
            match self.associate(&exe) {
                Ok(()) => {
                    marker.association = true;
                    report.done.push("Set .fbfactory and .fbproject files to open in Factory Builder".into());
                }
                Err(e) => report.problems.push(format!("Couldn't set .fbfactory and .fbproject files to open here: {e}")),
            }
        }

        let json = serde_json::to_string_pretty(&marker).unwrap_or_default();
        std::fs::write(dir.join(MARKER), json).map_err(|e| format!("Couldn't finish the install: {e}"))?;
        self.notify_shell();
        Ok(report)
    }

    fn associate(&self, exe: &Path) -> windows_registry::Result<()> {
        let classes = self.classes();
        let user = windows_registry::CURRENT_USER;
        let exe = exe.to_string_lossy();
        for (ext, prog_id, type_name) in FILE_TYPES {
            user.create(format!(r"{classes}\{ext}"))?.set_string("", prog_id)?;
            user.create(format!(r"{classes}\{ext}\OpenWithProgids"))?.set_string(prog_id, "")?;
            user.create(format!(r"{classes}\{prog_id}"))?.set_string("", type_name)?;
            user.create(format!(r"{classes}\{prog_id}\DefaultIcon"))?.set_string("", format!("{},0", quote(&exe)))?;
            user.create(format!(r#"{classes}\{prog_id}\shell\open\command"#))?.set_string("", format!(r#"{} "%1""#, quote(&exe)))?;
        }
        Ok(())
    }

    fn notify_shell(&self) {
        // Only for the real registry: tests write to a scratch key.
        if self.places.reg_software == "Software" {
            shortcut::assoc_changed();
        }
    }

    /// Removes the installed copy: shortcuts, Apps & features entry, file
    /// association and the app's own files. Only files the installer puts
    /// there are deleted; the folder goes only if nothing else is left.
    /// Returns the exe if it is the running one (it can't delete itself;
    /// see [`delete_after_exit`]).
    pub fn uninstall(&self) -> Result<Option<PathBuf>, String> {
        let user = windows_registry::CURRENT_USER;
        let dir = user
            .open(self.uninstall_key())
            .and_then(|k| k.get_string("InstallLocation"))
            .map(PathBuf::from)
            .map_err(|_| format!("{} isn't installed.", self.product))?;
        let marker: Marker = std::fs::read_to_string(dir.join(MARKER))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        // Shortcuts: the ones the install made, plus the usual places. Only
        // "<product>.lnk" files, whatever the marker says.
        let lnk_name = format!("{}.lnk", self.product);
        let mut links = marker.shortcuts.clone();
        links.push(self.places.start_menu.join(&lnk_name));
        links.push(self.places.desktop.join(&lnk_name));
        for l in links {
            if l.file_name().is_some_and(|n| n.to_string_lossy().eq_ignore_ascii_case(&lnk_name)) {
                let _ = std::fs::remove_file(&l);
            }
        }

        // The association, only if it is still ours.
        let classes = self.classes();
        let exe = dir.join(EXE_NAME);
        if !self.is_test() {
            let ours = user
                .open(format!(r#"{classes}\{PROG_ID}\shell\open\command"#))
                .and_then(|k| k.get_string(""))
                .is_ok_and(|cmd| cmd.to_lowercase().contains(&exe.to_string_lossy().to_lowercase()));
            if ours {
                for (ext, prog_id, _) in FILE_TYPES {
                    let _ = user.remove_tree(format!(r"{classes}\{prog_id}"));
                    let _ = user.remove_tree(format!(r"{classes}\{ext}\OpenWithProgids"));
                    let points_here = user.open(format!(r"{classes}\{ext}")).and_then(|k| k.get_string("")).is_ok_and(|v| v == prog_id);
                    if points_here {
                        let _ = user.remove_tree(format!(r"{classes}\{ext}"));
                    }
                }
            }
        }
        user.remove_tree(self.uninstall_key()).map_err(|e| format!("Couldn't remove the Apps & features entry: {e}"))?;

        // Files: only ours, and only from a folder that looks like an install.
        let mut running = None;
        if dir.join(MARKER).is_file() || exe.is_file() {
            for t in TOOLS {
                let _ = std::fs::remove_file(dir.join("tools").join(t));
            }
            let _ = std::fs::remove_dir(dir.join("tools"));
            let _ = std::fs::remove_file(dir.join(format!("{EXE_NAME}.old")));
            let _ = std::fs::remove_file(dir.join(format!("{EXE_NAME}.new")));
            let me = std::env::current_exe().unwrap_or_default();
            if exe.is_file() && same_file(&exe, &me) {
                running = Some(exe.clone());
            } else if exe.is_file() {
                std::fs::remove_file(&exe).map_err(|e| format!("Couldn't delete {} (is it still open?): {e}", exe.display()))?;
            }
            let _ = std::fs::remove_file(dir.join(MARKER));
            if running.is_none() {
                let _ = std::fs::remove_dir(&dir);
            }
        }
        self.notify_shell();
        Ok(running)
    }

    /// A copy started from somewhere else hands off to the installed copy
    /// (unless that one is older: then this copy offers to update it).
    /// Returns true when it did, and this process should exit.
    pub fn hand_off(&self, args: &[String]) -> bool {
        let Some(installed) = self.installed() else { return false };
        let me = std::env::current_exe().unwrap_or_default();
        let target = Path::new(&installed.dir).join(EXE_NAME);
        if same_file(&me, &target) || version_cmp(&installed.version, VERSION).is_lt() {
            return false;
        }
        std::process::Command::new(&target).args(args).current_dir(&installed.dir).spawn().is_ok()
    }

    /// Starts the installed copy (after installing), with this identity's
    /// launch arguments.
    pub fn start_installed(&self) -> Result<(), String> {
        let installed = self.installed().ok_or_else(|| format!("{} isn't installed.", self.product))?;
        let exe = Path::new(&installed.dir).join(EXE_NAME);
        std::process::Command::new(&exe)
            .args(self.launch_args())
            .current_dir(&installed.dir)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Couldn't start {}: {e}", exe.display()))
    }
}

/// Things the installer itself puts in an install folder.
fn is_ours(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [EXE_NAME, "tools", MARKER].iter().any(|o| n == o.to_ascii_lowercase())
        || n == format!("{EXE_NAME}.old").to_ascii_lowercase()
        || n == format!("{EXE_NAME}.new").to_ascii_lowercase()
}

/// The icon helper and its decoder next to `exe`, if both are there.
pub fn helper_paths(exe: &Path) -> Option<(PathBuf, PathBuf)> {
    let tools = exe.parent()?.join("tools");
    let (helper, shim) = (tools.join(TOOLS[0]), tools.join(TOOLS[1]));
    (helper.is_file() && shim.is_file()).then_some((helper, shim))
}

pub fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Copies through a temp file. A running exe can't be overwritten but can be
/// renamed, so an in-use target is moved aside to `<name>.old` first.
fn copy_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    if same_file(src, dst) {
        return Ok(());
    }
    let name = dst.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dst.with_file_name(format!("{name}.new"));
    std::fs::copy(src, &tmp)?;
    if dst.exists() && std::fs::remove_file(dst).is_err() {
        let old = dst.with_file_name(format!("{name}.old"));
        let _ = std::fs::remove_file(&old);
        std::fs::rename(dst, &old)?;
    }
    std::fs::rename(&tmp, dst)
}

fn folder_size(dir: &Path) -> u64 {
    walk(dir).iter().filter_map(|p| p.metadata().ok()).map(|m| m.len()).sum()
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.filter_map(Result::ok) {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// Quotes a path or argument for a Windows command line.
fn quote(s: &str) -> String {
    format!("\"{s}\"")
}

/// Deletes the running exe and its (then empty) folder a moment after this
/// process exits, the usual way for an uninstaller to remove itself.
pub fn delete_after_exit(exe: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let dir = exe.parent().ok_or("no folder")?;
    let (e, d) = (exe.to_string_lossy(), dir.to_string_lossy());
    // These go to cmd.exe: refuse anything it would treat specially.
    if [&e, &d].iter().any(|s| s.contains(['"', '%', '^', '!', '\r', '\n'])) {
        return Err("The install folder's name has characters the uninstaller can't handle; delete it by hand.".into());
    }
    std::process::Command::new("cmd.exe")
        .raw_arg(format!(r#"/d /c ping -n 3 127.0.0.1 >nul & del /f /q "{e}" & rd "{d}""#))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The real user's folders.
pub fn user_places() -> Places {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from).unwrap_or_default();
    let local = shortcut::known_folder(shortcut::Folder::LocalAppData).unwrap_or_else(|| env("LOCALAPPDATA"));
    Places {
        programs: local.join("Programs"),
        start_menu: shortcut::known_folder(shortcut::Folder::StartMenuPrograms)
            .unwrap_or_else(|| env("APPDATA").join(r"Microsoft\Windows\Start Menu\Programs")),
        desktop: shortcut::known_folder(shortcut::Folder::Desktop).unwrap_or_else(|| env("USERPROFILE").join("Desktop")),
        reg_software: "Software".into(),
    }
}

/// Uninstall started from Apps & features (`--uninstall`): asks first unless
/// `--quiet`, removes the install, then deletes the running exe after exit.
pub fn run_uninstaller(identity: &Identity, quiet: bool) {
    let title = format!("Uninstall {}", identity.product);
    if !quiet
        && !shortcut::ask(
            &title,
            &format!("Remove {} from this PC?\n\nYour factories and settings are kept.", identity.product),
        )
    {
        return;
    }
    match identity.uninstall() {
        Ok(running) => {
            if !quiet {
                shortcut::tell(&title, &format!("{} was removed.", identity.product), false);
            }
            if let Some(exe) = running {
                if let Err(e) = delete_after_exit(&exe) {
                    if !quiet {
                        shortcut::tell(&title, &e, true);
                    }
                }
            }
        }
        Err(e) => {
            if !quiet {
                shortcut::tell(&title, &e, true);
            }
        }
    }
}

/// Shell bits: shortcuts, known folders, message boxes.
mod shortcut {
    use std::path::{Path, PathBuf};
    use windows::core::{Interface, HSTRING};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, IPersistFile, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::UI::Shell::{
        IShellLinkW, SHChangeNotify, SHGetKnownFolderPath, ShellLink, FOLDERID_Desktop, FOLDERID_LocalAppData,
        FOLDERID_Programs, KF_FLAG_DEFAULT, SHCNE_ASSOCCHANGED, SHCNF_IDLIST,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO,
    };

    pub enum Folder {
        LocalAppData,
        StartMenuPrograms,
        Desktop,
    }

    pub fn known_folder(f: Folder) -> Option<PathBuf> {
        let id = match f {
            Folder::LocalAppData => FOLDERID_LocalAppData,
            Folder::StartMenuPrograms => FOLDERID_Programs,
            Folder::Desktop => FOLDERID_Desktop,
        };
        unsafe {
            let p = SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT, None).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s.map(PathBuf::from)
        }
    }

    pub fn create(lnk: &Path, target: &Path, args: &str, workdir: &Path, description: &str) -> Result<(), String> {
        if let Some(dir) = lnk.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        unsafe {
            // Fine if COM is already set up on this thread.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
            link.SetPath(&HSTRING::from(target.as_os_str())).map_err(|e| e.to_string())?;
            link.SetArguments(&HSTRING::from(args)).map_err(|e| e.to_string())?;
            link.SetWorkingDirectory(&HSTRING::from(workdir.as_os_str())).map_err(|e| e.to_string())?;
            link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0).map_err(|e| e.to_string())?;
            link.SetDescription(&HSTRING::from(description)).map_err(|e| e.to_string())?;
            let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
            file.Save(&HSTRING::from(lnk.as_os_str()), true).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub fn assoc_changed() {
        unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
    }

    pub fn ask(title: &str, text: &str) -> bool {
        unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), MB_YESNO | MB_ICONQUESTION) == IDYES }
    }

    pub fn tell(title: &str, text: &str, error: bool) {
        let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
        unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(title), MB_OK | icon) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything under a temp folder and a scratch registry key, so the
    /// tests never touch the real Start menu, desktop or Apps & features.
    fn sandbox(name: &str) -> (PathBuf, Places) {
        let root = std::env::temp_dir().join(format!("fb-install-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let places = Places {
            programs: root.join("Programs"),
            start_menu: root.join("StartMenu"),
            desktop: root.join("Desktop"),
            reg_software: format!(r"Software\FactoryBuilderTest-{name}-{}", std::process::id()),
        };
        (root, places)
    }

    fn fake_build(root: &Path) -> PathBuf {
        let build = root.join("build");
        std::fs::create_dir_all(build.join("tools")).unwrap();
        std::fs::write(build.join("factory-builder.exe"), b"app").unwrap();
        for t in TOOLS {
            std::fs::write(build.join("tools").join(t), t.as_bytes()).unwrap();
        }
        build.join("factory-builder.exe")
    }

    fn cleanup(root: &Path, places: &Places) {
        let _ = windows_registry::CURRENT_USER.remove_tree(&places.reg_software);
        assert!(windows_registry::CURRENT_USER.open(&places.reg_software).is_err(), "scratch key removed");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn picking_a_parent_folder_appends_the_product() {
        let (root, places) = sandbox("resolve");
        let id = Identity::new(None, places.clone());
        assert_eq!(id.resolve_dir(Path::new(r"D:\Games")), Path::new(r"D:\Games\Factory Builder"));
        assert_eq!(id.resolve_dir(Path::new(r"D:\Games\factory builder")), Path::new(r"D:\Games\factory builder"));
        assert_eq!(id.default_dir(), places.programs.join("Factory Builder"));
        let test = Identity::new(Some(root.join("profile")), places.clone());
        assert_eq!(test.product, "Factory Builder (Test)");
        cleanup(&root, &places);
    }

    #[test]
    fn install_then_uninstall_leaves_nothing() {
        let (root, places) = sandbox("roundtrip");
        let src = fake_build(&root);
        let id = Identity::new(None, places.clone());
        assert!(id.installed().is_none());
        let dir = id.default_dir();
        let report = id.install(&dir, &src, true).unwrap();
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert_eq!(std::fs::read(dir.join(EXE_NAME)).unwrap(), b"app");
        for t in TOOLS {
            assert!(dir.join("tools").join(t).is_file());
        }
        let lnk = places.start_menu.join("Factory Builder.lnk");
        assert!(lnk.is_file() && places.desktop.join("Factory Builder.lnk").is_file(), "both shortcuts");
        let installed = id.installed().expect("registered");
        assert_eq!(installed.version, VERSION);
        assert!(same_file(Path::new(&installed.dir), &dir));
        let key = windows_registry::CURRENT_USER.open(id.uninstall_key()).unwrap();
        for v in ["DisplayName", "DisplayVersion", "Publisher", "DisplayIcon", "InstallLocation", "UninstallString"] {
            assert!(!key.get_string(v).unwrap().is_empty(), "{v}");
        }
        assert!(key.get_string("UninstallString").unwrap().contains("--uninstall"));
        assert!(key.get_u32("EstimatedSize").is_ok());
        let cmd = windows_registry::CURRENT_USER
            .open(format!(r"{}\Classes\{PROG_ID}\shell\open\command", places.reg_software))
            .unwrap()
            .get_string("")
            .unwrap();
        assert!(cmd.contains(EXE_NAME) && cmd.ends_with("\"%1\""), "{cmd}");

        assert_eq!(id.uninstall().unwrap(), None, "not the running exe");
        assert!(!dir.exists(), "install folder removed");
        assert!(!lnk.exists() && !places.desktop.join("Factory Builder.lnk").exists());
        assert!(id.installed().is_none());
        assert!(windows_registry::CURRENT_USER.open(format!(r"{}\Classes\.fbfactory", places.reg_software)).is_err());
        assert!(windows_registry::CURRENT_USER.open(format!(r"{}\Classes\.fbproject", places.reg_software)).is_err());
        cleanup(&root, &places);
    }

    #[test]
    fn refuses_folders_with_other_things_and_keeps_them_on_uninstall() {
        let (root, places) = sandbox("foreign");
        let src = fake_build(&root);
        let id = Identity::new(None, places.clone());
        let busy = root.join("Busy");
        std::fs::create_dir_all(&busy).unwrap();
        std::fs::write(busy.join("notes.txt"), b"mine").unwrap();
        assert!(id.install(&busy, &src, false).unwrap_err().contains("other files"));
        assert!(id.install(Path::new("C:\\"), &src, false).is_err(), "never a whole drive");

        // Something the user adds after installing survives uninstall.
        let dir = id.default_dir();
        id.install(&dir, &src, false).unwrap();
        assert!(!places.desktop.join("Factory Builder.lnk").exists(), "desktop shortcut unticked");
        std::fs::write(dir.join("mine.txt"), b"keep").unwrap();
        id.uninstall().unwrap();
        assert!(dir.join("mine.txt").is_file());
        assert!(!dir.join(EXE_NAME).exists());
        cleanup(&root, &places);
    }

    #[test]
    fn test_installs_are_separate_and_skip_the_association() {
        let (root, places) = sandbox("testid");
        let src = fake_build(&root);
        let id = Identity::new(Some(root.join("profile")), places.clone());
        let dir = id.default_dir();
        assert!(dir.ends_with("Factory Builder (Test)"));
        let report = id.install(&dir, &src, true).unwrap();
        assert!(!report.done.iter().any(|d| d.contains(".fbfactory")));
        assert!(windows_registry::CURRENT_USER.open(format!(r"{}\Classes\.fbfactory", places.reg_software)).is_err());
        let un = windows_registry::CURRENT_USER.open(id.uninstall_key()).unwrap().get_string("UninstallString").unwrap();
        assert!(un.contains("--test-profile"), "{un}");
        assert!(Identity::new(None, places.clone()).installed().is_none(), "the real identity isn't installed");
        id.uninstall().unwrap();
        cleanup(&root, &places);
    }

    #[test]
    fn uninstaller_removes_its_own_exe_after_exiting() {
        let (root, places) = sandbox("selfdelete");
        let dir = root.join("Factory Builder (Test) & more");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join(EXE_NAME);
        std::fs::write(&exe, b"app").unwrap();
        delete_after_exit(&exe).unwrap();
        let start = std::time::Instant::now();
        while dir.exists() && start.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        assert!(!dir.exists(), "exe and folder deleted a moment later");
        assert!(delete_after_exit(&root.join("100%").join(EXE_NAME)).is_err(), "names cmd.exe would mangle are refused");
        cleanup(&root, &places);
    }

    #[test]
    fn other_copies_hand_off_to_the_installed_one_unless_it_is_older() {
        let (root, places) = sandbox("handoff");
        let src = fake_build(&root);
        let ping = Path::new(&std::env::var("SystemRoot").unwrap()).join(r"System32\PING.EXE");
        std::fs::copy(&ping, &src).unwrap();
        let id = Identity::new(None, places.clone());
        assert!(!id.hand_off(&[]), "nothing installed");
        id.install(&id.default_dir(), &src, false).unwrap();
        let args = ["-n".to_string(), "1".into(), "127.0.0.1".into()];
        assert!(id.hand_off(&args), "started the installed copy");
        windows_registry::CURRENT_USER.create(id.uninstall_key()).unwrap().set_string("DisplayVersion", "0.0.1").unwrap();
        assert!(!id.hand_off(&args), "an older install doesn't take over a newer copy");
        std::thread::sleep(std::time::Duration::from_millis(500));
        id.uninstall().unwrap();
        cleanup(&root, &places);
    }

    #[test]
    fn real_places_point_at_the_users_folders() {
        // Only resolves paths; writes nothing.
        let p = user_places();
        assert!(p.programs.ends_with("Programs") && p.programs.parent().unwrap().is_dir());
        assert!(p.start_menu.is_dir() && p.start_menu.ends_with(r"Start Menu\Programs"), "{}", p.start_menu.display());
        assert!(p.desktop.is_dir());
        assert_eq!(p.reg_software, "Software");
    }

    #[test]
    fn updating_works_while_the_installed_copy_is_running() {
        use std::os::windows::process::CommandExt;
        let (root, places) = sandbox("update");
        let src = fake_build(&root);
        // A real program stands in for the app, so it can actually run.
        let ping = Path::new(&std::env::var("SystemRoot").unwrap()).join(r"System32\PING.EXE");
        std::fs::copy(&ping, &src).unwrap();
        let id = Identity::new(None, places.clone());
        let dir = id.default_dir();
        id.install(&dir, &src, false).unwrap();
        let mut running = std::process::Command::new(dir.join(EXE_NAME))
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(std::fs::remove_file(dir.join(EXE_NAME)).is_err(), "a running exe can't be deleted");
        std::fs::write(&src, b"app v2").unwrap();
        let r = id.install(&dir, &src, false);
        running.kill().unwrap();
        running.wait().unwrap();
        r.unwrap();
        assert_eq!(std::fs::read(dir.join(EXE_NAME)).unwrap(), b"app v2");
        id.uninstall().unwrap();
        assert!(!dir.exists(), "the moved-aside old exe is cleaned up too");
        cleanup(&root, &places);
    }
}
