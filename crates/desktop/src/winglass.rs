//! Glass windows: every normal app window gets a constant opacity (`windowGlass`, `windowOpacity`,
//! `windowGlassExclude` in shell.json), which DWM blends with no per-frame work here. The opacity
//! applies to the whole window, text included.
//!
//! The blur (`windowBlur`) is a pane: while a glass window is on screen the shell shows a full-screen
//! click-through window at the bottom of the z-order with the blurred wallpaper, so what shows
//! through looks like frosted glass. DWM cannot do it for other programs' windows: measured on
//! 2026-11-06, SetWindowCompositionAttribute (blur, acrylic) and DWMWA_SYSTEMBACKDROP_TYPE all report
//! success on a foreign window and change no pixel. Another window behind a glass one therefore
//! shows through sharp; only the wallpaper is blurred, and desktop icons are covered meanwhile.
//!
//! Changing another program's window style waits on that program's thread, so everything runs on
//! one thread of its own: a hung app can stall this feature but nothing else.
//!
//! Left alone: our own windows, excluded programs, fullscreen windows (games and video keep their
//! direct path to the screen; they get the effect back when they leave fullscreen), windows that
//! are already layered by their own app, and elevated windows (Windows refuses the change).
use std::collections::HashSet;
use std::sync::atomic::{AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;
use windows::core::BOOL;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::*;

#[derive(Clone, Debug, PartialEq)]
struct Settings {
    enabled: bool,
    alpha: u8,
    blur: bool,
    exclude: Vec<String>,
}

static SETTINGS: Mutex<Option<Settings>> = Mutex::new(None);
static THREAD: AtomicU32 = AtomicU32::new(0);
/// Requests applied by the thread, so [`stop`] can wait for windows to be given back.
static WANTED: AtomicU64 = AtomicU64::new(0);
static DONE: AtomicU64 = AtomicU64::new(0);
const WM_RECONFIGURE: u32 = WM_APP + 1;
/// To the shell's window: `wparam` != 0 when the blurred pane should show.
pub const WM_GLASS_FROST: u32 = WM_APP + 0x7A1;
static TARGET: AtomicIsize = AtomicIsize::new(0);

/// Opacity 0..1 as a layered-window alpha; values too low to read are raised.
pub fn alpha_of(opacity: f32) -> u8 {
    let o = if opacity.is_finite() { opacity } else { 1. };
    (o.clamp(0.6, 1.) * 255.).round() as u8
}

/// `exe` is a path or file name, entries of the list are file names; case is ignored and ".exe"
/// is optional on both sides.
pub fn excluded(exe: &str, list: &[String]) -> bool {
    let stem = |s: &str| {
        let s = s.rsplit(['\\', '/']).next().unwrap_or(s).to_lowercase();
        s.strip_suffix(".exe").map(str::to_string).unwrap_or(s)
    };
    let name = stem(exe);
    !name.is_empty() && list.iter().any(|e| stem(e) == name)
}

/// Apply the settings (from the shell's config). No thread exists until the feature is first on.
pub fn configure(target: HWND, enabled: bool, opacity: f32, blur: bool, exclude: Vec<String>) {
    TARGET.store(target.0 as isize, Ordering::SeqCst);
    let next = Settings {
        enabled,
        alpha: alpha_of(opacity),
        blur,
        exclude,
    };
    let mut cur = SETTINGS.lock().unwrap_or_else(|e| e.into_inner());
    if cur.as_ref() == Some(&next) {
        return;
    }
    *cur = Some(next);
    drop(cur);
    request();
    if enabled && THREAD.load(Ordering::SeqCst) == 0 {
        let _ = std::thread::Builder::new()
            .name("window-glass".into())
            .spawn(run);
    }
}

/// Look at every window again (after a resume, when events may have been missed).
pub fn rescan() {
    request();
}

fn request() {
    WANTED.fetch_add(1, Ordering::SeqCst);
    let t = THREAD.load(Ordering::SeqCst);
    if t != 0 {
        unsafe {
            let _ = PostThreadMessageW(t, WM_RECONFIGURE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Shell exit: make every window opaque again; waits up to 2 s (a hung app is skipped anyway).
pub fn stop() {
    if THREAD.load(Ordering::SeqCst) == 0 {
        return;
    }
    if let Some(s) = SETTINGS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        s.enabled = false;
    }
    request();
    let want = WANTED.load(Ordering::SeqCst);
    for _ in 0..40 {
        if DONE.load(Ordering::SeqCst) >= want {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// After repeated crashes nobody will run [`stop`]: find the configured (or default) opacity on
/// any window and make it opaque. Called by the supervisor, which holds no list of windows, so the
/// layered style itself stays: removing it from a window that had it would break that program.
pub fn give_back_after_crash(opacity: f32) {
    let ours = [alpha_of(opacity), alpha_of(DEFAULT_OPACITY)];
    for h in top_level_windows() {
        unsafe {
            if !IsHungAppWindow(h).as_bool() && ours.iter().any(|a| *a < 255 && our_alpha(h, *a)) {
                clear(h, false);
            }
        }
    }
}

pub const DEFAULT_OPACITY: f32 = 0.85;

fn top_level_windows() -> Vec<HWND> {
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        (*(l.0 as *mut Vec<HWND>)).push(h);
        BOOL(1)
    }
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut Vec<HWND> as isize));
    }
    v
}

/// The window is layered with exactly this constant alpha (ours, possibly from a run that crashed).
unsafe fn our_alpha(h: HWND, alpha: u8) -> bool {
    let (mut a, mut flags) = (0u8, LAYERED_WINDOW_ATTRIBUTES_FLAGS(0));
    GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_LAYERED.0 != 0
        && GetLayeredWindowAttributes(h, None, Some(&mut a), Some(&mut flags)).is_ok()
        && flags.0 & LWA_ALPHA.0 != 0
        && a == alpha
}

/// Opaque again; `strip` also removes the layered style (not when the app had it itself).
unsafe fn clear(h: HWND, strip: bool) {
    let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
    if strip {
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        SetWindowLongW(h, GWL_EXSTYLE, (ex & !WS_EX_LAYERED.0) as i32);
    }
}

/// Layered by the app itself but fully opaque, so it can take our opacity and give it back.
unsafe fn opaque_layered(h: HWND) -> bool {
    our_alpha(h, 255)
}

#[derive(Default)]
struct Engine {
    /// Windows we made translucent.
    tracked: HashSet<isize>,
    /// Tracked windows that went fullscreen and were made opaque for it.
    fullscreen: HashSet<isize>,
    /// Tracked windows that were layered before us: they keep the style when given back.
    app_layered: HashSet<isize>,
    settings: Option<Settings>,
    frosted: bool,
}

thread_local! { static ENGINE: std::cell::RefCell<Engine> = std::cell::RefCell::default(); }

impl Engine {
    fn on(&self) -> Option<&Settings> {
        self.settings.as_ref().filter(|s| s.enabled)
    }

    unsafe fn give_back(&mut self, key: isize) {
        let h = HWND(key as *mut _);
        let strip = !self.app_layered.remove(&key);
        if self.tracked.remove(&key) && IsWindow(Some(h)).as_bool() && !IsHungAppWindow(h).as_bool() {
            clear(h, strip);
        }
    }

    unsafe fn consider(&mut self, h: HWND) {
        let Some(s) = self.on().cloned() else {
            return;
        };
        let key = h.0 as isize;
        if GetAncestor(h, GA_ROOT) != h || !crate::shell::is_app_window(h) || IsHungAppWindow(h).as_bool() {
            return;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == std::process::id() || excluded(&crate::wm::window_exe(h, pid), &s.exclude) {
            self.give_back(key);
            return;
        }
        if crate::wm::covers_monitor(h) {
            if self.tracked.contains(&key) {
                self.give_back(key);
            }
            self.fullscreen.insert(key);
            return;
        }
        self.fullscreen.remove(&key);
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if self.tracked.contains(&key) || our_alpha(h, s.alpha) {
            // found with our opacity but not on record (a previous run that crashed): whether the
            // app was layered before is unknown, so it keeps the style when it is given back
            if self.tracked.insert(key) {
                self.app_layered.insert(key);
            }
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), s.alpha, LWA_ALPHA);
            return;
        }
        if ex & WS_EX_LAYERED.0 != 0 {
            if !opaque_layered(h) {
                return; // the app draws its own transparency: not ours to touch
            }
            self.app_layered.insert(key);
        }
        SetWindowLongW(h, GWL_EXSTYLE, (ex | WS_EX_LAYERED.0) as i32);
        if SetLayeredWindowAttributes(h, COLORREF(0), s.alpha, LWA_ALPHA).is_ok()
            && GetWindowLongW(h, GWL_EXSTYLE) as u32 & WS_EX_LAYERED.0 != 0
        {
            self.tracked.insert(key);
        }
    }

    /// Show the blurred pane while a glass window is actually on screen (not minimized, not on
    /// another virtual desktop), so an empty desktop keeps the sharp wallpaper and its icons.
    unsafe fn sync_frost(&mut self) {
        let want = self.on().is_some_and(|s| s.blur)
            && self.tracked.iter().any(|&key| {
                let h = HWND(key as *mut _);
                let mut cloaked = 0u32;
                let _ = windows::Win32::Graphics::Dwm::DwmGetWindowAttribute(
                    h,
                    windows::Win32::Graphics::Dwm::DWMWA_CLOAKED,
                    &mut cloaked as *mut u32 as *mut _,
                    4,
                );
                IsWindowVisible(h).as_bool() && !IsIconic(h).as_bool() && cloaked == 0
            });
        if want != self.frosted {
            self.frosted = want;
            let target = HWND(TARGET.load(Ordering::SeqCst) as *mut _);
            let _ = PostMessageW(Some(target), WM_GLASS_FROST, WPARAM(want as usize), LPARAM(0));
        }
    }

    unsafe fn reconfigure(&mut self) {
        self.settings = SETTINGS.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match self.on() {
            Some(_) => {
                for h in top_level_windows() {
                    self.consider(h);
                }
            }
            None => {
                for key in self.tracked.iter().copied().collect::<Vec<_>>() {
                    self.give_back(key);
                }
                self.fullscreen.clear();
            }
        }
        self.sync_frost();
    }
}

unsafe extern "system" fn on_event(
    _: HWINEVENTHOOK,
    event: u32,
    h: HWND,
    object: i32,
    child: i32,
    _: u32,
    _: u32,
) {
    if object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 || h.is_invalid() {
        return;
    }
    ENGINE.with(|e| {
        // never panic in a system callback: a nested event is simply skipped
        let Ok(mut e) = e.try_borrow_mut() else {
            return;
        };
        if e.on().is_none() {
            return;
        }
        let key = h.0 as isize;
        match event {
            EVENT_OBJECT_DESTROY => {
                e.tracked.remove(&key);
                e.fullscreen.remove(&key);
                e.app_layered.remove(&key);
            }
            // moves fire constantly: only a change of fullscreen state on a known window matters
            EVENT_OBJECT_LOCATIONCHANGE => {
                let full = crate::wm::covers_monitor(h);
                if (full && e.tracked.contains(&key)) || (!full && e.fullscreen.contains(&key)) {
                    e.consider(h);
                } else {
                    return; // the common case: nothing changed
                }
            }
            _ => e.consider(h),
        }
        e.sync_frost();
    });
}

fn run() {
    unsafe {
        THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
        for (min, max) in [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
            (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        ] {
            SetWinEventHook(
                min,
                max,
                None,
                Some(on_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
        }
        let apply = || {
            let seen = WANTED.load(Ordering::SeqCst);
            ENGINE.with(|e| e.borrow_mut().reconfigure());
            DONE.store(seen, Ordering::SeqCst);
        };
        apply();
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_RECONFIGURE {
                apply();
            } else {
                DispatchMessageW(&msg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alpha_is_clamped_to_readable_values() {
        assert_eq!(alpha_of(0.85), 217);
        assert_eq!(alpha_of(0.1), 153);
        assert_eq!(alpha_of(3.), 255);
        assert_eq!(alpha_of(f32::NAN), 255);
    }

    #[test]
    fn exclusion_matches_the_file_name_only() {
        let list = vec!["VLC.exe".to_string(), "mpv".to_string()];
        assert!(excluded(r"C:\Program Files\VideoLAN\vlc.exe", &list));
        assert!(excluded("mpv.exe", &list));
        assert!(!excluded(r"C:\vlc.exe\notepad.exe", &list));
        assert!(!excluded("", &list));
    }
}
