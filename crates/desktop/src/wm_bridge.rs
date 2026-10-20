//! One seam between the shell surfaces and the subsystems (tiling WM, wallpaper/glow, system controls).
//! Surfaces and hotkeys call only these functions, so subsystems can evolve without touching UI code.
//! Window actions here are the direct Win32 fallbacks used when the tiling manager is not running.
use std::cell::Cell;
use windows::Win32::{
    Foundation::*,
    Security::*,
    System::{Power::SetSuspendState, Shutdown::*, Threading::*},
    UI::WindowsAndMessaging::*,
};

use crate::wm::{self, DragKind};
use monarch_core::wm_layout::{Dir, WmAction};
use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, Ordering};

thread_local! {
    static ROOT: Cell<isize> = const { Cell::new(0) };
    static WM: RefCell<Option<wm::Wm>> = const { RefCell::new(None) };
}
/// 0 = no drag, 1 = move, 2 = resize (written by the mouse hook only).
static DRAG: AtomicU8 = AtomicU8::new(0);

/// Another tiling manager (the legacy komorebi stack) must never fight ours for the same windows.
/// Cached for the session: decided once at start.
pub fn foreign_running() -> bool {
    FOREIGN.load(Ordering::Relaxed)
}
static FOREIGN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn foreign_manager_running() -> bool {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return false;
        };
        let mut e = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let n = String::from_utf16_lossy(
                &e.szExeFile[..e.szExeFile.iter().position(|c| *c == 0).unwrap_or(0)],
            )
            .to_lowercase();
            if n == "komorebi.exe" || n == "glazewm.exe" {
                found = true;
                break;
            }
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
        found
    }
}

pub fn start(root: HWND, enabled: bool) {
    ROOT.with(|r| r.set(root.0 as isize));
    let foreign = foreign_manager_running();
    FOREIGN.store(foreign, Ordering::Relaxed);
    if !enabled || std::env::var_os("MONARCH_NO_WM").is_some() || foreign {
        if let Ok(p) = crate::platform::paths() {
            crate::platform::log(
                &p,
                "window manager off (disabled in settings, MONARCH_NO_WM, or another manager is running)",
            );
        }
        // windows hidden by a crashed run must come back even when the manager stays off
        let _ = wm::recover();
        return;
    }
    match wm::Wm::start(root) {
        Ok(w) => WM.with(|m| *m.borrow_mut() = Some(w)),
        Err(e) => {
            if let Ok(p) = crate::platform::paths() {
                crate::platform::log(&p, &format!("window manager: {e}"));
            }
        }
    }
}
pub fn stop() {
    if let Some(w) = WM.with(|m| m.borrow_mut().take()) {
        w.stop();
    }
}

fn verb_action(cmd: &str, arg: Option<&str>) -> Option<WmAction> {
    let dir = || match arg? {
        "left" => Some(Dir::Left),
        "right" => Some(Dir::Right),
        "up" => Some(Dir::Up),
        "down" => Some(Dir::Down),
        _ => None,
    };
    let n = || arg?.parse::<u8>().ok();
    Some(match cmd {
        "close" => WmAction::Close,
        "minimize" => WmAction::Minimize,
        "toggle-maximize" => WmAction::ToggleMaximize,
        "toggle-float" => WmAction::ToggleFloat,
        "flip-layout" => WmAction::Flip,
        "unminimize" => WmAction::RestoreLast,
        "focus" => WmAction::Focus(dir()?),
        "move" => WmAction::Move(dir()?),
        "resize" => WmAction::ResizeAxis {
            vertical: false,
            grow: arg == Some("increase"),
        },
        "focus-workspace" => WmAction::FocusWorkspace(n()?),
        "send-to-workspace" => WmAction::SendToWorkspace(n()?),
        "focus-last-workspace" => WmAction::FocusLastWorkspace,
        "special" => WmAction::ToggleSpecial,
        "cycle-workspace" => WmAction::CycleWorkspace {
            next: arg == Some("next"),
        },
        "retile" => WmAction::Retile,
        _ => return None,
    })
}

fn own_window(h: HWND) -> bool {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid == std::process::id()
}

fn fg() -> Option<HWND> {
    let h = unsafe { GetForegroundWindow() };
    (!h.is_invalid()).then_some(h)
}

/// Window-manager verbs (same names as the legacy komorebic commands).
pub fn action(verb: &str) {
    let mut it = verb.split_whitespace();
    let (cmd, arg) = (it.next().unwrap_or(""), it.next());
    // our own surfaces (panel, capsule, launcher) are never closed/minimized/maximized as windows:
    // window verbs aimed at them just dismiss the surface
    if matches!(
        cmd,
        "close" | "minimize" | "toggle-maximize" | "toggle-float"
    ) && fg().is_some_and(own_window)
    {
        crate::runtime::dismiss_surfaces();
        return;
    }
    if let Some(a) = verb_action(cmd, arg) {
        if wm::perform(a) {
            return;
        }
    }
    unsafe {
        match cmd {
            "close" => {
                if let Some(h) = fg() {
                    let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
            }
            "minimize" => {
                if let Some(h) = fg() {
                    let _ = ShowWindow(h, SW_MINIMIZE);
                }
            }
            "toggle-maximize" => {
                if let Some(h) = fg() {
                    let _ = ShowWindow(
                        h,
                        if IsZoomed(h).as_bool() {
                            SW_RESTORE
                        } else {
                            SW_MAXIMIZE
                        },
                    );
                }
            }
            "unminimize" => {
                unminimize_last();
            }
            _ => {
                let _ = arg;
            }
        }
    }
}

/// Restore the most recently minimised top-level window (z-order walk, cheapest first).
unsafe fn unminimize_last() {
    let mut h = GetTopWindow(None).unwrap_or_default();
    while !h.is_invalid() {
        if IsIconic(h).as_bool()
            && IsWindowVisible(h).as_bool()
            && GetWindow(h, GW_OWNER).unwrap_or_default().is_invalid()
        {
            let _ = ShowWindow(h, SW_RESTORE);
            let _ = SetForegroundWindow(h);
            return;
        }
        h = GetWindow(h, GW_HWNDNEXT).unwrap_or_default();
    }
}

unsafe fn enable_shutdown_privilege() -> bool {
    let mut token = HANDLE::default();
    if OpenProcessToken(
        GetCurrentProcess(),
        TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
        &mut token,
    )
    .is_err()
    {
        return false;
    }
    let mut luid = LUID::default();
    let ok = LookupPrivilegeValueW(None, windows::core::w!("SeShutdownPrivilege"), &mut luid)
        .is_ok()
        && {
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None).is_ok()
        };
    let _ = CloseHandle(token);
    ok
}

/// lock · sleep · hibernate · logout · restart · shutdown
pub fn power(act: &str) {
    unsafe {
        match act {
            "lock" => {
                let _ = windows::Win32::System::Shutdown::LockWorkStation();
            }
            "sleep" | "hibernate" => {
                let hibernate = act == "hibernate";
                // SetSuspendState needs the shutdown privilege and returns only after resume:
                // run it off the UI thread. Modern-standby PCs refuse S3, so turning the
                // display off (which enters standby there) is the fallback.
                std::thread::spawn(move || {
                    enable_shutdown_privilege();
                    if !SetSuspendState(hibernate, false, false) && !hibernate {
                        let _ = PostMessageW(
                            Some(HWND_BROADCAST),
                            WM_SYSCOMMAND,
                            WPARAM(SC_MONITORPOWER as usize),
                            LPARAM(2),
                        );
                    }
                });
            }
            "logout" => {
                let _ = ExitWindowsEx(EWX_LOGOFF, SHTDN_REASON_MAJOR_OTHER);
            }
            "restart" | "shutdown" => {
                enable_shutdown_privilege();
                let flags = if act == "restart" {
                    EWX_REBOOT
                } else {
                    EWX_POWEROFF
                };
                let _ = ExitWindowsEx(flags, SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_FLAG_PLANNED);
            }
            _ => {}
        }
    }
}

/// Named destinations used by panel buttons.
pub fn open(target: &str) {
    let uri = match target {
        "notifications" => "ms-actioncenter:",
        "settings" => "ms-settings:",
        "wifi" => "ms-settings:network-wifi",
        "bluetooth" => "ms-settings:bluetooth",
        "sound" => "ms-settings:sound",
        "display" => "ms-settings:display",
        "taskmgr" => "taskmgr",
        "explorer" => "explorer",
        "obsidian" => "obsidian://open?vault=obsidian",
        other => other,
    };
    let _ = crate::platform::open(std::path::Path::new(uri));
}

pub fn work_area_changed() {
    let _ = wm::perform(WmAction::Retile);
}

/// Called from the LL mouse hook for every move: drag tracking first, else focus-follows-mouse.
pub fn mouse_move(p: POINT) {
    let drag = DRAG.load(Ordering::Relaxed);
    if drag != 0 {
        // The release can be lost (UAC prompt, Ctrl+Alt+Del, hook timeout): end the drag
        // once the button is up instead of leaving the window glued to the cursor.
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON,
        };
        let button = if drag == 2 { VK_RBUTTON } else { VK_LBUTTON };
        if unsafe { GetAsyncKeyState(button.0 as i32) } >= 0 {
            drag_end(p, drag == 2);
            return;
        }
        wm::drag_update(p.x, p.y);
    } else {
        wm::pointer_moved(p.x, p.y);
    }
}
fn win_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    unsafe { GetAsyncKeyState(VK_LWIN.0 as i32) < 0 || GetAsyncKeyState(VK_RWIN.0 as i32) < 0 }
}
/// Win+left-drag moves, Win+right-drag resizes. True = swallow the button press.
pub fn drag_begin(p: POINT, resize: bool) -> bool {
    if !win_down()
        || !wm::drag_begin(
            p.x,
            p.y,
            if resize {
                DragKind::Resize
            } else {
                DragKind::Move
            },
        )
    {
        return false;
    }
    DRAG.store(if resize { 2 } else { 1 }, Ordering::Relaxed);
    crate::input_hooks::chord_used();
    true
}
pub fn drag_end(p: POINT, resize: bool) -> bool {
    let want = if resize { 2 } else { 1 };
    if DRAG
        .compare_exchange(want, 0, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return false;
    }
    wm::drag_end(p.x, p.y);
    true
}

/// Workspaces for the top bar (primary monitor), with each window's icon.
pub fn workspaces() -> Vec<crate::topbar::Workspace> {
    wm::bar_state(0)
        .into_iter()
        .map(|w| {
            let mut apps: Vec<(String, Option<crate::topbar::Image>, u32)> = vec![];
            for (id, exe) in w.windows {
                if let Some(a) = apps.iter_mut().find(|a| a.0.eq_ignore_ascii_case(&exe)) {
                    a.2 += 1;
                    continue;
                }
                let icon = crate::appicons::for_window(HWND(id as isize as *mut _));
                apps.push((exe, icon, 1));
            }
            crate::topbar::Workspace {
                name: w.name.to_string(),
                focused: w.focused,
                apps,
            }
        })
        .collect()
}
