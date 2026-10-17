//! Native notification-area (system tray) host.
//!
//! Technique (same as Zebar's `systray-util`): a hidden, top-most top-level
//! window of class `Shell_TrayWnd` is created on a dedicated thread. Because
//! `Shell_NotifyIcon` locates the tray with `FindWindow("Shell_TrayWnd")`, the
//! first window of that class in z-order receives every `WM_COPYDATA`
//! (`dwData == 1`) notification. We parse it into an icon table and then
//! forward the message unchanged to Explorer's real `Shell_TrayWnd`, returning
//! its result, so Explorer's own tray keeps working.
//!
//! The host is purely message driven: no periodic timers. Z-order is
//! re-asserted on `TaskbarCreated`, display/setting changes and foreground
//! changes (WinEvent hook, plus a one-shot debounce timer after each of them).
#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, DeleteDC, DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, ChangeWindowMessageFilterEx, CopyIcon, CreateWindowExW,
    DefWindowProcW, DestroyIcon, DestroyWindow, DispatchMessageW, FindWindowExW, FindWindowW,
    GetIconInfo, GetMessageW, GetWindowThreadProcessId, InSendMessage, IsWindow, KillTimer,
    PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SendMessageTimeoutW,
    SendNotifyMessageW, SetTimer, SetWindowPos, TranslateMessage, UnregisterClassW,
    EVENT_SYSTEM_FOREGROUND, HICON, HWND_BROADCAST, HWND_TOPMOST, ICONINFO, MSG, MSGFLT_ALLOW,
    SMTO_ABORTIFHUNG, SMTO_NORMAL, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
    WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_CLOSE, WM_COMMAND, WM_CONTEXTMENU,
    WM_COPYDATA, WM_DESTROY, WM_DISPLAYCHANGE, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_SYSCOMMAND,
    WM_TIMER, WM_USER, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

pub const MAX_ICONS: usize = 64;

/// Edge length of the RGBA image delivered for every icon.
const ICON_PX: u32 = 32;

#[derive(Clone, Debug, PartialEq)]
pub struct TrayIconData {
    /// Stable id: `"{hwnd:x}:{uid}"`, or the `{GUID}` when the icon uses `NIF_GUID`.
    pub id: String,
    pub tooltip: String,
    /// Straight (non-premultiplied) RGBA, `w * h * 4` bytes.
    pub rgba: Vec<u8>,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    Left,
    Right,
    Double,
    Middle,
}

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

const TRAY_SIGNATURE: u32 = 0x3475_3423;
const COPYDATA_TRAY: usize = 1;

const NIM_ADD: u32 = 0;
const NIM_MODIFY: u32 = 1;
const NIM_DELETE: u32 = 2;
const NIM_SETFOCUS: u32 = 3;
const NIM_SETVERSION: u32 = 4;

const NIF_MESSAGE: u32 = 0x01;
const NIF_ICON: u32 = 0x02;
const NIF_TIP: u32 = 0x04;
const NIF_STATE: u32 = 0x08;
const NIF_GUID: u32 = 0x20;

const NIS_HIDDEN: u32 = 0x01;

const NIN_SELECT: u32 = WM_USER;
const NOTIFYICON_VERSION_3: u32 = 3;
const NOTIFYICON_VERSION_4: u32 = 4;

/// Upper bound on accepted `cbData` (the real payload is < 1 KiB).
const MAX_COPYDATA: usize = 64 * 1024;

// ---------------------------------------------------------------------------
// Pure parsing (unit tested)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    /// Handles stored as 32-bit values (what shell32 marshals; also 32-bit senders).
    X86,
    /// Native 64-bit `NOTIFYICONDATAW` with 8-byte handles and padding.
    X64,
}

/// Field offsets inside `NOTIFYICONDATAW` (relative to the struct start).
struct Offsets {
    hwnd: usize,
    uid: usize,
    flags: usize,
    callback: usize,
    hicon: usize,
    tip: usize,
    state: usize,
    state_mask: usize,
    version: usize,
    guid: usize,
}

const X86_OFFSETS: Offsets = Offsets {
    hwnd: 4,
    uid: 8,
    flags: 12,
    callback: 16,
    hicon: 20,
    tip: 24,
    state: 280,
    state_mask: 284,
    version: 800,
    guid: 936,
};
const X64_OFFSETS: Offsets = Offsets {
    hwnd: 8,
    uid: 16,
    flags: 20,
    callback: 24,
    hicon: 32,
    tip: 40,
    state: 296,
    state_mask: 300,
    version: 816,
    guid: 952,
};
/// Full struct sizes for each layout.
const X86_SIZE: usize = 956;
const X64_SIZE: usize = 976;

/// A decoded `SHELLTRAYDATA` message.
#[derive(Clone, Debug, Default, PartialEq)]
struct TrayMsg {
    message: u32,
    hwnd: isize,
    uid: u32,
    flags: u32,
    callback: u32,
    hicon: isize,
    tip: String,
    state: u32,
    state_mask: u32,
    version: u32,
    guid: Option<[u8; 16]>,
}

fn rd_u32(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off.checked_add(4)?)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn rd_u64(b: &[u8], off: usize) -> Option<u64> {
    let s = b.get(off..off.checked_add(8)?)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Some(u64::from_le_bytes(a))
}

/// Reads a NUL-terminated UTF-16 string of at most `max_chars`, clipped to the buffer.
fn rd_wstr(b: &[u8], off: usize, max_chars: usize) -> String {
    let Some(rest) = b.get(off..) else {
        return String::new();
    };
    let units: Vec<u16> = rest
        .as_chunks::<2>()
        .0
        .iter()
        .take(max_chars)
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Picks the struct layout from the payload length and the padding after `cbSize`.
fn detect_layout(nid: &[u8]) -> Layout {
    let pad_zero = rd_u32(nid, 4) == Some(0);
    if nid.len() >= X64_SIZE && pad_zero {
        return Layout::X64;
    }
    if nid.len() >= X86_SIZE {
        return Layout::X86;
    }
    // Truncated / older struct versions: in the 64-bit layout bytes 4..8 are
    // padding and the window handle follows at 8.
    if pad_zero && rd_u64(nid, 8).is_some_and(|h| h != 0) {
        Layout::X64
    } else {
        Layout::X86
    }
}

/// Parses the `WM_COPYDATA` payload (`dwData == 1`). Never reads out of bounds.
fn parse_tray_data(buf: &[u8]) -> Option<TrayMsg> {
    if rd_u32(buf, 0)? != TRAY_SIGNATURE {
        return None;
    }
    let message = rd_u32(buf, 4)?;
    let nid = buf.get(8..)?;
    let layout = detect_layout(nid);
    let (o, hwnd, hicon) = match layout {
        Layout::X86 => {
            let o = &X86_OFFSETS;
            // 32-bit handles are sign-extended, like LongToHandle.
            let h = rd_u32(nid, o.hwnd)? as i32 as isize;
            let i = rd_u32(nid, o.hicon)? as i32 as isize;
            (o, h, i)
        }
        Layout::X64 => {
            let o = &X64_OFFSETS;
            (
                o,
                rd_u64(nid, o.hwnd)? as isize,
                rd_u64(nid, o.hicon)? as isize,
            )
        }
    };
    let flags = rd_u32(nid, o.flags)?;
    // NOTIFYICONDATA v1 has a 64-char tip and nothing after it.
    let tip_chars = if nid.len() < o.state { 64 } else { 128 };
    let guid = if flags & NIF_GUID != 0 {
        nid.get(o.guid..o.guid + 16).map(|g| {
            let mut a = [0u8; 16];
            a.copy_from_slice(g);
            a
        })
    } else {
        None
    };
    Some(TrayMsg {
        message,
        hwnd,
        uid: rd_u32(nid, o.uid)?,
        flags,
        callback: rd_u32(nid, o.callback)?,
        hicon,
        tip: if flags & NIF_TIP != 0 {
            rd_wstr(nid, o.tip, tip_chars)
        } else {
            String::new()
        },
        state: rd_u32(nid, o.state).unwrap_or(0),
        state_mask: rd_u32(nid, o.state_mask).unwrap_or(0),
        version: rd_u32(nid, o.version).unwrap_or(0),
        guid,
    })
}

fn format_guid(g: &[u8; 16]) -> String {
    let d1 = u32::from_le_bytes([g[0], g[1], g[2], g[3]]);
    let d2 = u16::from_le_bytes([g[4], g[5]]);
    let d3 = u16::from_le_bytes([g[6], g[7]]);
    format!(
        "{{{d1:08X}-{d2:04X}-{d3:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g[8], g[9], g[10], g[11], g[12], g[13], g[14], g[15]
    )
}

fn format_id(hwnd: isize, uid: u32) -> String {
    format!("{:x}:{}", hwnd as usize, uid)
}

// ---------------------------------------------------------------------------
// Click message composition (unit tested)
// ---------------------------------------------------------------------------

fn make_long(lo: u32, hi: u32) -> u32 {
    (lo & 0xFFFF) | ((hi & 0xFFFF) << 16)
}

/// Returns `(wParam, lParam)` pairs to post with the icon's callback message.
fn compose_click(version: u32, uid: u32, click: Click, x: i32, y: i32) -> Vec<(usize, isize)> {
    let events: &[u32] = match (click, version >= NOTIFYICON_VERSION_3) {
        (Click::Left, true) => &[WM_LBUTTONDOWN, WM_LBUTTONUP, NIN_SELECT],
        (Click::Left, false) => &[WM_LBUTTONDOWN, WM_LBUTTONUP],
        (Click::Right, true) => &[WM_RBUTTONDOWN, WM_RBUTTONUP, WM_CONTEXTMENU],
        (Click::Right, false) => &[WM_RBUTTONDOWN, WM_RBUTTONUP],
        (Click::Double, _) => &[WM_LBUTTONDBLCLK],
        (Click::Middle, _) => &[WM_MBUTTONDOWN, WM_MBUTTONUP],
    };
    events
        .iter()
        .map(|&ev| {
            if version >= NOTIFYICON_VERSION_4 {
                (
                    make_long(x as u32, y as u32) as usize,
                    make_long(ev, uid) as i32 as isize,
                )
            } else {
                (uid as usize, ev as isize)
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Icon table (unit tested)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum Key {
    Guid([u8; 16]),
    Win(isize, u32),
}

#[derive(Clone, Debug)]
struct Entry {
    key: Key,
    hwnd: isize,
    uid: u32,
    callback: u32,
    version: u32,
    tip: String,
    rgba: Vec<u8>,
    hidden: bool,
}

impl Entry {
    fn id(&self) -> String {
        match &self.key {
            Key::Guid(g) => format_guid(g),
            Key::Win(h, u) => format_id(*h, *u),
        }
    }
}

#[derive(Default, Debug)]
struct Table {
    entries: Vec<Entry>,
}

impl Table {
    fn key_of(m: &TrayMsg) -> Key {
        match m.guid {
            Some(g) => Key::Guid(g),
            None => Key::Win(m.hwnd, m.uid),
        }
    }

    fn update_fields(e: &mut Entry, m: &TrayMsg, rgba: Option<Vec<u8>>) {
        if m.hwnd != 0 {
            e.hwnd = m.hwnd;
            e.uid = m.uid;
        }
        if m.flags & NIF_MESSAGE != 0 {
            e.callback = m.callback;
        }
        if m.flags & NIF_TIP != 0 {
            e.tip.clone_from(&m.tip);
        }
        if let Some(px) = rgba {
            e.rgba = px;
        }
        if m.flags & NIF_STATE != 0 && m.state_mask & NIS_HIDDEN != 0 {
            e.hidden = m.state & NIS_HIDDEN != 0;
        }
    }

    /// Applies a tray message; `rgba` is the already converted icon (if any).
    /// Returns whether the visible list may have changed.
    fn apply(&mut self, m: &TrayMsg, rgba: Option<Vec<u8>>) -> bool {
        let key = Self::key_of(m);
        let pos = self.entries.iter().position(|e| e.key == key);
        match (m.message, pos) {
            (NIM_ADD | NIM_MODIFY, Some(i)) => {
                Self::update_fields(&mut self.entries[i], m, rgba);
                true
            }
            // A MODIFY for an unknown icon (added before we started) is adopted.
            (NIM_ADD | NIM_MODIFY, None) => {
                if self.entries.len() >= MAX_ICONS {
                    return false;
                }
                let mut e = Entry {
                    key,
                    hwnd: m.hwnd,
                    uid: m.uid,
                    callback: 0,
                    version: 0,
                    tip: String::new(),
                    rgba: Vec::new(),
                    hidden: false,
                };
                Self::update_fields(&mut e, m, rgba);
                self.entries.push(e);
                true
            }
            (NIM_DELETE, Some(i)) => {
                self.entries.remove(i);
                true
            }
            (NIM_SETVERSION, Some(i)) => {
                self.entries[i].version = m.version;
                false
            }
            _ => false,
        }
    }

    /// Removes entries whose owner window is gone. Returns whether anything was removed.
    fn prune(&mut self, alive: impl Fn(isize) -> bool) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| alive(e.hwnd));
        self.entries.len() != before
    }

    fn visible(&self) -> Vec<TrayIconData> {
        self.entries
            .iter()
            .filter(|e| !e.hidden && !e.rgba.is_empty())
            .map(|e| TrayIconData {
                id: e.id(),
                tooltip: e.tip.clone(),
                rgba: e.rgba.clone(),
                w: ICON_PX,
                h: ICON_PX,
            })
            .collect()
    }

    fn find(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id() == id)
    }
}

// ---------------------------------------------------------------------------
// Pixel helpers (unit tested)
// ---------------------------------------------------------------------------

/// Converts top-down BGRA pixels (+ optional AND mask, 1 = transparent) to straight RGBA.
fn bgra_to_rgba(color: &[u8], mask: Option<&[bool]>) -> Vec<u8> {
    let px = color.as_chunks::<4>().0;
    let has_alpha = px.iter().any(|p| p[3] != 0);
    let mut out = Vec::with_capacity(color.len());
    for (i, p) in px.iter().enumerate() {
        let a = if has_alpha {
            p[3]
        } else {
            match mask.and_then(|m| m.get(i)) {
                Some(true) => 0,
                _ => 255,
            }
        };
        out.extend_from_slice(&[p[2], p[1], p[0], a]);
    }
    out
}

/// Monochrome icon: `and`/`xor` are per-pixel bits (true = 1). Returns RGBA.
fn mono_to_rgba(and: &[bool], xor: &[bool]) -> Vec<u8> {
    let mut out = Vec::with_capacity(and.len() * 4);
    for (&a, &x) in and.iter().zip(xor) {
        let px = match (a, x) {
            (false, false) => [0, 0, 0, 255],
            (false, true) => [255, 255, 255, 255],
            (true, false) => [0, 0, 0, 0],
            // "Invert screen" pixels: render as opaque white.
            (true, true) => [255, 255, 255, 255],
        };
        out.extend_from_slice(&px);
    }
    out
}

/// Box-filter resample of straight RGBA (alpha-weighted to avoid dark fringes).
fn scale_rgba(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let mut out = vec![0u8; dw * dh * 4];
    if sw == 0 || sh == 0 || src.len() < sw * sh * 4 {
        return out;
    }
    for dy in 0..dh {
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh).div_ceil(dh).max(y0 + 1).min(sh);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw).div_ceil(dw).max(x0 + 1).min(sw);
            let (mut r, mut g, mut b, mut a, mut n) = (0u64, 0u64, 0u64, 0u64, 0u64);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = &src[(sy * sw + sx) * 4..][..4];
                    let pa = p[3] as u64;
                    r += p[0] as u64 * pa;
                    g += p[1] as u64 * pa;
                    b += p[2] as u64 * pa;
                    a += pa;
                    n += 1;
                }
            }
            let o = &mut out[(dy * dw + dx) * 4..][..4];
            if let (Some(r), Some(g), Some(b)) =
                (r.checked_div(a), g.checked_div(a), b.checked_div(a))
            {
                o[0] = r as u8;
                o[1] = g as u8;
                o[2] = b as u8;
                o[3] = a.checked_div(n).unwrap_or(0) as u8;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// HICON -> RGBA (Win32)
// ---------------------------------------------------------------------------

/// Reads a bitmap as top-down 32bpp BGRA. Returns (pixels, width, height).
unsafe fn read_bitmap(hbm: HBITMAP) -> Option<(Vec<u8>, u32, u32)> {
    let mut bm = BITMAP::default();
    let got = GetObjectW(
        HGDIOBJ(hbm.0),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut _),
    );
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
        return None;
    }
    // Bound memory: tray icons are small; refuse absurd bitmaps.
    if bm.bmWidth > 1024 || bm.bmHeight > 2048 {
        return None;
    }
    let (w, h) = (bm.bmWidth as u32, bm.bmHeight as u32);
    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u8; (w * h * 4) as usize];
    let dc = CreateCompatibleDC(None);
    if dc.is_invalid() {
        return None;
    }
    let lines = GetDIBits(
        dc,
        hbm,
        0,
        h,
        Some(px.as_mut_ptr() as *mut _),
        &mut bi,
        DIB_RGB_COLORS,
    );
    let _ = DeleteDC(dc);
    (lines == h as i32).then_some((px, w, h))
}

/// Converts an icon handle owned by another process to 32x32 straight RGBA.
/// The icon is copied first because its owner may destroy it at any time.
fn icon_to_rgba(hicon: isize) -> Option<Vec<u8>> {
    if hicon == 0 {
        return None;
    }
    unsafe {
        let copy = CopyIcon(HICON(hicon as *mut _)).ok()?;
        let mut info = ICONINFO::default();
        let res = if GetIconInfo(copy, &mut info).is_ok() {
            let color = if info.hbmColor.is_invalid() {
                None
            } else {
                read_bitmap(info.hbmColor)
            };
            let mask = if info.hbmMask.is_invalid() {
                None
            } else {
                read_bitmap(info.hbmMask)
            };
            // GetDIBits expands a 1bpp mask to 32bpp: white (bit 1) is non-zero.
            let bits = |m: &[u8]| -> Vec<bool> {
                m.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| p[0] | p[1] | p[2] != 0)
                    .collect()
            };
            let out = match (color, mask) {
                (Some((c, w, h)), m) => {
                    let mb = m
                        .filter(|(_, mw, mh)| *mw == w && *mh >= h)
                        .map(|(mp, _, _)| {
                            let mut b = bits(&mp);
                            b.truncate((w * h) as usize);
                            b
                        });
                    Some((bgra_to_rgba(&c, mb.as_deref()), w, h))
                }
                // Mask-only (monochrome) icon: AND bits on top, XOR bits below.
                (None, Some((mp, w, h2))) if h2 >= 2 => {
                    let h = h2 / 2;
                    let b = bits(&mp);
                    let n = (w * h) as usize;
                    Some((mono_to_rgba(&b[..n], &b[n..2 * n]), w, h))
                }
                _ => None,
            };
            if !info.hbmColor.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(info.hbmColor.0));
            }
            if !info.hbmMask.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(info.hbmMask.0));
            }
            out
        } else {
            None
        };
        let _ = DestroyIcon(copy);
        let (px, w, h) = res?;
        Some(if w == ICON_PX && h == ICON_PX {
            px
        } else {
            scale_rgba(&px, w, h, ICON_PX, ICON_PX)
        })
    }
}

// ---------------------------------------------------------------------------
// Shared state and the tray thread
// ---------------------------------------------------------------------------

struct Shared {
    table: Mutex<Table>,
    /// A change notification is posted and not yet consumed by `icons()`.
    pending: AtomicBool,
    stopping: AtomicBool,
    hwnd: AtomicIsize,
    notify: isize,
    msg: u32,
}

impl Shared {
    fn signal(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            let ok = unsafe {
                PostMessageW(
                    Some(HWND(self.notify as *mut _)),
                    self.msg,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
            if ok.is_err() {
                self.pending.store(false, Ordering::Release);
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Table> {
        self.table.lock().unwrap_or_else(|p| p.into_inner())
    }
}

thread_local! {
    static CTX: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
    static OWN_HWND: Cell<isize> = const { Cell::new(0) };
}

const CLASS: PCWSTR = w!("Shell_TrayWnd");
const TOPMOST_TIMER: usize = 1;
const TOPMOST_DEBOUNCE_MS: u32 = 300;
const FORWARD_TIMEOUT_MS: u32 = 2000;
/// One-shot: repeat `TaskbarCreated` this long after an external one (Explorer restart), so icons
/// re-added while Explorer's tray was still coming up also reach us. Duplicates are harmless.
const REBROADCAST_TIMER: usize = 2;
const REBROADCAST_DELAY_MS: u32 = 1000;
/// Never rebroadcast more often than this (avoids ping-pong with another tray host doing the same).
const REBROADCAST_MIN_GAP: std::time::Duration = std::time::Duration::from_secs(10);

thread_local! {
    /// True while this thread broadcasts `TaskbarCreated` (our own window receives it synchronously).
    static SELF_BROADCAST: Cell<bool> = const { Cell::new(false) };
    static LAST_REBROADCAST: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

fn taskbar_created_msg() -> u32 {
    unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }
}

/// Broadcast from the tray thread, marked so our own window does not schedule a rebroadcast.
fn broadcast_taskbar_created_self() {
    SELF_BROADCAST.with(|c| c.set(true));
    broadcast_taskbar_created();
    SELF_BROADCAST.with(|c| c.set(false));
}

fn broadcast_taskbar_created() {
    let m = taskbar_created_msg();
    if m != 0 {
        unsafe {
            let _ = SendNotifyMessageW(HWND_BROADCAST, m, WPARAM(0), LPARAM(0));
        }
    }
}

/// Explorer's real tray window: first `Shell_TrayWnd` not owned by this process.
fn explorer_tray(own: HWND) -> Option<HWND> {
    let me = unsafe { GetCurrentProcessId() };
    let mut after: Option<HWND> = None;
    for _ in 0..16 {
        let h = unsafe { FindWindowExW(None, after, CLASS, PCWSTR::null()) }.ok()?;
        if h.is_invalid() {
            return None;
        }
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if h != own && pid != me {
            return Some(h);
        }
        after = Some(h);
    }
    None
}

/// Moves our window to the top of the top-most band if another tray is above it.
fn ensure_topmost(own: HWND) {
    unsafe {
        let first = FindWindowW(CLASS, PCWSTR::null()).unwrap_or_default();
        if first != own {
            let _ = SetWindowPos(
                own,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            );
        }
    }
}

fn schedule_topmost_check(own: HWND) {
    unsafe {
        SetTimer(Some(own), TOPMOST_TIMER, TOPMOST_DEBOUNCE_MS, None);
    }
}

/// Forwards a message to Explorer's tray. Sent messages are relayed
/// synchronously (with timeout) so the original sender gets Explorer's result.
fn forward(own: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    let target = explorer_tray(own)?;
    unsafe {
        if msg != WM_COPYDATA && !InSendMessage().as_bool() {
            let _ = PostMessageW(Some(target), msg, wp, lp);
            return Some(LRESULT(0));
        }
        let mut result = 0usize;
        let ok = SendMessageTimeoutW(
            target,
            msg,
            wp,
            lp,
            SMTO_NORMAL | SMTO_ABORTIFHUNG,
            FORWARD_TIMEOUT_MS,
            Some(&mut result),
        );
        (ok.0 != 0).then_some(LRESULT(result as isize))
    }
}

/// Handles a `WM_COPYDATA` tray notification. Never holds locks while forwarding.
unsafe fn on_copydata(own: HWND, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let cds = lp.0 as *const COPYDATASTRUCT;
    let mut is_tray = false;
    if !cds.is_null() {
        let cds = &*cds;
        let len = cds.cbData as usize;
        if cds.dwData == COPYDATA_TRAY && !cds.lpData.is_null() && len <= MAX_COPYDATA {
            is_tray = true;
            let buf = std::slice::from_raw_parts(cds.lpData as *const u8, len);
            if let Some(m) = parse_tray_data(buf) {
                handle_tray_msg(&m);
            }
        }
    }
    match forward(own, WM_COPYDATA, wp, lp) {
        Some(r) => r,
        // No Explorer tray: report success for tray messages so apps are content.
        None => LRESULT(is_tray as isize),
    }
}

fn handle_tray_msg(m: &TrayMsg) {
    if !matches!(
        m.message,
        NIM_ADD | NIM_MODIFY | NIM_DELETE | NIM_SETFOCUS | NIM_SETVERSION
    ) {
        return;
    }
    let Some(shared) = CTX.with(|c| c.borrow().clone()) else {
        return;
    };
    let rgba = if matches!(m.message, NIM_ADD | NIM_MODIFY) && m.flags & NIF_ICON != 0 {
        icon_to_rgba(m.hicon)
    } else {
        None
    };
    let changed = {
        let mut t = shared.lock();
        // Cheap (<= MAX_ICONS IsWindow calls) and only on incoming traffic.
        let pruned = t.prune(is_window);
        t.apply(m, rgba) | pruned
    };
    if changed {
        shared.signal();
    }
}

fn is_window(h: isize) -> bool {
    h != 0 && unsafe { IsWindow(Some(HWND(h as *mut _))).as_bool() }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        wndproc_inner(hwnd, msg, wp, lp)
    }));
    r.unwrap_or_else(|_| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
}

unsafe fn wndproc_inner(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let taskbar_created = TASKBAR_CREATED.with(Cell::get);
    match msg {
        WM_COPYDATA => on_copydata(hwnd, wp, lp),
        WM_TIMER if wp.0 == TOPMOST_TIMER => {
            let _ = KillTimer(Some(hwnd), TOPMOST_TIMER);
            ensure_topmost(hwnd);
            // One-shot (after foreground/display changes): drop icons of dead owners.
            if let Some(shared) = CTX.with(|c| c.borrow().clone()) {
                let pruned = shared.lock().prune(is_window);
                if pruned {
                    shared.signal();
                }
            }
            LRESULT(0)
        }
        WM_TIMER if wp.0 == REBROADCAST_TIMER => {
            let _ = KillTimer(Some(hwnd), REBROADCAST_TIMER);
            LAST_REBROADCAST.with(|c| c.set(Some(std::time::Instant::now())));
            broadcast_taskbar_created_self();
            LRESULT(0)
        }
        // Shell commands sent to "Shell_TrayWnd" (e.g. WM_COMMAND 407 show desktop, 419 minimize
        // all; WM_SYSCOMMAND): FindWindow finds us first, so relay them to Explorer's tray.
        WM_COMMAND | WM_SYSCOMMAND => match forward(hwnd, msg, wp, lp) {
            Some(r) => r,
            None if msg == WM_COMMAND => LRESULT(0),
            None => DefWindowProcW(hwnd, msg, wp, lp),
        },
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            schedule_topmost_check(hwnd);
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        WM_CLOSE => {
            // Only our own owner may close the host; ignore foreign WM_CLOSE.
            let stopping = CTX.with(|c| {
                c.borrow()
                    .as_ref()
                    .is_some_and(|s| s.stopping.load(Ordering::Acquire))
            });
            if stopping {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(Some(hwnd), TOPMOST_TIMER);
            let _ = KillTimer(Some(hwnd), REBROADCAST_TIMER);
            PostQuitMessage(0);
            LRESULT(0)
        }
        m if m != 0 && m == taskbar_created => {
            // Explorer (re)started or someone broadcast it: get back on top.
            ensure_topmost(hwnd);
            schedule_topmost_check(hwnd);
            let recent = LAST_REBROADCAST
                .with(Cell::get)
                .is_some_and(|t| t.elapsed() < REBROADCAST_MIN_GAP);
            if !SELF_BROADCAST.with(Cell::get) && !recent {
                // Re-setting the timer coalesces bursts into one rebroadcast.
                SetTimer(Some(hwnd), REBROADCAST_TIMER, REBROADCAST_DELAY_MS, None);
            }
            LRESULT(0)
        }
        // Explorer-private tray messages (WM_USER..WM_APP range): relay.
        m if (WM_USER..0xC000).contains(&m) => match forward(hwnd, m, wp, lp) {
            Some(r) => r,
            None => DefWindowProcW(hwnd, m, wp, lp),
        },
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn foreground_hook(
    _hook: HWINEVENTHOOK,
    _event: u32,
    _hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let _ = std::panic::catch_unwind(|| {
        let own = OWN_HWND.with(Cell::get);
        if own != 0 {
            let own = HWND(own as *mut _);
            ensure_topmost(own);
            // Explorer may re-raise its tray shortly after activation.
            schedule_topmost_check(own);
        }
    });
}

fn create_window() -> io::Result<(HWND, HINSTANCE)> {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).map_err(io::Error::other)?.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            lpszClassName: CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(io::Error::last_os_error());
        }
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            CLASS,
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        )
        .map_err(io::Error::other)?;
        Ok((hwnd, hinst))
    }
}

fn run(shared: Arc<Shared>, ready: mpsc::SyncSender<io::Result<()>>) {
    TASKBAR_CREATED.with(|c| c.set(taskbar_created_msg()));
    CTX.with(|c| *c.borrow_mut() = Some(shared.clone()));
    let (hwnd, hinst) = match create_window() {
        Ok(v) => v,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    OWN_HWND.with(|c| c.set(hwnd.0 as isize));
    shared.hwnd.store(hwnd.0 as isize, Ordering::Release);
    unsafe {
        // Allow lower-integrity apps to reach us (UIPI).
        let _ = ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, None);
        let tc = TASKBAR_CREATED.with(Cell::get);
        if tc != 0 {
            let _ = ChangeWindowMessageFilterEx(hwnd, tc, MSGFLT_ALLOW, None);
        }
    }
    ensure_topmost(hwnd);
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            None,
            Some(foreground_hook),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    };
    let _ = ready.send(Ok(()));
    // Ask running apps to re-register their icons (now routed through us).
    broadcast_taskbar_created_self();

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if !hook.is_invalid() {
            let _ = UnhookWinEvent(hook);
        }
        let _ = UnregisterClassW(CLASS, Some(hinst));
    }
    OWN_HWND.with(|c| c.set(0));
    CTX.with(|c| *c.borrow_mut() = None);
}

pub struct Tray {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Tray {
    /// Starts the tray host. `msg` is posted to `notify` (wParam/lParam 0)
    /// whenever the icon list changes; repeated changes are coalesced until
    /// the next `icons()` call.
    pub fn start(notify: HWND, msg: u32) -> io::Result<Tray> {
        let shared = Arc::new(Shared {
            table: Mutex::new(Table::default()),
            pending: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            hwnd: AtomicIsize::new(0),
            notify: notify.0 as isize,
            msg,
        });
        let (tx, rx) = mpsc::sync_channel(1);
        let s2 = shared.clone();
        let thread = std::thread::Builder::new()
            .name("monarch-tray".into())
            .spawn(move || run(s2, tx))?;
        match rx.recv() {
            Ok(Ok(())) => Ok(Tray {
                shared,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(io::Error::other("tray thread exited during start"))
            }
        }
    }

    /// Snapshot of visible icons in add order. Prunes icons whose owner window is gone.
    pub fn icons(&self) -> Vec<TrayIconData> {
        self.shared.pending.store(false, Ordering::Release);
        let mut t = self.shared.lock();
        t.prune(is_window);
        t.visible()
    }

    /// Simulates a click on icon `id`; `at` is the screen point (used by menus).
    pub fn click(&self, id: &str, click: Click, at: POINT) {
        let target = {
            let mut t = self.shared.lock();
            if t.prune(is_window) {
                self.shared.signal();
            }
            t.find(id).map(|e| (e.hwnd, e.uid, e.callback, e.version))
        };
        let Some((hwnd, uid, callback, version)) = target else {
            return;
        };
        if callback == 0 {
            return;
        }
        let owner = HWND(hwnd as *mut _);
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(owner, Some(&mut pid));
            if pid != 0 {
                let _ = AllowSetForegroundWindow(pid);
            }
            for (wp, lp) in compose_click(version, uid, click, at.x, at.y) {
                let _ = PostMessageW(Some(owner), callback, WPARAM(wp), LPARAM(lp));
            }
        }
    }

    /// Destroys the host window, joins the thread and lets apps re-register with Explorer.
    pub fn stop(self) {}

    fn shutdown(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.shared.stopping.store(true, Ordering::Release);
        let hwnd = self.shared.hwnd.load(Ordering::Acquire);
        if hwnd != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        let _ = thread.join();
        broadcast_taskbar_created();
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ---------------------------------------------------------------------------
// Tests (pure parts only; nothing here touches the live tray)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u32(b: &mut [u8], off: usize, v: u32) {
        b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put_u64(b: &mut [u8], off: usize, v: u64) {
        b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }
    fn put_wstr(b: &mut [u8], off: usize, s: &str) {
        for (i, u) in s.encode_utf16().enumerate() {
            b[off + i * 2..off + i * 2 + 2].copy_from_slice(&u.to_le_bytes());
        }
    }

    const GUID: [u8; 16] = [
        0x78, 0x56, 0x34, 0x12, 0xBC, 0x9A, 0xF0, 0xDE, 1, 2, 3, 4, 5, 6, 7, 8,
    ];

    #[allow(clippy::too_many_arguments)]
    fn build(
        layout: Layout,
        msg: u32,
        hwnd: u64,
        uid: u32,
        flags: u32,
        hicon: u64,
        tip: &str,
        state: (u32, u32),
        version: u32,
    ) -> Vec<u8> {
        let (o, size) = match layout {
            Layout::X86 => (&X86_OFFSETS, X86_SIZE),
            Layout::X64 => (&X64_OFFSETS, X64_SIZE),
        };
        let mut b = vec![0u8; 8 + size];
        put_u32(&mut b, 0, TRAY_SIGNATURE);
        put_u32(&mut b, 4, msg);
        let n = &mut b[8..];
        put_u32(n, 0, size as u32);
        match layout {
            Layout::X86 => {
                put_u32(n, o.hwnd, hwnd as u32);
                put_u32(n, o.hicon, hicon as u32);
            }
            Layout::X64 => {
                put_u64(n, o.hwnd, hwnd);
                put_u64(n, o.hicon, hicon);
            }
        }
        put_u32(n, o.uid, uid);
        put_u32(n, o.flags, flags);
        put_u32(n, o.callback, 0x8001);
        put_wstr(n, o.tip, tip);
        put_u32(n, o.state, state.0);
        put_u32(n, o.state_mask, state.1);
        put_u32(n, o.version, version);
        n[o.guid..o.guid + 16].copy_from_slice(&GUID);
        b
    }

    #[test]
    fn parses_x86_layout() {
        let b = build(
            Layout::X86,
            NIM_ADD,
            0x0012_34AB,
            7,
            NIF_MESSAGE | NIF_ICON | NIF_TIP,
            0x0005_0001,
            "Volume",
            (0, 0),
            0,
        );
        assert_eq!(b.len(), 964);
        let m = parse_tray_data(&b).unwrap();
        assert_eq!(m.message, NIM_ADD);
        assert_eq!(m.hwnd, 0x0012_34AB);
        assert_eq!(m.uid, 7);
        assert_eq!(m.callback, 0x8001);
        assert_eq!(m.hicon, 0x0005_0001);
        assert_eq!(m.tip, "Volume");
        assert_eq!(m.guid, None);
    }

    #[test]
    fn parses_x64_layout() {
        let b = build(
            Layout::X64,
            NIM_MODIFY,
            0x0000_0001_0002_0003,
            42,
            NIF_TIP | NIF_GUID | NIF_STATE,
            0x0000_0000_00AB_CDEF,
            "Network",
            (NIS_HIDDEN, NIS_HIDDEN),
            4,
        );
        assert_eq!(b.len(), 984);
        let m = parse_tray_data(&b).unwrap();
        assert_eq!(m.message, NIM_MODIFY);
        assert_eq!(m.hwnd, 0x0001_0002_0003);
        assert_eq!(m.uid, 42);
        assert_eq!(m.hicon, 0xAB_CDEF);
        assert_eq!(m.tip, "Network");
        assert_eq!(m.state & m.state_mask, NIS_HIDDEN);
        assert_eq!(m.version, 4);
        assert_eq!(m.guid, Some(GUID));
    }

    #[test]
    fn x86_handles_sign_extend() {
        let b = build(Layout::X86, NIM_ADD, 0xFFFF_FFF0, 1, 0, 0, "", (0, 0), 0);
        assert_eq!(parse_tray_data(&b).unwrap().hwnd, -16);
    }

    #[test]
    fn rejects_bad_or_short_buffers() {
        let mut b = build(Layout::X86, NIM_ADD, 1, 1, 0, 0, "", (0, 0), 0);
        assert!(parse_tray_data(&b[..20]).is_none());
        assert!(parse_tray_data(&[]).is_none());
        b[0] = 0;
        assert!(parse_tray_data(&b).is_none());
        // Every prefix length must be safe (no panic).
        let b = build(Layout::X64, NIM_ADD, 5, 1, 0xFF, 9, "abc", (1, 1), 4);
        for n in 0..b.len() {
            let _ = parse_tray_data(&b[..n]);
        }
    }

    #[test]
    fn unterminated_tip_is_clipped() {
        let mut b = build(Layout::X86, NIM_ADD, 1, 1, NIF_TIP, 0, "", (0, 0), 0);
        let tip = 8 + X86_OFFSETS.tip;
        for i in 0..128 {
            b[tip + i * 2..tip + i * 2 + 2].copy_from_slice(&(b'x' as u16).to_le_bytes());
        }
        assert_eq!(parse_tray_data(&b).unwrap().tip.len(), 128);
    }

    #[test]
    fn id_formatting() {
        assert_eq!(format_id(0x1234ab, 7), "1234ab:7");
        assert_eq!(format_guid(&GUID), "{12345678-9ABC-DEF0-0102-030405060708}");
    }

    #[test]
    fn click_version4() {
        let v = compose_click(4, 3, Click::Left, 100, 200);
        let x = (100u32 | (200 << 16)) as usize;
        assert_eq!(
            v,
            vec![
                (x, (WM_LBUTTONDOWN | (3 << 16)) as isize),
                (x, (WM_LBUTTONUP | (3 << 16)) as isize),
                (x, (NIN_SELECT | (3 << 16)) as isize),
            ]
        );
        let r = compose_click(4, 3, Click::Right, 1, 2);
        assert_eq!(r.last().unwrap().1, (WM_CONTEXTMENU | (3 << 16)) as isize);
        let d = compose_click(4, 3, Click::Double, 1, 2);
        assert_eq!(
            d,
            vec![(1 | (2 << 16), (WM_LBUTTONDBLCLK | (3 << 16)) as isize)]
        );
    }

    #[test]
    fn click_version4_negative_coordinates() {
        let v = compose_click(4, 1, Click::Left, -5, -1);
        assert_eq!(v[0].0, 0xFFFF_FFFB);
    }

    #[test]
    fn click_legacy() {
        let v = compose_click(0, 9, Click::Right, 100, 200);
        assert_eq!(
            v,
            vec![(9, WM_RBUTTONDOWN as isize), (9, WM_RBUTTONUP as isize)]
        );
        let v = compose_click(0, 9, Click::Middle, 0, 0);
        assert_eq!(v.len(), 2);
    }

    fn msg(message: u32, hwnd: isize, uid: u32, flags: u32) -> TrayMsg {
        TrayMsg {
            message,
            hwnd,
            uid,
            flags,
            ..Default::default()
        }
    }

    #[test]
    fn table_add_modify_delete_order() {
        let mut t = Table::default();
        let px = || Some(vec![1u8; 4]);
        assert!(t.apply(&msg(NIM_ADD, 1, 1, NIF_ICON), px()));
        assert!(t.apply(&msg(NIM_ADD, 2, 1, NIF_ICON), px()));
        let mut m = msg(NIM_MODIFY, 1, 1, NIF_TIP);
        m.tip = "a".into();
        assert!(t.apply(&m, None));
        let v = t.visible();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "1:1");
        assert_eq!(v[0].tooltip, "a");
        assert_eq!(v[1].id, "2:1");
        assert!(t.apply(&msg(NIM_DELETE, 1, 1, 0), None));
        assert_eq!(t.visible().len(), 1);
        assert!(!t.apply(&msg(NIM_DELETE, 1, 1, 0), None));
    }

    #[test]
    fn table_hidden_and_version() {
        let mut t = Table::default();
        t.apply(&msg(NIM_ADD, 1, 1, NIF_ICON), Some(vec![0; 4]));
        let mut m = msg(NIM_MODIFY, 1, 1, NIF_STATE);
        m.state = NIS_HIDDEN;
        m.state_mask = NIS_HIDDEN;
        t.apply(&m, None);
        assert!(t.visible().is_empty());
        m.state = 0;
        t.apply(&m, None);
        assert_eq!(t.visible().len(), 1);
        let mut v = msg(NIM_SETVERSION, 1, 1, 0);
        v.version = 4;
        t.apply(&v, None);
        assert_eq!(t.entries[0].version, 4);
    }

    #[test]
    fn table_guid_identity() {
        let mut t = Table::default();
        let mut a = msg(NIM_ADD, 1, 1, NIF_ICON | NIF_GUID);
        a.guid = Some(GUID);
        t.apply(&a, Some(vec![0; 4]));
        // Same GUID from a different hwnd/uid updates the same entry.
        let mut b = msg(NIM_MODIFY, 2, 5, NIF_GUID | NIF_TIP);
        b.guid = Some(GUID);
        b.tip = "x".into();
        t.apply(&b, None);
        assert_eq!(t.entries.len(), 1);
        assert_eq!(t.visible()[0].id, format_guid(&GUID));
        assert!(t.find(&format_guid(&GUID)).is_some());
    }

    #[test]
    fn table_bounded() {
        let mut t = Table::default();
        for i in 0..(MAX_ICONS as isize + 10) {
            t.apply(&msg(NIM_ADD, i + 1, 0, NIF_ICON), Some(vec![0; 4]));
        }
        assert_eq!(t.entries.len(), MAX_ICONS);
    }

    #[test]
    fn table_prune() {
        let mut t = Table::default();
        for h in 1..=4 {
            t.apply(&msg(NIM_ADD, h, 0, NIF_ICON), Some(vec![0; 4]));
        }
        assert!(t.prune(|h| h % 2 == 0));
        let ids: Vec<_> = t.visible().into_iter().map(|i| i.id).collect();
        assert_eq!(ids, vec!["2:0", "4:0"]);
        assert!(!t.prune(|_| true));
    }

    #[test]
    fn pixel_conversion() {
        // No alpha channel: alpha from mask.
        let c = [10, 20, 30, 0, 40, 50, 60, 0];
        assert_eq!(
            bgra_to_rgba(&c, Some(&[false, true])),
            vec![30, 20, 10, 255, 60, 50, 40, 0]
        );
        // Alpha present: mask ignored.
        let c = [1, 2, 3, 128];
        assert_eq!(bgra_to_rgba(&c, Some(&[true])), vec![3, 2, 1, 128]);
        assert_eq!(
            mono_to_rgba(&[false, false, true], &[false, true, false]),
            vec![0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0]
        );
    }

    #[test]
    fn scaling() {
        // 2x2 -> 1x1 averages with alpha weighting: transparent pixels don't darken.
        let src = [
            255, 0, 0, 255, 0, 0, 0, 0, //
            255, 0, 0, 255, 0, 0, 0, 0,
        ];
        assert_eq!(scale_rgba(&src, 2, 2, 1, 1), vec![255, 0, 0, 127]);
        // 16 -> 32 upscale keeps size and colors.
        let src: Vec<u8> = [9, 8, 7, 255].repeat(16 * 16);
        let out = scale_rgba(&src, 16, 16, 32, 32);
        assert_eq!(out.len(), 32 * 32 * 4);
        assert!(out.as_chunks::<4>().0.iter().all(|p| *p == [9, 8, 7, 255]));
        // Undersized source is rejected safely.
        assert_eq!(scale_rgba(&[0; 4], 2, 2, 1, 1), vec![0; 4]);
    }
}
