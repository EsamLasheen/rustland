use monarch_core::{Paths, Settings};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};
use windows::{
    core::{GUID, PCWSTR},
    Win32::{
        Foundation::*,
        Storage::FileSystem::*,
        System::{Com::CoTaskMemFree, Threading::*},
        UI::Shell::*,
    },
};

pub fn wide(s: impl AsRef<std::ffi::OsStr>) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn known_folder(id: &GUID) -> io::Result<PathBuf> {
    unsafe {
        let p = SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).map_err(io::Error::other)?;
        let value = p.to_string().map_err(io::Error::other);
        CoTaskMemFree(Some(p.0.cast()));
        value.map(PathBuf::from)
    }
}
pub fn paths() -> io::Result<Paths> {
    Ok(Paths::under(&known_folder(&FOLDERID_LocalAppData)?))
}

pub fn log(paths: &Paths, message: &str) {
    let _ = fs::create_dir_all(&paths.data);
    if fs::metadata(&paths.log)
        .map(|m| m.len() > 1024 * 1024)
        .unwrap_or(false)
    {
        let _ = fs::rename(&paths.log, paths.data.join("session.previous.log"));
    }
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log)
    {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "{stamp} {message}");
    }
}

pub fn save(paths: &Paths, settings: &Settings) -> io::Result<()> {
    let bytes = settings.bytes()?;
    fs::create_dir_all(&paths.data)?;
    let temp = paths.data.join("settings.pending.json");
    let mut file = fs::File::create(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    // A bad original configuration is never replaced by an implicit default.
    if paths.settings.exists() {
        Settings::load(&paths.settings)?;
        fs::copy(&paths.settings, paths.data.join("settings.previous.json"))?;
    }
    let src = wide(&temp);
    let dst = wide(&paths.settings);
    unsafe {
        MoveFileExW(
            PCWSTR(src.as_ptr()),
            PCWSTR(dst.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(io::Error::other)
    }
}

pub fn load_tasks(path: &Path) -> io::Result<monarch_core::local_data::Tasks> {
    if !path.exists() {
        return Ok(Default::default());
    }
    if fs::metadata(path)?.len() > 4 * 1024 * 1024 {
        return Err(io::Error::other(
            "Task file exceeds limit; original preserved",
        ));
    }
    let data: monarch_core::local_data::Tasks =
        serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)?;
    data.validate()?;
    Ok(data)
}
pub fn save_tasks(path: &Path, tasks: &monarch_core::local_data::Tasks) -> io::Result<()> {
    tasks.validate()?;
    let bytes = serde_json::to_vec(tasks).map_err(io::Error::other)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(io::Error::other("Task data exceeds storage limit"));
    }
    if path.exists() {
        load_tasks(path)?;
        fs::copy(path, path.with_extension("previous.json"))?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("pending.json");
    let mut file = fs::File::create(&temp)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    let src = wide(&temp);
    let dst = wide(path);
    unsafe {
        MoveFileExW(
            PCWSTR(src.as_ptr()),
            PCWSTR(dst.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(io::Error::other)
    }
}

pub struct Instance(HANDLE);
impl Instance {
    pub fn acquire() -> io::Result<Option<Self>> {
        Self::named("Local\\MonarchNext.Preview.v1")
    }
    pub fn named(name: &str) -> io::Result<Option<Self>> {
        let name = wide(name);
        unsafe {
            let h = CreateMutexW(None, false, PCWSTR(name.as_ptr())).map_err(io::Error::other)?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(h);
                return Ok(None);
            }
            Ok(Some(Self(h)))
        }
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[derive(Debug, Clone)]
pub struct Application {
    pub label: String,
    pub search_key: String,
    pub path: PathBuf,
}
pub fn applications() -> Vec<Application> {
    fn visit(dir: &Path, depth: usize, out: &mut Vec<Application>) {
        if depth > 8 || out.len() >= 5000 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            if out.len() >= 5000 {
                break;
            }
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            let p = e.path();
            if kind.is_dir() {
                visit(&p, depth + 1, out);
            } else if p.extension().is_some_and(|e| {
                e.eq_ignore_ascii_case("lnk") || e.eq_ignore_ascii_case("appref-ms")
            }) {
                if let Some(label) = p.file_stem().and_then(|s| s.to_str()) {
                    out.push(Application {
                        label: label.into(),
                        search_key: label.to_lowercase(),
                        path: p,
                    });
                }
            }
        }
    }
    let mut out = vec![];
    for id in [FOLDERID_Programs, FOLDERID_CommonPrograms] {
        if let Ok(path) = known_folder(&id) {
            visit(&path, 0, &mut out);
        }
    }
    out.sort_by(|a, b| a.search_key.cmp(&b.search_key));
    out.dedup_by(|a, b| a.label.eq_ignore_ascii_case(&b.label) && a.path == b.path);
    merge_apps_folder(out, apps_folder())
}
/// Upper bound on the whole index (shortcuts + Apps folder entries).
const MAX_APPS: usize = 5000;
/// `(display name, parsing name)` of every entry in the shell Apps folder: Store/UWP apps (parsing
/// name = AUMID) as well as desktop apps. Initialises COM (apartment-threaded) on the calling
/// thread if it has none yet; empty on any failure.
fn apps_folder() -> Vec<(String, String)> {
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, IBindCtx, COINIT_APARTMENTTHREADED,
    };
    let mut out = vec![];
    unsafe {
        // S_FALSE (already initialised) also needs a matching CoUninitialize; RPC_E_CHANGED_MODE
        // (thread is MTA) does not, and the shell folder still enumerates there.
        let com = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let read = |item: &IShellItem, kind: SIGDN| -> Option<String> {
            let p = item.GetDisplayName(kind).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0.cast()));
            s.filter(|s| !s.trim().is_empty())
        };
        if let Ok(folder) =
            SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)
        {
            if let Ok(items) =
                folder.BindToHandler::<_, IEnumShellItems>(None::<&IBindCtx>, &BHID_EnumItems)
            {
                let mut one = [None];
                while out.len() < MAX_APPS && items.Next(&mut one, None).is_ok() {
                    let Some(item) = one[0].take() else { break };
                    if let (Some(label), Some(id)) = (
                        read(&item, SIGDN_NORMALDISPLAY),
                        read(&item, SIGDN_PARENTRELATIVEPARSING),
                    ) {
                        out.push((label, id));
                    }
                }
            }
        }
        if com {
            CoUninitialize();
        }
    }
    out
}
/// Add Apps folder entries whose name no Start-menu shortcut already has (shortcuts win: they keep
/// their arguments and resolve to a real file for icons). Entries launch as `shell:AppsFolder\<id>`.
fn merge_apps_folder(
    mut apps: Vec<Application>,
    folder: Vec<(String, String)>,
) -> Vec<Application> {
    let mut seen: std::collections::HashSet<String> =
        apps.iter().map(|a| a.search_key.clone()).collect();
    for (label, id) in folder {
        if apps.len() >= MAX_APPS {
            break;
        }
        let key = label.to_lowercase();
        if seen.insert(key.clone()) {
            apps.push(Application {
                label,
                search_key: key,
                path: PathBuf::from(format!("shell:AppsFolder\\{id}")),
            });
        }
    }
    apps.sort_by(|a, b| a.search_key.cmp(&b.search_key));
    apps
}
pub fn search(apps: &[Application], query: &str) -> Vec<usize> {
    let needle = query.to_lowercase();
    apps.iter()
        .enumerate()
        .filter(|(_, a)| a.search_key.contains(&needle))
        .take(200)
        .map(|(i, _)| i)
        .collect()
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemSnapshot {
    pub cpu: Option<f64>,
    pub memory: Option<u32>,
    pub used_gib: f64,
    pub total_gib: f64,
    pub battery: Option<u8>,
    pub charging: bool,
}
pub fn system_snapshot(before: Option<(u64, u64)>) -> (SystemSnapshot, Option<(u64, u64)>) {
    use windows::Win32::System::{Power::*, SystemInformation::*};
    let mut s = SystemSnapshot::default();
    let mut times = None;
    unsafe {
        let (mut idle, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        if GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)).is_ok() {
            let n = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
            let after = (n(kernel).saturating_add(n(user)), n(idle));
            s.cpu = before.and_then(|b| monarch_core::cpu_percent(b, after));
            times = Some(after);
        }
        let mut m = MEMORYSTATUSEX {
            dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
            ..Default::default()
        };
        if GlobalMemoryStatusEx(&mut m).is_ok() {
            s.memory = Some(m.dwMemoryLoad);
            s.total_gib = m.ullTotalPhys as f64 / 1073741824.0;
            s.used_gib = m.ullTotalPhys.saturating_sub(m.ullAvailPhys) as f64 / 1073741824.0;
        }
        let mut power = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut power).is_ok() {
            s.charging = power.ACLineStatus == 1;
            if power.BatteryLifePercent <= 100 {
                s.battery = Some(power.BatteryLifePercent);
            }
        }
    }
    (s, times)
}
/// Named event a running shell supervisor watches: set = "exit requested, do not restart".
pub const SHELL_STOP_EVENT: windows::core::PCWSTR = windows::core::w!("Local\\MonarchNext.Stop");
pub fn shell_stop_event() -> Option<HANDLE> {
    unsafe {
        windows::Win32::System::Threading::CreateEventW(None, true, false, SHELL_STOP_EVENT).ok()
    }
}
/// Ask the supervisor not to restart the shell (installer, uninstaller). The handle stays open until
/// this process exits so the supervisor can observe the signal.
pub fn signal_shell_stop() {
    if let Some(e) = shell_stop_event() {
        unsafe {
            let _ = windows::Win32::System::Threading::SetEvent(e);
        }
    }
}
pub fn open(path: &Path) -> io::Result<()> {
    let p = wide(path);
    let result = unsafe {
        ShellExecuteW(
            None,
            windows::core::w!("open"),
            PCWSTR(p.as_ptr()),
            None,
            None,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    if result.0 as isize > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Windows could not open the item ({})",
            result.0 as isize
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn apps_folder_entries_merge_and_rank() {
        let lnk = vec![Application {
            label: "Notepad".into(),
            search_key: "notepad".into(),
            path: PathBuf::from(r"C:\Start\Notepad.lnk"),
        }];
        let folder = vec![
            ("Calculator".into(), "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App".into()),
            ("NOTEPAD".into(), "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App".into()),
            ("Settings".into(), "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel".into()),
        ];
        let apps = merge_apps_folder(lnk, folder);
        // the shortcut wins over the same-named Store entry
        assert_eq!(apps.len(), 3);
        let note = apps.iter().find(|a| a.search_key == "notepad").unwrap();
        assert!(note.path.ends_with("Notepad.lnk"));
        let names: Vec<&str> = apps.iter().map(|a| a.search_key.as_str()).collect();
        let hit = &apps[monarch_core::launcher::rank(&names, "calc", 8)[0]];
        assert_eq!(hit.label, "Calculator");
        assert_eq!(
            hit.path,
            PathBuf::from(r"shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App")
        );
        let settings = monarch_core::launcher::rank(&names, "settings", 8)[0];
        assert_eq!(apps[settings].label, "Settings");
    }
    #[test]
    fn task_store_roundtrip_and_corruption_preservation() {
        let root = std::env::temp_dir().join(format!(
            "monarch-task-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = root.join("todo.json");
        let mut tasks = monarch_core::local_data::Tasks::default();
        tasks.add("مهمة اختبار");
        save_tasks(&path, &tasks).unwrap();
        assert_eq!(load_tasks(&path).unwrap().items, tasks.items);
        tasks.items[0].urgent = true;
        save_tasks(&path, &tasks).unwrap();
        assert!(load_tasks(&path).unwrap().items[0].urgent);
        fs::write(&path, b"broken").unwrap();
        assert!(save_tasks(&path, &tasks).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"broken");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn settings_replace_is_atomic_and_corrupt_original_is_preserved() {
        let root = std::env::temp_dir().join(format!(
            "monarch-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let p = Paths::under(&root);
        let mut s = Settings::default();
        save(&p, &s).unwrap();
        s.gap = 14;
        save(&p, &s).unwrap();
        assert_eq!(Settings::load(&p.settings).unwrap().gap, 14);
        assert_eq!(
            Settings::load(&p.data.join("settings.previous.json"))
                .unwrap()
                .gap,
            10
        );
        fs::write(&p.settings, b"broken").unwrap();
        assert!(save(&p, &s).is_err());
        assert_eq!(fs::read(&p.settings).unwrap(), b"broken");
        fs::remove_dir_all(root).unwrap();
    }
}
