//! Per-user self-contained preview installer. No shell commands or downloaded runtimes.
use crate::platform::{known_folder, wide};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        System::{Com::*, Registry::*},
        UI::Shell::*,
    },
};

pub fn install_dir() -> io::Result<PathBuf> {
    Ok(known_folder(&FOLDERID_UserProgramFiles)?.join("MonarchNext"))
}
pub fn install() -> io::Result<PathBuf> {
    let Some(_guard) = crate::platform::Instance::named("Local\\MonarchNext.Setup.v1")? else {
        return Err(io::Error::other("Another installation is already running"));
    };
    let source = std::env::current_exe()?;
    let dir = install_dir()?;
    let shortcut_path = known_folder(&FOLDERID_Programs)?.join("RustLand.lnk");
    // earlier builds used another Start-menu name
    for old in ["Monarch Next.lnk", "Solo Leveling Shell.lnk"] {
        let _ = std::fs::remove_file(known_folder(&FOLDERID_Programs)?.join(old));
    }
    let target = dir.join("MonarchNext.exe");
    let upgrading = target.exists();
    if upgrading {
        // the running shell exits cleanly (screen space and taskbar given back) before replacement;
        // older builds (no shell window) are asked to close via their windows
        close_running_shell();
        close_processes_of(&target)?;
    }
    let result = install_into(
        &source,
        &dir,
        &shortcut_path,
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\MonarchNext",
    );
    if result.is_err() && upgrading && target.exists() {
        // never leave the user without a desktop: bring the previous version back
        let _ = std::process::Command::new(&target).arg("--shell").spawn();
    }
    result
}
fn install_into(
    source: &Path,
    dir: &Path,
    shortcut_path: &Path,
    registry_path: &str,
) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let target = dir.join("MonarchNext.exe");
    let upgrade = target.exists();
    if upgrade && fs::canonicalize(source)? == fs::canonicalize(&target)? {
        return Ok(target);
    }
    // in-place update; install has already closed a running shell. Settings and data stay.
    let result = (|| {
        let copied = fs::copy(source, dir.join("MonarchNext.pending.exe"))?;
        if copied != fs::metadata(source)?.len() {
            return Err(io::Error::other("The application copy was incomplete"));
        }
        let from = wide(dir.join("MonarchNext.pending.exe"));
        let to = wide(&target);
        unsafe {
            windows::Win32::Storage::FileSystem::MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                windows::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                    | windows::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
            )
            .map_err(io::Error::other)?;
        }
        if !shortcut_path.exists() {
            shortcut(&target, shortcut_path)?;
        }
        register_uninstall(&target, registry_path)?;
        Ok(target.clone())
    })();
    if result.is_err() && upgrade {
        // never remove a working installation because an update failed
        let _ = fs::remove_file(dir.join("MonarchNext.pending.exe"));
        return result;
    }
    if result.is_err() {
        let _ = fs::remove_file(dir.join("MonarchNext.pending.exe"));
        let _ = fs::remove_file(&target);
        let _ = fs::remove_file(shortcut_path);
        unsafe {
            let registry = wide(registry_path);
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(registry.as_ptr()));
        }
        let _ = fs::remove_dir(dir);
    }
    result
}
fn shortcut(target: &Path, shortcut_path: &Path) -> io::Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED)
            .ok()
            .map_err(io::Error::other)?;
        let result = (|| -> windows::core::Result<()> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let p = wide(target);
            link.SetPath(PCWSTR(p.as_ptr()))?;
            link.SetDescription(w!("RustLand"))?;
            let persist: IPersistFile = windows::core::Interface::cast(&link)?;
            let dest = wide(shortcut_path);
            persist.Save(PCWSTR(dest.as_ptr()), true)
        })();
        CoUninitialize();
        result.map_err(io::Error::other)
    }
}
fn set_string(key: HKEY, name: &str, value: &str) -> io::Result<()> {
    let n = wide(name);
    let v = wide(value);
    let data = unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), v.len() * 2) };
    unsafe {
        RegSetValueExW(key, PCWSTR(n.as_ptr()), None, REG_SZ, Some(data))
            .ok()
            .map_err(io::Error::other)
    }
}
fn register_uninstall(target: &Path, registry_path: &str) -> io::Result<()> {
    let mut key = HKEY::default();
    let registry = wide(registry_path);
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(registry.as_ptr()),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()
        .map_err(io::Error::other)?;
    }
    let result = (|| {
        set_string(key, "DisplayName", "RustLand")?;
        set_string(key, "DisplayVersion", env!("CARGO_PKG_VERSION"))?;
        set_string(key, "Publisher", "EsamLasheen")?;
        set_string(
            key,
            "InstallLocation",
            &target.parent().unwrap_or(Path::new("")).to_string_lossy(),
        )?;
        set_string(key, "DisplayIcon", &target.to_string_lossy())?;
        set_string(
            key,
            "UninstallString",
            &format!("\"{}\" --uninstall", target.display()),
        )
    })();
    unsafe {
        let _ = RegCloseKey(key);
    }
    result
}

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

/// The shell is registered to start at sign-in.
pub fn autostart_enabled() -> bool {
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            w!("MonarchNext"),
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        )
        .is_ok()
    }
}

/// Start the shell at sign-in (per user, no scheduled task or service needed).
pub fn set_autostart(target: &Path, on: bool) -> io::Result<()> {
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()
        .map_err(io::Error::other)?;
        let r = if on {
            set_string(
                key,
                "MonarchNext",
                &format!("\"{}\" --shell", target.display()),
            )
        } else {
            let _ = RegDeleteValueW(key, w!("MonarchNext"));
            Ok(())
        };
        let _ = RegCloseKey(key);
        r
    }
}

/// Ask a running shell to exit cleanly and wait (bounded) for it.
pub fn close_running_shell() {
    use windows::Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*};
    unsafe {
        let Ok(bar) = FindWindowW(w!("MonarchNext.Surface"), w!("Monarch Next · bar")) else {
            return;
        };
        let mut pid = 0;
        GetWindowThreadProcessId(bar, Some(&mut pid));
        // the supervisor must not restart a shell that was asked to close
        crate::platform::signal_shell_stop();
        let _ = PostMessageW(Some(bar), WM_CLOSE, WPARAM(0), LPARAM(0));
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            let _ = WaitForSingleObject(h, 8000);
            let _ = CloseHandle(h);
        }
    }
}

/// Close every running copy of `exe` (any build) through its top-level windows; never force-kill.
fn close_processes_of(exe: &Path) -> io::Result<()> {
    use windows::Win32::{
        Foundation::*,
        System::{Diagnostics::ToolHelp::*, Threading::*},
        UI::WindowsAndMessaging::*,
    };
    let want = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let pids: Vec<u32> = unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return Ok(());
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut out = vec![];
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            if e.th32ProcessID != std::process::id() {
                if let Ok(h) =
                    OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, e.th32ProcessID)
                {
                    let mut buf = [0u16; 1024];
                    let mut n = buf.len() as u32;
                    if QueryFullProcessImageNameW(
                        h,
                        PROCESS_NAME_WIN32,
                        windows::core::PWSTR(buf.as_mut_ptr()),
                        &mut n,
                    )
                    .is_ok()
                    {
                        let p = PathBuf::from(String::from_utf16_lossy(&buf[..n as usize]));
                        if fs::canonicalize(&p).unwrap_or(p) == want {
                            out.push(e.th32ProcessID);
                        }
                    }
                    let _ = CloseHandle(h);
                }
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        out
    };
    if pids.is_empty() {
        return Ok(());
    }
    unsafe extern "system" fn post_close(h: HWND, l: LPARAM) -> windows::core::BOOL {
        let pids = &*(l.0 as *const Vec<u32>);
        let mut pid = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pids.contains(&pid) {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
        windows::core::BOOL(1)
    }
    unsafe {
        let _ = EnumWindows(Some(post_close), LPARAM(&pids as *const _ as isize));
        for pid in &pids {
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, *pid) {
                let w = WaitForSingleObject(h, 8000);
                let _ = CloseHandle(h);
                if w != WAIT_OBJECT_0 {
                    return Err(io::Error::other(
                        "RustLand is still running. Close it, then run setup again.",
                    ));
                }
            }
        }
    }
    Ok(())
}

pub fn uninstall() -> io::Result<()> {
    close_running_shell();
    if let Ok(t) = std::env::current_exe() {
        let _ = set_autostart(&t, false);
    }
    // Run a temporary copy so the installed executable can be removed normally.
    let source = std::env::current_exe()?;
    let helper =
        std::env::temp_dir().join(format!("MonarchNext-remove-{}.exe", std::process::id()));
    fs::copy(&source, &helper)?;
    std::process::Command::new(&helper)
        .arg("--finish-uninstall")
        .arg(std::process::id().to_string())
        .spawn()?;
    Ok(())
}
pub fn finish_uninstall(parent: u32) -> io::Result<()> {
    use windows::Win32::{Foundation::*, System::Threading::*};
    unsafe {
        if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, parent) {
            let wait = WaitForSingleObject(h, 10000);
            let _ = CloseHandle(h);
            if wait != WAIT_OBJECT_0 {
                return Err(io::Error::other(
                    "The installed app did not exit; installation preserved",
                ));
            }
        }
    }
    let dir = install_dir()?;
    // Only named product files; never recursively delete a computed directory.
    let installed = dir.join("MonarchNext.exe");
    if installed.exists() {
        fs::remove_file(installed)?;
    }
    for name in [
        "RustLand.lnk",
        "Solo Leveling Shell.lnk",
        "Monarch Next.lnk",
    ] {
        let shortcut = known_folder(&FOLDERID_Programs)?.join(name);
        if shortcut.exists() {
            fs::remove_file(shortcut)?;
        }
    }
    unsafe {
        RegDeleteTreeW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\MonarchNext"),
        )
        .ok()
        .map_err(io::Error::other)?;
    }
    let _ = fs::remove_dir(dir);
    // Windows cannot delete the running temporary helper; it is intentionally left
    // in the OS temp directory. User settings are retained for reinstall.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Sandbox {
        root: PathBuf,
        registry: String,
    }
    impl Sandbox {
        fn new() -> Self {
            let id = format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let root = std::env::temp_dir().join(format!("monarch-setup-{id}"));
            fs::create_dir_all(&root).unwrap();
            Self {
                root,
                registry: format!("Software\\MonarchNextPackagingTest-{id}"),
            }
        }
        fn source(&self) -> PathBuf {
            let p = self.root.join("source.exe");
            fs::write(&p, b"installer copy fixture - never executed").unwrap();
            p
        }
    }
    impl Drop for Sandbox {
        fn drop(&mut self) {
            let key = wide(&self.registry);
            unsafe {
                let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()));
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }
    #[test]
    fn installs_exact_bytes_and_updates_in_place() {
        let s = Sandbox::new();
        let source = s.source();
        let dir = s.root.join("program");
        let link = s.root.join("Monarch Next.lnk");
        let exe = install_into(&source, &dir, &link, &s.registry).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), fs::read(&source).unwrap());
        assert!(link.exists());
        // same file again: no-op
        assert_eq!(install_into(&exe, &dir, &link, &s.registry).unwrap(), exe);
        // a newer build replaces the executable in place and keeps the shortcut
        let newer = s.root.join("newer.exe");
        let mut bytes = fs::read(&source).unwrap();
        bytes.extend_from_slice(b"update");
        fs::write(&newer, &bytes).unwrap();
        assert_eq!(install_into(&newer, &dir, &link, &s.registry).unwrap(), exe);
        assert_eq!(fs::read(&exe).unwrap(), bytes);
        assert!(link.exists());
        let registry = wide(&s.registry);
        let mut key = HKEY::default();
        unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(registry.as_ptr()),
                None,
                KEY_READ,
                &mut key,
            )
            .ok()
            .unwrap();
            RegCloseKey(key).ok().unwrap();
        }
    }
    #[test]
    fn failed_shortcut_rolls_back_the_new_installation() {
        let s = Sandbox::new();
        let source = s.source();
        let dir = s.root.join("program");
        let impossible_link = s.root.join("missing-parent").join("Monarch Next.lnk");
        assert!(install_into(&source, &dir, &impossible_link, &s.registry).is_err());
        assert!(!dir.join("MonarchNext.exe").exists());
        assert!(!dir.join("MonarchNext.pending.exe").exists());
        assert!(source.exists());
        let registry = wide(&s.registry);
        let mut key = HKEY::default();
        unsafe {
            assert!(RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(registry.as_ptr()),
                None,
                KEY_READ,
                &mut key
            )
            .is_err());
        }
    }
}
