//! Shell preferences, versioned and written atomically. A file that fails to parse is never
//! overwritten: the shell runs on defaults and saving is refused until the file is fixed or removed.
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf};

pub const SCHEMA: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ShellConfig {
    pub schema: u32,
    pub rank: char,
    /// Accent in use (from the wallpaper when `dynamic_accent`, else the rank colour or a preset).
    pub accent: String,
    pub dynamic_accent: bool,
    pub preset: String,
    pub custom_palette: Option<crate::ai::Palette>,
    pub reduce_motion: bool,
    pub leftbar_autohide: bool,
    pub focus_pulse: String,
    pub rotate_enabled: bool,
    pub rotate_hours: f32,
    /// Folder with one sub-folder per rank (E … S) of wallpapers; empty = Pictures\Shadow Monarch.
    pub rank_folders: String,
    pub wallpaper: String,
    pub weather_city: String,
    pub native_hotkeys: bool,
    pub follow_new_windows: bool,
    /// Unix ms of the last automatic wallpaper rotation (rotation survives restarts).
    pub last_rotation_ms: u64,
    /// Obsidian focus-timer state file (optional; empty = no focus timer).
    pub focus_file: String,
    /// Material You source colour of the current wallpaper (dynamic colours; empty = none).
    pub seed: String,
    /// Name used in the dashboard greeting (empty = no name).
    pub user_name: String,
    /// Tile and manage windows (workspaces, rules, focus-follows-mouse).
    pub window_manager: bool,
    /// Hide the Windows taskbar while the shell runs.
    pub hide_taskbar: bool,
    /// Wallpaper, clock layer and active-window glow.
    pub wallpaper_engine: bool,
    /// Frosted-glass surfaces (blur of the desktop behind the bar, capsule and panels).
    pub glass: bool,
    /// Size of the bar, capsule and panels relative to the system scale (1.0 – 1.5).
    pub ui_scale: f32,
    /// Rasto, the little villain in the top bar.
    pub pet: bool,
    /// See-through app windows (see `winglass.rs`).
    pub window_glass: bool,
    /// Their opacity, 0.6 – 1.0.
    pub window_opacity: f32,
    /// Blur the wallpaper behind them.
    pub window_blur: bool,
    /// Programs that stay opaque, by file name.
    pub window_glass_exclude: Vec<String>,
}

impl ShellConfig {
    /// `ui_scale` as used: a hand-edited value can not make the shell unusable.
    pub fn ui_scale_clamped(&self) -> f32 {
        if self.ui_scale.is_finite() {
            self.ui_scale.clamp(1., 1.5)
        } else {
            1.
        }
    }
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            rank: 'E',
            accent: "#62baf8".into(),
            dynamic_accent: true,
            preset: "ShadowMonarch".into(),
            custom_palette: None,
            reduce_motion: false,
            leftbar_autohide: true,
            focus_pulse: "static".into(), // the breathing glow ticks 12x/s: opt-in
            rotate_enabled: true,
            rotate_hours: 3.,
            rank_folders: String::new(),
            wallpaper: String::new(),
            weather_city: String::new(),
            native_hotkeys: true,
            follow_new_windows: true,
            last_rotation_ms: 0,
            focus_file: String::new(),
            seed: String::new(),
            user_name: String::new(),
            window_manager: true,
            hide_taskbar: true,
            wallpaper_engine: true,
            glass: false,
            ui_scale: 1.,
            pet: true,
            window_glass: false, // changes how every program looks: opt-in
            window_opacity: crate::winglass::DEFAULT_OPACITY,
            window_blur: true,
            window_glass_exclude: ["vlc.exe", "mpv.exe", "obs64.exe", "mstsc.exe"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// The six hunter ranks and their colours.
pub const RANKS: [(char, &str); 6] = [
    ('E', "#62baf8"),
    ('D', "#49eef3"),
    ('C', "#52e09e"),
    ('B', "#b981f8"),
    ('A', "#f8c54f"),
    ('S', "#f55cbd"),
];
pub fn rank_color(rank: char) -> &'static str {
    RANKS
        .iter()
        .find(|(r, _)| *r == rank)
        .map(|(_, c)| *c)
        .unwrap_or("#62baf8")
}

pub struct Store {
    path: PathBuf,
    pub config: ShellConfig,
    pub writable: bool,
    pub error: Option<String>,
}

impl Store {
    pub fn open() -> io::Result<Self> {
        let path = crate::platform::paths()?.data.join("shell.json");
        Ok(Self::at(path))
    }
    pub fn at(path: PathBuf) -> Self {
        let mut store = match std::fs::read(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Self {
                path,
                config: ShellConfig::default(),
                writable: true,
                error: None,
            },
            Err(e) => Self {
                path,
                config: ShellConfig::default(),
                writable: false,
                error: Some(e.to_string()),
            },
            Ok(bytes) => match serde_json::from_slice::<ShellConfig>(&bytes) {
                Ok(c) if c.schema == SCHEMA => Self {
                    path,
                    config: c,
                    writable: true,
                    error: None,
                },
                Ok(c) => Self {
                    path,
                    config: ShellConfig::default(),
                    writable: false,
                    error: Some(format!(
                        "Unsupported shell.json version {}; original preserved",
                        c.schema
                    )),
                },
                Err(e) => Self {
                    path,
                    config: ShellConfig::default(),
                    writable: false,
                    error: Some(format!("shell.json is invalid ({e}); original preserved")),
                },
            },
        };
        if !store.writable {
            // A broken configuration must not unexpectedly take ownership of the desktop.
            store.config.window_manager = false;
            store.config.hide_taskbar = false;
            store.config.window_glass = false;
            let previous = store.path.with_file_name("shell.previous.json");
            if let Ok(bytes) = std::fs::read(previous) {
                if let Ok(c) = serde_json::from_slice::<ShellConfig>(&bytes) {
                    if c.schema == SCHEMA {
                        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
                        let preserved = store.path.with_extension(format!("corrupt-{stamp}.json"));
                        if std::fs::rename(&store.path, preserved).is_ok() {
                            store.config = c;
                            store.writable = true;
                            store.error = Some("Recovered previous settings; original preserved".into());
                        }
                    }
                }
            }
        }
        store
    }
    /// Atomic replace (temp file + MoveFileEx), previous version kept as shell.previous.json.
    pub fn save(&self) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::other(
                self.error
                    .clone()
                    .unwrap_or_else(|| "settings are read-only".into()),
            ));
        }
        let dir = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("no settings folder"))?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("shell.pending.json");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&serde_json::to_vec_pretty(&self.config).map_err(io::Error::other)?)?;
            f.sync_all()?;
        }
        if self.path.exists() {
            let _ = std::fs::copy(&self.path, dir.join("shell.previous.json"));
        }
        let (src, dst) = (
            crate::platform::wide(&tmp),
            crate::platform::wide(&self.path),
        );
        unsafe {
            windows::Win32::Storage::FileSystem::MoveFileExW(
                windows::core::PCWSTR(src.as_ptr()),
                windows::core::PCWSTR(dst.as_ptr()),
                windows::Win32::Storage::FileSystem::MOVEFILE_REPLACE_EXISTING
                    | windows::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
            )
            .map_err(io::Error::other)
        }
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    fn fixture() -> PathBuf {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("rustland-config-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[test]
    fn corrupt_settings_restore_last_good_and_remain_writable() {
        let dir = fixture(); let path = dir.join("shell.json");
        std::fs::write(&path, b"{broken").unwrap();
        let expected = ShellConfig { user_name: "Recovered".into(), ..Default::default() };
        std::fs::write(dir.join("shell.previous.json"), serde_json::to_vec(&expected).unwrap()).unwrap();
        let store = Store::at(path.clone());
        assert!(store.writable); assert_eq!(store.config, expected);
        store.save().unwrap();
        assert_eq!(Store::at(path).config, expected);
        assert!(std::fs::read_dir(&dir).unwrap().any(|p| p.unwrap().file_name().to_string_lossy().contains("corrupt-")));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn no_valid_backup_starts_without_changing_desktop() {
        let dir = fixture(); let path = dir.join("shell.json");
        std::fs::write(&path, b"invalid").unwrap();
        let store = Store::at(path.clone());
        assert!(!store.writable); assert!(!store.config.window_manager); assert!(!store.config.hide_taskbar);
        assert!(store.save().is_err()); assert_eq!(std::fs::read(path).unwrap(), b"invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
