//! Focus glow: a soft accent light around the active window that breathes (or beats), so you always
//! see where you are. Four thin click-through layered windows hug the window's DWM visual bounds
//! (rounded 8 px, reach 9 px: fits the tile gap). The glow bitmap is drawn only when the target is
//! resized or the colour / DPI changes (a move only moves the strips); the pulse changes nothing but
//! the strips' constant alpha (UpdateLayeredWindow, blend only), which DWM composes on the GPU.
//! Modes: breath (sine, 2.8 s) | heartbeat | static | off. The animation timer (~12 fps) only runs
//! while a glow is visible in an animated mode, and pauses while the session is locked or the
//! display is off (WTS session / GUID_CONSOLE_DISPLAY_STATE notifications). Static / off: no timer.
#![allow(dead_code)] // Public API is consumed once the integrator wires the module in.
use crate::render::Color;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;
use windows::core::{w, GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW,
    MonitorFromWindow, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    BLENDFUNCTION, DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    RegisterPowerSettingNotification, UnregisterPowerSettingNotification, HPOWERNOTIFY,
    POWERBROADCAST_SETTING,
};
use windows::Win32::System::Threading::{
    GetCurrentProcessId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Glow reach outside the window (px at 96 dpi).
pub const REACH: i32 = 9;
/// Windows 11 window corner radius (px at 96 dpi).
pub const RADIUS: i32 = 8;
const DEFAULT_COLOR: Color = Color::rgb(0x62, 0xba, 0xf8);
/// ~12 fps opacity pulse: smooth enough for a 2.8 s breath / 1.4 s beat, a third of the wake-ups of 30 fps.
const TICK_MS: u32 = 83;
/// Ticks between safety re-checks of the target (~0.5 s).
const RECHECK_TICKS: u32 = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Breath,
    Heartbeat,
    Static,
    Off,
}

impl Mode {
    /// theme `focusPulse` value; unknown → breath.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "heartbeat" => Mode::Heartbeat,
            "static" => Mode::Static,
            "off" | "none" => Mode::Off,
            _ => Mode::Breath,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Breath => "breath",
            Mode::Heartbeat => "heartbeat",
            Mode::Static => "static",
            Mode::Off => "off",
        }
    }
    fn animated(self) -> bool {
        matches!(self, Mode::Breath | Mode::Heartbeat)
    }
}

/// Pulse curve 0..1 at `t` seconds. Breath: slow sine (2.8 s). Heartbeat: lub-dub, then rest (1.4 s).
pub fn pulse(mode: Mode, t: f32) -> f32 {
    match mode {
        Mode::Heartbeat => {
            let p = t.rem_euclid(1.4);
            let beat = |c: f32, w: f32| (-((p - c) / w).powi(2)).exp();
            (0.38 + 0.62 * beat(0.12, 0.07).max(0.8 * beat(0.36, 0.08))).min(1.0)
        }
        Mode::Static => 1.0,
        Mode::Off => 0.0,
        Mode::Breath => 0.42 + 0.58 * (0.5 - 0.5 * (t * std::f32::consts::TAU / 2.8).cos()),
    }
}

/// Signed distance from (x, y) to the rounded rectangle `r` (negative inside).
pub fn sd_round_rect(x: f32, y: f32, r: &RECT, rad: f32) -> f32 {
    let (cx, cy) = (
        (r.left + r.right) as f32 / 2.0,
        (r.top + r.bottom) as f32 / 2.0,
    );
    let (hw, hh) = (
        (r.right - r.left) as f32 / 2.0 - rad,
        (r.bottom - r.top) as f32 / 2.0 - rad,
    );
    let (qx, qy) = ((x - cx).abs() - hw, (y - cy).abs() - hh);
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - rad
}

/// Glow alpha at signed distance `d`: crisp 1.5 px rim at the edge + quadratic falloff to `reach`.
pub fn glow_alpha(d: f32, reach: f32) -> f32 {
    if d < -1.0 {
        0.0
    } else if d < 1.5 {
        1.0
    } else {
        let t = (1.0 - (d - 1.5) / (reach - 1.5).max(0.5)).clamp(0.0, 1.0);
        t * t * 0.75
    }
}

/// The four strips (top and bottom with the corners, left, right) around window rect `r`.
pub fn strip_rects(r: &RECT, reach: i32, radius: i32) -> [RECT; 4] {
    let o = RECT {
        left: r.left - reach,
        top: r.top - reach,
        right: r.right + reach,
        bottom: r.bottom + reach,
    };
    // the corners belong to the top/bottom strips; tiny windows: keep the side strips non-negative
    let inner_top = (r.top + radius).min((r.top + r.bottom) / 2);
    let inner_bottom = (r.bottom - radius).max(inner_top);
    [
        RECT {
            left: o.left,
            top: o.top,
            right: o.right,
            bottom: inner_top,
        },
        RECT {
            left: o.left,
            top: inner_bottom,
            right: o.right,
            bottom: o.bottom,
        },
        RECT {
            left: o.left,
            top: inner_top,
            right: r.left + 2,
            bottom: inner_bottom,
        },
        RECT {
            left: r.right - 2,
            top: inner_top,
            right: o.right,
            bottom: inner_bottom,
        },
    ]
}

fn scaled(px: i32, dpi: u32) -> i32 {
    (px * dpi as i32 + 48) / 96
}

fn rgb8(c: Color) -> [u8; 3] {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [b(c.r), b(c.g), b(c.b)]
}

// ── window helpers ──────────────────────────────────────────────────────────

fn class_name(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n.min(buf.len())])
}

fn window_pid(h: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
    pid
}

/// Lower-case executable stem of a process ("explorer").
fn process_name(pid: u32) -> Option<String> {
    unsafe {
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let r =
            QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(p);
        r.ok()?;
        let path = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
        let file = path.rsplit(['\\', '/']).next()?.to_lowercase();
        Some(file.strip_suffix(".exe").unwrap_or(&file).to_string())
    }
}

const SKIP_CLASSES: [&str; 14] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "#32768",
    "MonarchGlow",
    "MonarchWallpaper",
    "MonarchNextGlow",
    "MonarchNextWallpaper",
    "Windows.UI.Core.CoreWindow",
    "XamlExplorerHostIslandWindow",
    "ForegroundStaging",
    "MultitaskingViewFrame",
    "TopLevelWindowForOverflowXamlIsland",
];
const SKIP_PROCESSES: [&str; 9] = [
    "zebar",
    "flow.launcher",
    "monarch",
    "monarch-next",
    "searchhost",
    "startmenuexperiencehost",
    "shellexperiencehost",
    "lockapp",
    "lively",
];

/// A window worth glowing around: a visible, real app window (not the shell, a menu, a tooltip).
fn eligible(h: HWND, own_pid: u32) -> bool {
    unsafe {
        if h.is_invalid() || !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return false;
        }
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 || ex & WS_EX_NOACTIVATE.0 != 0 {
            return false;
        }
        if SKIP_CLASSES.contains(&class_name(h).as_str()) {
            return false;
        }
        let pid = window_pid(h);
        if pid == own_pid {
            return false;
        }
        let name = process_name(pid).unwrap_or_default();
        if SKIP_PROCESSES.contains(&name.as_str()) {
            return false;
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(h, &mut r);
        r.right - r.left > 120 && r.bottom - r.top > 80
    }
}

/// DWM visual bounds of a shown, uncloaked, non-maximized, non-fullscreen window.
fn visual_rect(h: HWND) -> Option<RECT> {
    unsafe {
        if h.is_invalid() || !IsWindow(Some(h)).as_bool() {
            return None;
        }
        let mut cloak = 0u32;
        let _ = DwmGetWindowAttribute(
            h,
            DWMWA_CLOAKED,
            &mut cloak as *mut _ as *mut _,
            std::mem::size_of::<u32>() as u32,
        );
        if cloak != 0
            || IsIconic(h).as_bool()
            || IsZoomed(h).as_bool()
            || !IsWindowVisible(h).as_bool()
        {
            return None;
        }
        let mut r = RECT::default();
        DwmGetWindowAttribute(
            h,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            &mut r as *mut _ as *mut _,
            std::mem::size_of::<RECT>() as u32,
        )
        .ok()?;
        // fullscreen (video, games): the glow would only bleed onto the next monitor
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mon = MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST);
        if GetMonitorInfoW(mon, &mut mi).as_bool() && r == mi.rcMonitor {
            return None;
        }
        (r.right > r.left && r.bottom > r.top).then_some(r)
    }
}

// ── glow thread state ───────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
struct Drawn {
    rect: RECT,
    color: [u8; 3],
    dpi: u32,
}

struct Glow {
    host: HWND,
    strips: [HWND; 4],
    target: HWND,
    mode: Mode,
    color: [u8; 3],
    drawn: Option<Drawn>,
    alpha: u8,
    target_hooks: Vec<HWINEVENTHOOK>,
    global_hooks: Vec<HWINEVENTHOOK>,
    animating: bool,
    ticks: u32,
    start: Instant,
    own_pid: u32,
    /// Session locked / switched away (WTS notifications).
    locked: bool,
    /// Console display off (GUID_CONSOLE_DISPLAY_STATE).
    display_off: bool,
    power_note: Option<HPOWERNOTIFY>,
    session_note: bool,
}

/// GUID_CONSOLE_DISPLAY_STATE {6FE69556-704A-47A0-8F24-C28D936FDA47}: 0 off, 1 on, 2 dimmed.
const GUID_CONSOLE_DISPLAY_STATE: GUID = GUID::from_u128(0x6fe69556_704a_47a0_8f24_c28d936fda47);
const NOTIFY_FOR_THIS_SESSION: u32 = 0;
const WTS_CONSOLE_CONNECT: usize = 0x1;
const WTS_CONSOLE_DISCONNECT: usize = 0x2;
const WTS_REMOTE_CONNECT: usize = 0x3;
const WTS_REMOTE_DISCONNECT: usize = 0x4;
const WTS_SESSION_LOCK: usize = 0x7;
const WTS_SESSION_UNLOCK: usize = 0x8;

/// New "locked / away" state for a WM_WTSSESSION_CHANGE code, `None` for codes that do not matter.
pub fn session_paused(code: usize) -> Option<bool> {
    match code {
        WTS_SESSION_LOCK | WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT => Some(true),
        WTS_SESSION_UNLOCK | WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => Some(false),
        _ => None,
    }
}

/// Display state (GUID_CONSOLE_DISPLAY_STATE payload) → display off. Dimmed still shows the glow.
pub fn display_is_off(state: u32) -> bool {
    state == 0
}

/// The pulse timer runs only for a visible glow in an animated mode while someone can see it.
fn wants_timer(mode: Mode, visible: bool, locked: bool, display_off: bool) -> bool {
    mode.animated() && visible && !locked && !display_off
}

unsafe fn register_session_notification(h: HWND) -> bool {
    windows_core::link!("wtsapi32.dll" "system" fn WTSRegisterSessionNotification(hwnd: HWND, dwflags: u32) -> windows_core::BOOL);
    WTSRegisterSessionNotification(h, NOTIFY_FOR_THIS_SESSION).as_bool()
}

unsafe fn unregister_session_notification(h: HWND) {
    windows_core::link!("wtsapi32.dll" "system" fn WTSUnRegisterSessionNotification(hwnd: HWND) -> windows_core::BOOL);
    let _ = WTSUnRegisterSessionNotification(h);
}

thread_local! {
    static GLOW: RefCell<Option<Glow>> = const { RefCell::new(None) };
}

/// Run `f` on the state; skipped when the state is busy (a re-entrant Win32 callback).
fn with_glow(f: impl FnOnce(&mut Glow)) {
    GLOW.with(|g| {
        if let Ok(mut g) = g.try_borrow_mut() {
            if let Some(g) = g.as_mut() {
                f(g);
            }
        }
    });
}

const WM_APP_CMD: u32 = WM_APP + 0x41;
const WM_APP_STOP: u32 = WM_APP + 0x42;
const TIMER_PULSE: usize = 1;

impl Glow {
    fn current_alpha(&self) -> u8 {
        (pulse(self.mode, self.start.elapsed().as_secs_f32()) * 255.0).round() as u8
    }

    fn hide_all(&mut self) {
        for s in self.strips {
            unsafe {
                let _ = ShowWindow(s, SW_HIDE);
            }
        }
        self.drawn = None;
        self.update_timer();
    }

    /// The pulse timer runs only while a glow is visible in an animated mode, the session is
    /// unlocked and the display is on.
    fn update_timer(&mut self) {
        let want = wants_timer(
            self.mode,
            self.drawn.is_some(),
            self.locked,
            self.display_off,
        );
        if want != self.animating {
            unsafe {
                if want {
                    SetTimer(Some(self.host), TIMER_PULSE, TICK_MS, None);
                } else {
                    let _ = KillTimer(Some(self.host), TIMER_PULSE);
                }
            }
            self.animating = want;
        }
    }

    /// Re-draw if the target moved, resized, or the colour / DPI changed.
    fn refresh(&mut self, force: bool) {
        if self.mode == Mode::Off {
            self.hide_all();
            return;
        }
        let Some(r) = visual_rect(self.target) else {
            self.hide_all();
            return;
        };
        let dpi = match unsafe { GetDpiForWindow(self.target) } {
            0 => 96,
            d => d,
        };
        let next = Drawn {
            rect: r,
            color: self.color,
            dpi,
        };
        if !force && self.drawn == Some(next) {
            return;
        }
        let (reach, rad) = (scaled(REACH, dpi), scaled(RADIUS, dpi));
        let parts = strip_rects(&r, reach, rad);
        let same_size = |d: &Drawn| {
            d.color == next.color
                && d.dpi == next.dpi
                && d.rect.right - d.rect.left == r.right - r.left
                && d.rect.bottom - d.rect.top == r.bottom - r.top
        };
        if !force && self.drawn.as_ref().map(same_size).unwrap_or(false) {
            // moved only: the bitmaps stay, the strips move
            unsafe {
                if let Ok(mut dwp) = BeginDeferWindowPos(4) {
                    for (h, s) in self.strips.iter().zip(parts) {
                        match DeferWindowPos(
                            dwp,
                            *h,
                            None,
                            s.left,
                            s.top,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                        ) {
                            Ok(next) => dwp = next,
                            Err(_) => break,
                        }
                    }
                    let _ = EndDeferWindowPos(dwp);
                }
            }
            self.drawn = Some(next);
            return;
        }
        self.alpha = if self.mode.animated() {
            self.current_alpha()
        } else {
            255
        };
        for (h, s) in self.strips.iter().zip(parts) {
            unsafe { paint_strip(*h, s, &r, self.color, reach as f32, rad as f32, self.alpha) };
        }
        self.drawn = Some(next);
        self.update_timer();
    }

    fn unhook_target(&mut self) {
        for h in self.target_hooks.drain(..) {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
    }

    /// Follow `h`; location / show / hide / destroy events come from its process only.
    fn retarget(&mut self, h: HWND) {
        self.target = h;
        self.unhook_target();
        let pid = window_pid(h);
        if pid != 0 {
            for (a, b) in [
                (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
                (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
            ] {
                let hk = unsafe {
                    SetWinEventHook(a, b, None, Some(on_event), pid, 0, WINEVENT_OUTOFCONTEXT)
                };
                if !hk.is_invalid() {
                    self.target_hooks.push(hk);
                }
            }
        }
        self.refresh(true);
    }

    fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        // every ~0.5 s: catch anything the events missed (e.g. tiling WM workspace switches)
        if self.ticks.is_multiple_of(RECHECK_TICKS) {
            self.refresh(false);
        }
        if self.drawn.is_none() || !self.mode.animated() {
            return;
        }
        let a = self.current_alpha();
        if a == self.alpha {
            return;
        }
        self.alpha = a;
        let blend = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: a,
            AlphaFormat: 1,
        };
        for s in self.strips {
            // blend-only update: the bitmap stays, DWM re-composes it with a new opacity
            unsafe {
                let _ = UpdateLayeredWindow(
                    s,
                    None,
                    None,
                    None,
                    None,
                    None,
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
            }
        }
    }

    /// Lock / unlock or display off / on: stop the pulse while nobody can see it, resume after
    /// (re-checking the target, which may have changed meanwhile).
    fn set_paused(&mut self, locked: Option<bool>, display_off: Option<bool>) {
        let was = self.locked || self.display_off;
        self.locked = locked.unwrap_or(self.locked);
        self.display_off = display_off.unwrap_or(self.display_off);
        let now = self.locked || self.display_off;
        if was && !now {
            self.refresh(false);
        }
        self.update_timer();
    }

    fn teardown(&mut self) {
        unsafe {
            if let Some(n) = self.power_note.take() {
                let _ = UnregisterPowerSettingNotification(n);
            }
            if std::mem::take(&mut self.session_note) {
                unregister_session_notification(self.host);
            }
        }
        self.unhook_target();
        for h in self.global_hooks.drain(..) {
            unsafe {
                let _ = UnhookWinEvent(h);
            }
        }
        unsafe {
            let _ = KillTimer(Some(self.host), TIMER_PULSE);
            for s in self.strips {
                let _ = DestroyWindow(s);
            }
        }
        self.animating = false;
        self.drawn = None;
    }
}

/// Paint one strip (screen rect `s`) of the glow around `r` and push it to its layered window.
unsafe fn paint_strip(h: HWND, s: RECT, r: &RECT, color: [u8; 3], reach: f32, rad: f32, alpha: u8) {
    let (w, ht) = (s.right - s.left, s.bottom - s.top);
    if w <= 0 || ht <= 0 {
        let _ = ShowWindow(h, SW_HIDE);
        return;
    }
    let screen = GetDC(None);
    let mem = CreateCompatibleDC(Some(screen));
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -ht,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let dib = match CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
        Ok(d) if !bits.is_null() => d,
        _ => {
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            return;
        }
    };
    let old = SelectObject(mem, dib.into());
    let px = std::slice::from_raw_parts_mut(bits as *mut u8, (w * ht * 4) as usize);
    for y in 0..ht {
        for x in 0..w {
            let d = sd_round_rect((s.left + x) as f32 + 0.5, (s.top + y) as f32 + 0.5, r, rad);
            let a = glow_alpha(d, reach);
            let i = ((y * w + x) * 4) as usize;
            // premultiplied BGRA
            px[i] = (color[2] as f32 * a) as u8;
            px[i + 1] = (color[1] as f32 * a) as u8;
            px[i + 2] = (color[0] as f32 * a) as u8;
            px[i + 3] = (a * 255.0) as u8;
        }
    }
    let pos = POINT {
        x: s.left,
        y: s.top,
    };
    let size = SIZE { cx: w, cy: ht };
    let src = POINT::default();
    let blend = BLENDFUNCTION {
        BlendOp: 0,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: 1, // AC_SRC_ALPHA
    };
    let _ = UpdateLayeredWindow(
        h,
        Some(screen),
        Some(&pos),
        Some(&size),
        Some(mem),
        Some(&src),
        COLORREF(0),
        Some(&blend),
        ULW_ALPHA,
    );
    let _ = SetWindowPos(
        h,
        Some(HWND_TOPMOST),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    SelectObject(mem, old);
    let _ = DeleteObject(dib.into());
    let _ = DeleteDC(mem);
    ReleaseDC(None, screen);
}

unsafe extern "system" fn on_event(
    _: HWINEVENTHOOK,
    ev: u32,
    h: HWND,
    obj: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if obj != OBJID_WINDOW.0 {
        return;
    }
    with_glow(|g| match ev {
        EVENT_SYSTEM_FOREGROUND => {
            if h != g.target && eligible(h, g.own_pid) {
                g.retarget(h);
            }
        }
        EVENT_OBJECT_LOCATIONCHANGE | EVENT_SYSTEM_MOVESIZEEND | EVENT_SYSTEM_MINIMIZEEND
            if h == g.target =>
        {
            g.refresh(false)
        }
        EVENT_SYSTEM_MINIMIZESTART | EVENT_OBJECT_HIDE | EVENT_OBJECT_CLOAKED if h == g.target => {
            g.hide_all()
        }
        EVENT_OBJECT_DESTROY if h == g.target => {
            g.hide_all();
            g.unhook_target();
            g.target = HWND::default();
        }
        EVENT_OBJECT_UNCLOAKED | EVENT_OBJECT_SHOW if h == g.target => g.refresh(true),
        _ => {}
    });
}

enum Cmd {
    Mode(Mode),
    Color(Color),
    Retarget(isize),
}

thread_local! {
    static RX: RefCell<Option<Receiver<Cmd>>> = const { RefCell::new(None) };
}

fn drain_commands() {
    let cmds: Vec<Cmd> = RX.with(|r| {
        r.borrow()
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    });
    for c in cmds {
        with_glow(|g| match c {
            Cmd::Mode(m) => {
                if m != g.mode {
                    g.mode = m;
                    g.refresh(true);
                    g.update_timer();
                }
            }
            Cmd::Color(c) => {
                let c = rgb8(c);
                if c != g.color {
                    g.color = c;
                    g.refresh(false);
                }
            }
            Cmd::Retarget(h) => {
                let h = HWND(h as *mut _);
                if unsafe { IsWindow(Some(h)).as_bool() } {
                    g.retarget(h);
                }
            }
        });
    }
}

unsafe extern "system" fn host_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if wp.0 == TIMER_PULSE => {
            with_glow(Glow::tick);
            LRESULT(0)
        }
        WM_APP_CMD => {
            drain_commands();
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE => {
            if let Some(p) = session_paused(wp.0) {
                with_glow(|g| g.set_paused(Some(p), None));
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            if wp.0 == PBT_POWERSETTINGCHANGE as usize && lp.0 != 0 {
                let s = &*(lp.0 as *const POWERBROADCAST_SETTING);
                if s.PowerSetting == GUID_CONSOLE_DISPLAY_STATE && s.DataLength >= 4 {
                    let state = std::ptr::read_unaligned(s.Data.as_ptr() as *const u32);
                    with_glow(|g| g.set_paused(None, Some(display_is_off(state))));
                }
            }
            LRESULT(1)
        }
        WM_APP_STOP => {
            with_glow(Glow::teardown);
            let _ = DestroyWindow(h);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

unsafe extern "system" fn strip_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

const HOST_CLASS: PCWSTR = w!("MonarchNextGlowHost");
const STRIP_CLASS: PCWSTR = w!("MonarchNextGlow");

fn glow_thread(
    mode: Mode,
    color: Color,
    rx: Receiver<Cmd>,
    host_out: Arc<AtomicIsize>,
    stopping: Arc<AtomicBool>,
) {
    RX.with(|r| *r.borrow_mut() = Some(rx));
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let hinst = HINSTANCE(module.0);
        for (class, proc_) in [
            (
                HOST_CLASS,
                host_proc as unsafe extern "system" fn(_, _, _, _) -> _,
            ),
            (STRIP_CLASS, strip_proc),
        ] {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc_),
                hInstance: hinst,
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc); // fails harmlessly when already registered (restart)
        }
        let Ok(host) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            HOST_CLASS,
            PCWSTR::null(),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst),
            None,
        ) else {
            return;
        };
        let mut strips = [HWND::default(); 4];
        for s in strips.iter_mut() {
            let ex = WS_EX_LAYERED
                | WS_EX_TRANSPARENT
                | WS_EX_TOOLWINDOW
                | WS_EX_NOACTIVATE
                | WS_EX_TOPMOST;
            if let Ok(h) = CreateWindowExW(
                ex,
                STRIP_CLASS,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(hinst),
                None,
            ) {
                *s = h;
            }
        }
        let mut global_hooks = Vec::new();
        for (a, b) in [
            (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
            (EVENT_SYSTEM_MOVESIZEEND, EVENT_SYSTEM_MOVESIZEEND),
            (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
            (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
        ] {
            let hk = SetWinEventHook(
                a,
                b,
                None,
                Some(on_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            );
            if !hk.is_invalid() {
                global_hooks.push(hk);
            }
        }
        let own_pid = GetCurrentProcessId();
        // lock / display-off pause the pulse (both are sent straight to the message-only host)
        let session_note = register_session_notification(host);
        let power_note = RegisterPowerSettingNotification(
            HANDLE(host.0),
            &GUID_CONSOLE_DISPLAY_STATE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
        .ok();
        GLOW.with(|g| {
            *g.borrow_mut() = Some(Glow {
                host,
                strips,
                target: HWND::default(),
                mode,
                color: rgb8(color),
                drawn: None,
                alpha: 255,
                target_hooks: Vec::new(),
                global_hooks,
                animating: false,
                ticks: 0,
                start: Instant::now(),
                own_pid,
                locked: false,
                display_off: false,
                power_note,
                session_note,
            })
        });
        host_out.store(host.0 as isize, Ordering::SeqCst);
        if stopping.load(Ordering::SeqCst) {
            let _ = PostMessageW(Some(host), WM_APP_STOP, WPARAM(0), LPARAM(0));
        } else {
            drain_commands();
            let fg = GetForegroundWindow();
            with_glow(|g| {
                if g.target.is_invalid() && eligible(fg, g.own_pid) {
                    g.retarget(fg);
                }
            });
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    host_out.store(0, Ordering::SeqCst);
    GLOW.with(|g| g.borrow_mut().take());
    RX.with(|r| r.borrow_mut().take());
}

/// Running focus glow. Calls never block, except [`Handle::stop`] (joins the thread).
pub struct Handle {
    tx: Sender<Cmd>,
    host: Arc<AtomicIsize>,
    stopping: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Start the glow (breath, default accent) on its own UI thread.
pub fn start() -> Handle {
    start_with(Mode::Breath, DEFAULT_COLOR)
}

pub fn start_with(mode: Mode, color: Color) -> Handle {
    let (tx, rx) = mpsc::channel();
    let host = Arc::new(AtomicIsize::new(0));
    let stopping = Arc::new(AtomicBool::new(false));
    let (h2, s2) = (host.clone(), stopping.clone());
    let thread = std::thread::Builder::new()
        .name("focus-glow".into())
        .spawn(move || glow_thread(mode, color, rx, h2, s2))
        .ok();
    Handle {
        tx,
        host,
        stopping,
        thread,
    }
}

impl Handle {
    fn send(&self, c: Cmd) {
        if self.tx.send(c).is_ok() {
            let h = self.host.load(Ordering::SeqCst);
            if h != 0 {
                unsafe {
                    let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_CMD, WPARAM(0), LPARAM(0));
                }
            }
        }
    }
    pub fn set_mode(&self, mode: Mode) {
        self.send(Cmd::Mode(mode));
    }
    pub fn set_color(&self, color: Color) {
        self.send(Cmd::Color(color));
    }
    /// Glow around `hwnd` now (bypasses the eligibility filter, e.g. after the shell focused it).
    pub fn retarget(&self, hwnd: HWND) {
        self.send(Cmd::Retarget(hwnd.0 as isize));
    }
    /// Hide and destroy the strips, unhook, end the thread.
    pub fn stop(mut self) {
        self.shutdown();
    }
    fn shutdown(&mut self) {
        let Some(t) = self.thread.take() else { return };
        self.stopping.store(true, Ordering::SeqCst);
        let h = self.host.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_STOP, WPARAM(0), LPARAM(0));
            }
        }
        let _ = t.join();
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_round_trip() {
        for m in [Mode::Breath, Mode::Heartbeat, Mode::Static, Mode::Off] {
            assert_eq!(Mode::parse(m.as_str()), m);
        }
        assert_eq!(Mode::parse("bogus"), Mode::Breath);
        assert_eq!(Mode::parse(" HEARTBEAT "), Mode::Heartbeat);
    }

    #[test]
    fn pulse_ranges() {
        for i in 0..400 {
            let t = i as f32 * 0.01;
            for m in [Mode::Breath, Mode::Heartbeat] {
                let p = pulse(m, t);
                assert!((0.37..=1.0).contains(&p), "{m:?} t={t} p={p}");
            }
        }
        assert!((pulse(Mode::Breath, 0.0) - 0.42).abs() < 1e-4);
        assert!((pulse(Mode::Breath, 1.4) - 1.0).abs() < 1e-4);
        assert!((pulse(Mode::Breath, 2.8) - 0.42).abs() < 1e-3);
        assert!(pulse(Mode::Heartbeat, 0.12) > 0.95);
        assert_eq!(pulse(Mode::Static, 3.0), 1.0);
        assert_eq!(pulse(Mode::Off, 3.0), 0.0);
    }

    #[test]
    fn pulse_timer_only_when_seen() {
        // ~12 fps, re-check every ~0.5 s
        assert!((80..=90).contains(&TICK_MS));
        assert!((450..=550).contains(&(TICK_MS * RECHECK_TICKS)));
        for m in [Mode::Breath, Mode::Heartbeat] {
            assert!(wants_timer(m, true, false, false));
            assert!(!wants_timer(m, false, false, false)); // nothing drawn
            assert!(!wants_timer(m, true, true, false)); // locked
            assert!(!wants_timer(m, true, false, true)); // display off
        }
        for m in [Mode::Static, Mode::Off] {
            assert!(!wants_timer(m, true, false, false)); // static / off never tick
        }
        assert_eq!(session_paused(WTS_SESSION_LOCK), Some(true));
        assert_eq!(session_paused(WTS_SESSION_UNLOCK), Some(false));
        assert_eq!(session_paused(WTS_CONSOLE_DISCONNECT), Some(true));
        assert_eq!(session_paused(WTS_CONSOLE_CONNECT), Some(false));
        assert_eq!(session_paused(0x5), None); // logon
        assert!(display_is_off(0));
        assert!(!display_is_off(1));
        assert!(!display_is_off(2)); // dimmed
    }

    #[test]
    fn distance_and_alpha() {
        let r = RECT {
            left: 0,
            top: 0,
            right: 100,
            bottom: 50,
        };
        assert!((sd_round_rect(50.0, -5.0, &r, 8.0) - 5.0).abs() < 1e-4);
        assert!(sd_round_rect(50.0, 25.0, &r, 8.0) < -20.0);
        // corner is rounded: the bounding-box corner is outside by (√2−1)·r
        assert!((sd_round_rect(0.0, 0.0, &r, 8.0) - 8.0 * (2f32.sqrt() - 1.0)).abs() < 1e-3);
        assert_eq!(glow_alpha(-5.0, 9.0), 0.0);
        assert_eq!(glow_alpha(0.0, 9.0), 1.0);
        assert!(glow_alpha(4.0, 9.0) > 0.0 && glow_alpha(4.0, 9.0) < 0.75);
        assert_eq!(glow_alpha(9.0, 9.0), 0.0);
    }

    #[test]
    fn strips_cover_the_ring() {
        let r = RECT {
            left: 100,
            top: 100,
            right: 500,
            bottom: 400,
        };
        let s = strip_rects(&r, 9, 8);
        assert_eq!(
            (s[0].left, s[0].top, s[0].right, s[0].bottom),
            (91, 91, 509, 108)
        );
        assert_eq!(
            (s[1].left, s[1].top, s[1].right, s[1].bottom),
            (91, 392, 509, 409)
        );
        assert_eq!(
            (s[2].left, s[2].top, s[2].right, s[2].bottom),
            (91, 108, 102, 392)
        );
        assert_eq!(
            (s[3].left, s[3].top, s[3].right, s[3].bottom),
            (498, 108, 509, 392)
        );
        // degenerate window: side strips never get a negative height
        let tiny = RECT {
            left: 0,
            top: 0,
            right: 10,
            bottom: 6,
        };
        for p in strip_rects(&tiny, 9, 8) {
            assert!(p.bottom >= p.top && p.right >= p.left);
        }
        assert_eq!(scaled(REACH, 144), 14);
        assert_eq!(scaled(RADIUS, 96), 8);
    }
}
