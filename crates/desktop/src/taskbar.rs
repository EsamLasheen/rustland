//! Keeps the Windows taskbar fully hidden (even on hover) while the shell runs, and gives it back on
//! exit exactly as it was (visibility + the user's auto-hide setting). Event-driven: Explorer's own
//! show/location events re-hide it before it paints; no polling.
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use windows::{
    core::{w, BOOL},
    Win32::{
        Foundation::*,
        UI::{Accessibility::*, Shell::*, WindowsAndMessaging::*},
    },
};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static EXPLORER_PID: AtomicU32 = AtomicU32::new(0);
/// The user's auto-hide state before we changed it (restored on exit).
static WAS_AUTOHIDE: AtomicBool = AtomicBool::new(false);
static HOOKS: Mutex<Vec<isize>> = Mutex::new(Vec::new());

fn class_is_taskbar(h: HWND) -> bool {
    let mut buf = [0u16; 32];
    let n = unsafe { GetClassNameW(h, &mut buf) } as usize;
    let c = String::from_utf16_lossy(&buf[..n]);
    c == "Shell_TrayWnd" || c == "Shell_SecondaryTrayWnd"
}
fn pid(h: HWND) -> u32 {
    let mut p = 0;
    unsafe {
        GetWindowThreadProcessId(h, Some(&mut p));
    }
    p
}

/// Explorer's taskbars (our own tray host also registers a "Shell_TrayWnd"; skip our process).
fn taskbars() -> Vec<HWND> {
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let v = &mut *(l.0 as *mut Vec<HWND>);
        if class_is_taskbar(h) && pid(h) != std::process::id() {
            v.push(h);
        }
        TRUE
    }
    let mut v: Vec<HWND> = vec![];
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

fn appbar_state() -> u32 {
    let mut d = APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        ..Default::default()
    };
    unsafe { SHAppBarMessage(ABM_GETSTATE, &mut d) as u32 }
}
fn set_autohide(on: bool) {
    unsafe {
        let mut d = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        d.hWnd = FindWindowW(w!("Shell_TrayWnd"), None).unwrap_or_default();
        d.lParam = LPARAM(if on {
            ABS_AUTOHIDE as isize
        } else {
            ABS_ALWAYSONTOP as isize
        });
        SHAppBarMessage(ABM_SETSTATE, &mut d);
    }
}

fn enforce(hide: bool) {
    for h in taskbars() {
        unsafe {
            if IsWindowVisible(h).as_bool() == hide {
                let _ = ShowWindow(h, if hide { SW_HIDE } else { SW_SHOWNA });
            }
        }
    }
}

unsafe extern "system" fn on_event(
    _: HWINEVENTHOOK,
    _ev: u32,
    h: HWND,
    obj: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    if obj == 0
        && ACTIVE.load(Ordering::Relaxed)
        && class_is_taskbar(h)
        && IsWindowVisible(h).as_bool()
    {
        let _ = ShowWindow(h, SW_HIDE);
    }
}

/// Watch only Explorer's process (show + location changes), not every window on the system.
fn hook_explorer() {
    let Some(tray) = taskbars().into_iter().next() else {
        return;
    };
    let p = pid(tray);
    if p == 0 || p == EXPLORER_PID.swap(p, Ordering::Relaxed) {
        return;
    }
    let mut hooks = HOOKS.lock().unwrap_or_else(|e| e.into_inner());
    for h in hooks.drain(..) {
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(h as *mut _));
        }
    }
    // show events only: the taskbar is re-hidden the moment Explorer shows it
    let h = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_SHOW,
            None,
            Some(on_event),
            p,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    if !h.is_invalid() {
        hooks.push(h.0 as isize);
    }
}

/// Hide the taskbar (call on the UI thread; hooks deliver through its message loop).
pub fn hide() {
    if ACTIVE.swap(true, Ordering::Relaxed) {
        return;
    }
    // The original preference is recorded once, before the first change. If a previous run ended
    // without restoring (sign-out, crash), the file still holds it: the current "auto-hide" state is
    // ours, not the user's, and must not overwrite it.
    let recorded = state_file().and_then(|f| std::fs::read_to_string(f).ok());
    let was = match recorded.as_deref().map(str::trim) {
        Some("autohide") => true,
        Some("visible") => false,
        _ => {
            let now = appbar_state() & ABS_AUTOHIDE != 0;
            if let Some(f) = state_file() {
                let _ = std::fs::write(f, if now { "autohide" } else { "visible" });
            }
            now
        }
    };
    WAS_AUTOHIDE.store(was, Ordering::Relaxed);
    set_autohide(true); // the work area no longer reserves the taskbar strip
    hook_explorer();
    enforce(true);
}

/// Explorer restarted ("TaskbarCreated"): new process, new windows.
pub fn explorer_restarted() {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    EXPLORER_PID.store(0, Ordering::Relaxed);
    set_autohide(true);
    hook_explorer();
    enforce(true);
}

/// Give the taskbar back exactly as it was.
pub fn restore() {
    if !ACTIVE.swap(false, Ordering::Relaxed) {
        return;
    }
    for h in HOOKS.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(h as *mut _));
        }
    }
    EXPLORER_PID.store(0, Ordering::Relaxed);
    set_autohide(WAS_AUTOHIDE.load(Ordering::Relaxed));
    enforce(false);
    if let Some(f) = state_file() {
        let _ = std::fs::remove_file(f);
    }
}

fn state_file() -> Option<std::path::PathBuf> {
    crate::platform::paths()
        .ok()
        .map(|p| p.data.join("taskbar-state"))
}

/// From another process after the shell died: show the taskbar with the user's original setting.
pub fn restore_after_crash() {
    let Some(f) = state_file() else { return };
    let Ok(s) = std::fs::read_to_string(&f) else {
        return;
    };
    set_autohide(s.trim() == "autohide");
    enforce(false);
    let _ = std::fs::remove_file(f);
}
