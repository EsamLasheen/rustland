//! Low-level keyboard + mouse hooks for the shell. The callbacks only classify input and post a message
//! to the UI thread; they never block, allocate on the hot path, or touch windows (LL hooks have a
//! ~300 ms system budget). Implements the full legacy whkdrc table (63 bindings).
use crate::leftbar::Tab;
use crate::runtime::{Cmd, WM_SHELL_CLICK, WM_SHELL_EDGE};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering};
use windows::Win32::{
    Foundation::*,
    UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

/// dwExtraInfo tag on keystrokes we synthesise ourselves (Alt tap for focus, Win masking).
pub const MARK: usize = 0x4D4F_4E41; // "MONA"

const WIN: u8 = 1;
const SHIFT: u8 = 2;
const ALT: u8 = 4;
const CTRL: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Act {
    Wm(&'static str),
    WmIndexed(&'static str, u8),
    Launch(&'static str),
    Launcher,
    Panel(Tab),
    Capsule(crate::capsule::Tab),
    Help,
    Reload,
    Sleep,
    Capture,
}

pub struct Binding {
    pub mods: u8,
    pub key: u16,
    pub label: &'static str,
    pub act: Act,
}
macro_rules! b {
    ($m:expr, $k:expr, $l:expr, $a:expr) => {
        Binding {
            mods: $m,
            key: $k,
            label: $l,
            act: $a,
        }
    };
}

use crate::capsule::Tab as C;
use Act::*;
pub const BINDINGS: &[Binding] = &[
    // windows
    b!(WIN, 0x51, "Win+Q", Wm("close")),
    b!(WIN, 0x4D, "Win+M", Wm("toggle-maximize")),
    b!(WIN, 0x46, "Win+F", Wm("toggle-float")),
    b!(WIN, 0x4A, "Win+J", Wm("flip-layout")),
    b!(WIN, 0x4E, "Win+N", Wm("minimize")),
    b!(WIN | SHIFT, 0x4E, "Win+Shift+N", Wm("unminimize")),
    // apps
    b!(WIN, 0x0D, "Win+Enter", Launch("wt")),
    b!(WIN, 0x54, "Win+T", Launch("wt")),
    b!(WIN, 0x52, "Win+R", Launcher),
    b!(WIN, 0x4F, "Win+O", Launch("obsidian://open?vault=obsidian")),
    b!(WIN | SHIFT, 0x43, "Win+Shift+C", Launch("code")),
    b!(WIN | SHIFT, 0x45, "Win+Shift+E", Launch("explorer")),
    b!(WIN | SHIFT, 0x42, "Win+Shift+B", Launch("brave")),
    b!(WIN | SHIFT, 0x4B, "Win+Shift+K", Launch("taskmgr")),
    b!(WIN | SHIFT, 0x4D, "Win+Shift+M", Launch("taskmgr")),
    // panels
    b!(WIN, 0x70, "Win+F1", Panel(Tab::Dashboard)),
    b!(WIN, 0x71, "Win+F2", Panel(Tab::Notifications)),
    b!(WIN, 0x72, "Win+F3", Panel(Tab::Weather)),
    b!(WIN, 0x73, "Win+F4", Panel(Tab::Monitoring)),
    b!(WIN, 0x74, "Win+F5", Panel(Tab::Network)),
    b!(WIN, 0x75, "Win+F6", Panel(Tab::Clipboard)),
    b!(WIN, 0x76, "Win+F7", Panel(Tab::Todo)),
    b!(WIN | SHIFT, 0x52, "Win+Shift+R", Panel(Tab::Clipboard)),
    b!(WIN | SHIFT, 0x4C, "Win+Shift+L", Panel(Tab::Power)),
    b!(WIN, 0x53, "Win+S", Panel(Tab::Dashboard)),
    b!(WIN, 0xBF, "Win+/", Help),
    b!(WIN, 0x50, "Win+P", Capsule(C::Media)),
    b!(WIN, 0x57, "Win+W", Capsule(C::Weather)),
    // focus / move / resize
    b!(WIN, 0x25, "Win+Left", Wm("focus left")),
    b!(WIN, 0x27, "Win+Right", Wm("focus right")),
    b!(WIN, 0x26, "Win+Up", Wm("focus up")),
    b!(WIN, 0x28, "Win+Down", Wm("focus down")),
    b!(WIN | SHIFT, 0x25, "Win+Shift+Left", Wm("move left")),
    b!(WIN | SHIFT, 0x27, "Win+Shift+Right", Wm("move right")),
    b!(WIN | SHIFT, 0x26, "Win+Shift+Up", Wm("move up")),
    b!(WIN | SHIFT, 0x28, "Win+Shift+Down", Wm("move down")),
    b!(WIN | ALT, 0x25, "Win+Alt+Left", Wm("resize decrease")),
    b!(WIN | ALT, 0x27, "Win+Alt+Right", Wm("resize increase")),
    // workspaces
    b!(WIN, 0x1B, "Win+Esc", Wm("focus-last-workspace")),
    b!(WIN, 0x48, "Win+H", Wm("special")),
    b!(WIN, 0x31, "Win+1", WmIndexed("focus-workspace", 0)),
    b!(WIN, 0x32, "Win+2", WmIndexed("focus-workspace", 1)),
    b!(WIN, 0x33, "Win+3", WmIndexed("focus-workspace", 2)),
    b!(WIN, 0x34, "Win+4", WmIndexed("focus-workspace", 3)),
    b!(WIN, 0x35, "Win+5", WmIndexed("focus-workspace", 4)),
    b!(WIN, 0x36, "Win+6", WmIndexed("focus-workspace", 5)),
    b!(WIN, 0x37, "Win+7", WmIndexed("focus-workspace", 6)),
    b!(WIN, 0x38, "Win+8", WmIndexed("focus-workspace", 7)),
    b!(WIN, 0x39, "Win+9", WmIndexed("focus-workspace", 8)),
    b!(WIN, 0x30, "Win+0", WmIndexed("focus-workspace", 9)),
    b!(
        WIN | SHIFT,
        0x31,
        "Win+Shift+1",
        WmIndexed("send-to-workspace", 0)
    ),
    b!(
        WIN | SHIFT,
        0x32,
        "Win+Shift+2",
        WmIndexed("send-to-workspace", 1)
    ),
    b!(
        WIN | SHIFT,
        0x33,
        "Win+Shift+3",
        WmIndexed("send-to-workspace", 2)
    ),
    b!(
        WIN | SHIFT,
        0x34,
        "Win+Shift+4",
        WmIndexed("send-to-workspace", 3)
    ),
    b!(
        WIN | SHIFT,
        0x35,
        "Win+Shift+5",
        WmIndexed("send-to-workspace", 4)
    ),
    b!(
        WIN | SHIFT,
        0x36,
        "Win+Shift+6",
        WmIndexed("send-to-workspace", 5)
    ),
    b!(
        WIN | SHIFT,
        0x37,
        "Win+Shift+7",
        WmIndexed("send-to-workspace", 6)
    ),
    b!(
        WIN | SHIFT,
        0x38,
        "Win+Shift+8",
        WmIndexed("send-to-workspace", 7)
    ),
    b!(
        WIN | SHIFT,
        0x39,
        "Win+Shift+9",
        WmIndexed("send-to-workspace", 8)
    ),
    b!(
        WIN | SHIFT,
        0x30,
        "Win+Shift+0",
        WmIndexed("send-to-workspace", 9)
    ),
    // system
    b!(WIN | ALT, 0x52, "Win+Alt+R", Reload),
    b!(WIN | ALT, 0x53, "Win+Alt+S", Sleep),
    b!(WIN | SHIFT, 0x41, "Win+Shift+A", Capture),
];

pub fn find(mods: u8, key: u16) -> Option<&'static Binding> {
    BINDINGS.iter().find(|b| b.mods == mods && b.key == key)
}

pub fn to_cmd(a: Act) -> Cmd {
    match a {
        Wm(s) => Cmd::Wm(s.into()),
        WmIndexed(s, i) => Cmd::Wm(format!("{s} {i}")),
        Launch(s) => Cmd::Launch(s.into()),
        Launcher => Cmd::Launcher,
        Panel(t) => Cmd::Panel(t),
        Capsule(t) => Cmd::Capsule(t),
        Help => Cmd::Help,
        Reload => Cmd::Reload,
        Sleep => Cmd::Sleep,
        Capture => Cmd::Capture,
    }
}

static TARGET: AtomicIsize = AtomicIsize::new(0);
/// Keys we swallowed on key-down: swallow their key-up too.
static HELD: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];
/// A Win chord fired: the Win key-up must not open Start.
static WIN_USED: AtomicBool = AtomicBool::new(false);
/// Currently held modifiers (WIN | SHIFT | ALT | CTRL), left/right merged.
static MODS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

fn modifier_bit(vk: u16) -> u8 {
    match vk {
        0x5B | 0x5C => WIN,
        0x10 | 0xA0 | 0xA1 => SHIFT,
        0x11 | 0xA2 | 0xA3 => CTRL,
        0x12 | 0xA4 | 0xA5 => ALT,
        _ => 0,
    }
}
static EDGE_NEAR: AtomicBool = AtomicBool::new(false);
pub static HOTKEYS_ENABLED: AtomicBool = AtomicBool::new(true);

fn target() -> HWND {
    HWND(TARGET.load(Ordering::Relaxed) as *mut _)
}

static THREAD: AtomicU64 = AtomicU64::new(0);
static JOIN: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> = std::sync::Mutex::new(None);

/// Low-level hooks run on their own thread whose only job is pumping messages. Windows silently
/// removes an LL hook whose thread stalls past LowLevelHooksTimeout (~300 ms), so they must never
/// share the UI thread (startup, window management and painting can take longer than that).
pub fn start(target: HWND) {
    TARGET.store(target.0 as isize, Ordering::Relaxed);
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("input-hooks".into())
        .spawn(move || unsafe {
            use windows::Win32::System::Threading::GetCurrentThreadId;
            THREAD.store(GetCurrentThreadId() as u64, Ordering::Relaxed);
            let module = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
                .ok()
                .map(|m| m.into());
            let kb_hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb), module, 0);
            let ms_hook = SetWindowsHookExW(WH_MOUSE_LL, Some(ms), module, 0);
            let _ = tx.send(format!(
                "input hooks: keyboard {} · mouse {}",
                if kb_hook.is_ok() { "ok" } else { "FAILED" },
                if ms_hook.is_ok() { "ok" } else { "FAILED" }
            ));
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if let Ok(h) = kb_hook {
                let _ = UnhookWindowsHookEx(h);
            }
            if let Ok(h) = ms_hook {
                let _ = UnhookWindowsHookEx(h);
            }
        });
    if let Ok(j) = spawned {
        if let Ok(line) = rx.recv() {
            if let Ok(p) = crate::platform::paths() {
                crate::platform::log(&p, &line);
            }
        }
        *JOIN.lock().unwrap_or_else(|e| e.into_inner()) = Some(j);
    }
}
/// A Win+mouse chord happened: releasing Win must not open Start.
pub fn chord_used() {
    WIN_USED.store(true, Ordering::Relaxed);
}

pub fn stop() {
    let tid = THREAD.swap(0, Ordering::Relaxed) as u32;
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
    if let Some(j) = JOIN.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = j.join();
    }
}
/// Send a harmless masking key so releasing Win after a chord does not open Start.
unsafe fn mask_win() {
    let k = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0xE8),
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: MARK,
            },
        },
    };
    SendInput(&[k(false), k(true)], std::mem::size_of::<INPUT>() as i32);
}

unsafe extern "system" fn kb(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code < 0 {
        return CallNextHookEx(None, code, w, l);
    }
    let d = &*(l.0 as *const KBDLLHOOKSTRUCT);
    if d.dwExtraInfo == MARK || d.vkCode >= 256 {
        return CallNextHookEx(None, code, w, l);
    }
    let vk = d.vkCode as u16;
    let up = matches!(w.0 as u32, WM_KEYUP | WM_SYSKEYUP);
    // modifier state tracked from the event stream (GetAsyncKeyState is unreliable inside LL hooks)
    let m = modifier_bit(vk);
    if m != 0 {
        if up {
            MODS.fetch_and(!m, Ordering::Relaxed);
        } else {
            MODS.fetch_or(m, Ordering::Relaxed);
        }
    }
    let word = &HELD[vk as usize / 64];
    let bit = 1u64 << (vk % 64);
    if up {
        if word.fetch_and(!bit, Ordering::Relaxed) & bit != 0 {
            return LRESULT(1);
        }
        if (vk == VK_LWIN.0 || vk == VK_RWIN.0) && WIN_USED.swap(false, Ordering::Relaxed) {
            mask_win();
        }
        return CallNextHookEx(None, code, w, l);
    }
    if !HOTKEYS_ENABLED.load(Ordering::Relaxed) {
        return CallNextHookEx(None, code, w, l);
    }
    // A modifier released where this hook cannot see it (secure desktop: Win+L, Ctrl+Alt+Del, UAC)
    // would stay "held" and turn plain typing into shortcuts. For modifiers pressed earlier the async
    // state is reliable, so reconcile before using them.
    if m == 0 {
        let held = |a: VIRTUAL_KEY, b: VIRTUAL_KEY| {
            GetAsyncKeyState(a.0 as i32) < 0 || GetAsyncKeyState(b.0 as i32) < 0
        };
        for (bit_m, a, b) in [
            (WIN, VK_LWIN, VK_RWIN),
            (SHIFT, VK_LSHIFT, VK_RSHIFT),
            (CTRL, VK_LCONTROL, VK_RCONTROL),
            (ALT, VK_LMENU, VK_RMENU),
        ] {
            if MODS.load(Ordering::Relaxed) & bit_m != 0 && !held(a, b) {
                MODS.fetch_and(!bit_m, Ordering::Relaxed);
            }
        }
    }
    let mods = MODS.load(Ordering::Relaxed);
    if mods & WIN == 0 {
        // a key typed without Win is never an auto-repeat of a swallowed chord
        word.fetch_and(!bit, Ordering::Relaxed);
        return CallNextHookEx(None, code, w, l);
    }
    // auto-repeat of a swallowed key: keep swallowing, do not re-fire
    if word.load(Ordering::Relaxed) & bit != 0 {
        return LRESULT(1);
    }
    if crate::runtime::debug_enabled() {
        if let Ok(p) = crate::platform::paths() {
            crate::platform::log(
                &p,
                &format!(
                    "key vk={vk:#x} mods={mods} match={}",
                    find(mods, vk).is_some()
                ),
            );
        }
    }
    let Some(b) = find(mods, vk) else {
        return CallNextHookEx(None, code, w, l);
    };
    word.fetch_or(bit, Ordering::Relaxed);
    WIN_USED.store(true, Ordering::Relaxed);
    crate::runtime::post_cmd(to_cmd(b.act));
    LRESULT(1)
}

unsafe extern "system" fn ms(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code >= 0 {
        let d = &*(l.0 as *const MSLLHOOKSTRUCT);
        match w.0 as u32 {
            WM_MOUSEMOVE => {
                // left-edge reveal: x <= 0 shows, beyond the bar + panel gap hides
                let near = EDGE_NEAR.load(Ordering::Relaxed);
                // primary monitor only (it starts at x = 0): a monitor on the left has negative x
                let now = if near {
                    (0..=56).contains(&d.pt.x)
                } else {
                    d.pt.x == 0 && d.pt.y > 32
                };
                if now != near {
                    EDGE_NEAR.store(now, Ordering::Relaxed);
                    let _ = PostMessageW(
                        Some(target()),
                        WM_SHELL_EDGE,
                        WPARAM(now as usize),
                        LPARAM(0),
                    );
                }
                crate::wm_bridge::mouse_move(d.pt);
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                let p = ((d.pt.y as u16 as isize) << 16) | d.pt.x as u16 as isize;
                let _ = PostMessageW(Some(target()), WM_SHELL_CLICK, WPARAM(0), LPARAM(p));
                if crate::wm_bridge::drag_begin(d.pt, w.0 as u32 == WM_RBUTTONDOWN) {
                    return LRESULT(1);
                }
            }
            WM_LBUTTONUP | WM_RBUTTONUP
                if crate::wm_bridge::drag_end(d.pt, w.0 as u32 == WM_RBUTTONUP) =>
            {
                return LRESULT(1);
            }
            _ => {}
        }
    }
    CallNextHookEx(None, code, w, l)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_legacy_table_without_duplicates() {
        assert_eq!(BINDINGS.len(), 63);
        for (i, a) in BINDINGS.iter().enumerate() {
            for b in &BINDINGS[i + 1..] {
                assert!(
                    !(a.mods == b.mods && a.key == b.key),
                    "duplicate {}",
                    a.label
                );
            }
            assert!(a.mods & WIN != 0, "{} must use Win", a.label);
        }
    }
    #[test]
    fn lookup() {
        assert!(matches!(
            find(WIN, 0x31).map(|b| b.act),
            Some(Act::WmIndexed("focus-workspace", 0))
        ));
        assert!(find(WIN, 0x4C).is_none(), "Win+L stays reserved for lock");
        assert!(find(WIN, 0x56).is_none(), "Win+V stays Windows clipboard");
    }
}
