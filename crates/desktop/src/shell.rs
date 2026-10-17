//! Live shell surfaces (pure Rust + Windows APIs). `--preview topbar` runs the real top bar with live data
//! at the bottom of the screen without reserving space or replacing anything (desktop integration stays
//! a separate, explicit step). Updates: 3 s metric timer while visible + foreground/title events only.
use crate::render::{self, Color};
use crate::theme::Tokens;
use crate::topbar::{self, State};
use std::cell::RefCell;
use windows::{
    core::w,
    Win32::{
        Foundation::*,
        NetworkManagement::WiFi::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{Accessibility::*, HiDpi::*, WindowsAndMessaging::*},
    },
};

const TICK: usize = 1;
const FG_CHANGED: u32 = WM_APP + 31;

struct Bar {
    hwnd: HWND,
    tokens: Tokens,
    state: State,
    cpu_times: Option<(u64, u64)>,
    net_before: Option<(std::time::Instant, u64, u64)>,
    wlan: Option<HANDLE>,
    hits: Vec<(render::Rect, topbar::Hit)>,
    fg: HWND,
}
thread_local! { static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) }; }
static BAR_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

fn dpi_scale(h: HWND) -> f32 {
    unsafe { GetDpiForWindow(h).max(96) as f32 / 96. }
}

/// Total bytes in/out on physical up interfaces (Ethernet / Wi-Fi).
pub(crate) fn net_totals() -> Option<(u64, u64)> {
    use windows::Win32::NetworkManagement::{IpHelper::*, Ndis::*};
    unsafe {
        let mut table = std::ptr::null_mut();
        if GetIfTable2(&mut table).is_err() || table.is_null() {
            return None;
        }
        let rows = std::slice::from_raw_parts(
            std::ptr::addr_of!((*table).Table) as *const MIB_IF_ROW2,
            (*table).NumEntries as usize,
        );
        let mut t = (0u64, 0u64);
        let mut any = false;
        for r in rows {
            if r.OperStatus == IfOperStatusUp
                && r.InterfaceAndOperStatusFlags._bitfield & 1 != 0
                && matches!(r.Type, 6 | 71)
            {
                t.0 += r.InOctets;
                t.1 += r.OutOctets;
                any = true;
            }
        }
        FreeMibTable(table.cast());
        any.then_some(t)
    }
}

/// SSID of the connected Wi-Fi network (native WLAN API, handle kept open).
pub(crate) fn ssid(handle: &mut Option<HANDLE>) -> Option<String> {
    unsafe {
        if handle.is_none() {
            let (mut ver, mut h) = (0u32, HANDLE::default());
            if WlanOpenHandle(2, None, &mut ver, &mut h) != 0 {
                return None;
            }
            *handle = Some(h);
        }
        let h = (*handle)?;
        let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
        if WlanEnumInterfaces(h, None, &mut list) != 0 || list.is_null() {
            return None;
        }
        let infos = std::slice::from_raw_parts(
            std::ptr::addr_of!((*list).InterfaceInfo) as *const WLAN_INTERFACE_INFO,
            (*list).dwNumberOfItems as usize,
        );
        let mut out = None;
        for i in infos {
            if i.isState != wlan_interface_state_connected {
                continue;
            }
            let (mut size, mut data, mut kind) = (
                0u32,
                std::ptr::null_mut(),
                WLAN_OPCODE_VALUE_TYPE::default(),
            );
            if WlanQueryInterface(
                h,
                &i.InterfaceGuid,
                wlan_intf_opcode_current_connection,
                None,
                &mut size,
                &mut data,
                Some(&mut kind),
            ) == 0
                && !data.is_null()
            {
                let attr = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                let s = &attr.wlanAssociationAttributes.dot11Ssid;
                out = Some(
                    String::from_utf8_lossy(&s.ucSSID[..(s.uSSIDLength as usize).min(32)])
                        .into_owned(),
                );
                WlanFreeMemory(data);
                break;
            }
        }
        WlanFreeMemory(list as *const _);
        out
    }
}

pub(crate) fn window_title(h: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

/// A foreground window that represents an app (not the shell, desktop, task switcher).
pub(crate) fn is_app_window(h: HWND) -> bool {
    unsafe {
        if h.is_invalid() || h.0 as isize == BAR_HWND.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if ex & WS_EX_TOOLWINDOW.0 != 0 {
            return false;
        }
        let mut cls = [0u16; 64];
        let n = GetClassNameW(h, &mut cls);
        let cls = String::from_utf16_lossy(&cls[..n.max(0) as usize]);
        !matches!(
            cls.as_str(),
            "Progman"
                | "WorkerW"
                | "Shell_TrayWnd"
                | "ForegroundStaging"
                | "MultitaskingViewFrame"
                | "XamlExplorerHostIslandWindow"
        )
    }
}

fn refresh_window(b: &mut Bar) {
    let fg = unsafe { GetForegroundWindow() };
    if is_app_window(fg) {
        b.fg = fg;
    }
    let title = window_title(b.fg);
    b.state.window_title = if title.is_empty() {
        "Solo Leveling Shell".into()
    } else {
        title
    };
    b.state.window_icon = crate::appicons::for_window(b.fg);
    b.state.window_letter = crate::appicons::exe_of(b.fg).and_then(|p| {
        std::path::Path::new(&p).file_stem().map(|s| {
            s.to_string_lossy()
                .chars()
                .next()
                .unwrap_or('?')
                .to_ascii_uppercase()
        })
    });
}

fn refresh_metrics(b: &mut Bar) {
    let (snap, times) = crate::platform::system_snapshot(b.cpu_times);
    b.cpu_times = times;
    b.state.cpu = snap.cpu.map(|v| v as f32);
    b.state.ram = snap.memory.map(|v| v as f32);
    b.state.battery = snap.battery.map(|p| (p as f32, snap.charging));
    let now = std::time::Instant::now();
    match net_totals() {
        Some((rx, tx)) => {
            b.state.connected = true;
            if let Some((at, orx, otx)) = b.net_before {
                let s = now.duration_since(at).as_secs_f64();
                if s > 0. && s < 30. {
                    b.state.down_bps = Some(rx.saturating_sub(orx) as f64 / s);
                    b.state.up_bps = Some(tx.saturating_sub(otx) as f64 / s);
                }
            }
            b.net_before = Some((now, rx, tx));
        }
        None => {
            b.state.connected = false;
            b.state.down_bps = None;
            b.state.up_bps = None;
            b.net_before = None;
        }
    }
    b.state.ssid = ssid(&mut b.wlan);
}

fn paint(h: HWND) {
    BAR.with(|b| {
        let mut b = b.borrow_mut();
        let Some(b) = b.as_mut() else { return };
        let mut r = RECT::default();
        unsafe {
            let _ = GetWindowRect(h, &mut r);
        }
        let (w, ht) = (r.right - r.left, r.bottom - r.top);
        let scale = dpi_scale(h);
        let mut hits = Vec::new();
        let _ = render::present(h, w, ht, scale, |c| {
            hits = topbar::paint(c, w as f32 / scale, &b.tokens, &b.state);
        });
        b.hits = hits;
    });
}

unsafe extern "system" fn on_foreground(
    _: HWINEVENTHOOK,
    ev: u32,
    h: HWND,
    obj: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    // title changes only matter for the window being shown
    if ev == EVENT_OBJECT_NAMECHANGE && (obj != 0 || h != GetForegroundWindow()) {
        return;
    }
    let bar = HWND(BAR_HWND.load(std::sync::atomic::Ordering::Relaxed) as *mut _);
    let _ = PostMessageW(Some(bar), FG_CHANGED, WPARAM(0), LPARAM(0));
}

unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER if w.0 == TICK => {
            BAR.with(|b| {
                if let Some(b) = b.borrow_mut().as_mut() {
                    refresh_metrics(b)
                }
            });
            paint(h);
            LRESULT(0)
        }
        FG_CHANGED => {
            BAR.with(|b| {
                if let Some(b) = b.borrow_mut().as_mut() {
                    refresh_window(b)
                }
            });
            paint(h);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let _ = DestroyWindow(h);
            LRESULT(0)
        } // preview: right-click closes
        WM_DPICHANGED | WM_DISPLAYCHANGE => {
            paint(h);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, w, l),
    }
}

/// Live top bar preview (bottom of the primary screen, no space reserved, right-click to close).
pub fn preview_topbar(accent: Color) -> std::io::Result<()> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let module = GetModuleHandleW(None).map_err(std::io::Error::other)?;
        let class = w!("MonarchNext.TopBar");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        });
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        let probe_scale = GetDpiForSystem().max(96) as f32 / 96.;
        let bh = (crate::theme::BAR_H * probe_scale).round() as i32;
        let h = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class,
            w!("Monarch Next — Top bar (preview)"),
            WS_POPUP,
            0,
            sh - bh,
            sw,
            bh,
            None,
            None,
            Some(module.into()),
            None,
        )
        .map_err(std::io::Error::other)?;
        BAR_HWND.store(h.0 as isize, std::sync::atomic::Ordering::Relaxed);
        BAR.with(|b| {
            *b.borrow_mut() = Some(Bar {
                hwnd: h,
                tokens: Tokens::new(accent),
                state: State::default(),
                cpu_times: None,
                net_before: None,
                wlan: None,
                hits: vec![],
                fg: HWND::default(),
            })
        });
        BAR.with(|b| {
            if let Some(b) = b.borrow_mut().as_mut() {
                refresh_metrics(b);
                refresh_window(b)
            }
        });
        let hooks = [
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(on_foreground),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            ),
            SetWinEventHook(
                EVENT_OBJECT_NAMECHANGE,
                EVENT_OBJECT_NAMECHANGE,
                None,
                Some(on_foreground),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            ),
        ];
        let _ = ShowWindow(h, SW_SHOWNOACTIVATE);
        paint(h);
        SetTimer(Some(h), TICK, 3000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        for k in hooks {
            let _ = UnhookWinEvent(k);
        }
        BAR.with(|b| {
            if let Some(b) = b.borrow_mut().take() {
                if let Some(w) = b.wlan {
                    WlanCloseHandle(w, None);
                }
                let _ = b.hwnd;
            }
        });
        render::shutdown();
        Ok(())
    }
}
