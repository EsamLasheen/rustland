//! Win+/ cheat sheet: every binding grouped like the legacy keybinds page, drawn natively.
//! Closes on Esc, any click, or losing focus.
use crate::input_hooks::{Act, BINDINGS};
use crate::render::{self, Align, Radii, Rect};
use crate::theme;
use std::cell::Cell;
use windows::{
    core::w,
    Win32::{
        Foundation::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::*},
    },
};

thread_local! { static OPEN: Cell<isize> = const { Cell::new(0) }; }

fn describe(a: Act) -> String {
    match a {
        Act::Wm(v) => v.replace('-', " "),
        Act::WmIndexed(v, i) => format!(
            "{} {}",
            v.replace('-', " "),
            if i == 10 {
                "S".into()
            } else {
                (i as u32 + 1).to_string()
            }
        ),
        Act::Launch(t) => format!("open {}", t.split(':').next().unwrap_or(t)),
        Act::Launcher => "launcher".into(),
        Act::Panel(t) => format!("panel · {t:?}").to_lowercase(),
        Act::Capsule(t) => format!("capsule · {t:?}").to_lowercase(),
        Act::Help => "this sheet".into(),
        Act::Reload => "reload settings".into(),
        Act::Sleep => "sleep".into(),
        Act::Capture => "Gemini screenshot".into(),
    }
}

fn group(a: Act) -> usize {
    match a {
        Act::Wm(v)
            if v.starts_with("focus ") || v.starts_with("move ") || v.starts_with("resize") =>
        {
            2
        }
        Act::Wm("focus-last-workspace") | Act::Wm("special") | Act::WmIndexed(..) => 3,
        Act::Wm(_) => 0,
        Act::Launch(_) | Act::Launcher => 1,
        Act::Panel(_) | Act::Capsule(_) | Act::Help => 4,
        _ => 5,
    }
}
const GROUPS: [&str; 6] = [
    "WINDOWS",
    "APPS",
    "FOCUS · MOVE",
    "WORKSPACES",
    "PANELS",
    "SYSTEM",
];

pub fn toggle() {
    let open = OPEN.with(|o| o.get());
    if open != 0 {
        unsafe {
            let _ = DestroyWindow(HWND(open as *mut _));
        }
        return;
    }
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("MonarchNext.Help");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        });
        let Ok(h) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class,
            w!("Monarch Next · keys"),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(module.into()),
            None,
        ) else {
            return;
        };
        OPEN.with(|o| o.set(h.0 as isize));
        paint(h);
        let _ = ShowWindow(h, SW_SHOW);
        crate::runtime::bring_to_front(h);
    }
}

const COL_W: f32 = 300.;
const ROW: f32 = 22.;

fn paint(h: HWND) {
    let t = crate::runtime::tokens();
    let mut cols: Vec<Vec<(String, String)>> = vec![vec![]; GROUPS.len()];
    for b in BINDINGS {
        cols[group(b.act)].push((b.label.to_string(), describe(b.act)));
    }
    // three columns of two groups each
    let col_h = |a: usize, b: usize| 2. * 34. + (cols[a].len() + cols[b].len()) as f32 * ROW;
    let body = (0..3).map(|i| col_h(i * 2, i * 2 + 1)).fold(0f32, f32::max);
    let (w, hgt) = (3. * COL_W + 64., body + 96.);
    unsafe {
        let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
        // short or scaled-up screens: shrink the sheet to fit instead of cutting off its title
        let fit = (sw as f32 * 0.95 / w).min(sh as f32 * 0.95 / hgt);
        let k = (GetDpiForWindow(h).max(96) as f32 / 96.).min(fit);
        let (pw, ph) = ((w * k) as i32, (hgt * k) as i32);
        let _ = SetWindowPos(
            h,
            Some(HWND_TOPMOST),
            (sw - pw) / 2,
            (sh - ph) / 2,
            pw,
            ph,
            SWP_NOACTIVATE,
        );
        let _ = render::present(h, pw, ph, k, |c| {
            let card = Rect::new(0., 0., w, hgt);
            c.chip(card, Radii::all(16.), t.bg, Some(t.accent.alpha(0.55)));
            c.text_tracked(
                "SHADOW MONARCH · KEYS",
                Rect::new(32., 22., w - 64., 20.),
                &theme::label(12., 700),
                t.accent,
                Align::Start,
                2.,
            );
            c.text(
                "Esc to close",
                Rect::new(32., 22., w - 64., 20.),
                &theme::label(11., 600),
                t.muted,
                Align::End,
            );
            for ci in 0..3 {
                let x = 32. + ci as f32 * COL_W;
                let mut y = 64.;
                for g in [ci * 2, ci * 2 + 1] {
                    c.text_tracked(
                        GROUPS[g],
                        Rect::new(x, y, COL_W - 16., 20.),
                        &theme::label(10.5, 700),
                        t.muted,
                        Align::Start,
                        1.5,
                    );
                    y += 26.;
                    for (key, what) in &cols[g] {
                        let kw = c.measure(key, &theme::label(11.5, 700), 0.) + 14.;
                        c.chip(
                            Rect::new(x, y + 1., kw, 18.),
                            Radii::all(5.),
                            t.accent_soft,
                            None,
                        );
                        c.text(
                            key,
                            Rect::new(x, y + 1., kw, 18.),
                            &theme::label(11.5, 700),
                            t.accent,
                            Align::Center,
                        );
                        c.text(
                            what,
                            Rect::new(x + kw + 10., y, COL_W - kw - 26., 20.),
                            &theme::body(12., 500),
                            t.text,
                            Align::Start,
                        );
                        y += ROW;
                    }
                    y += 8.;
                }
            }
        });
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_KEYDOWN if wp.0 as u16 == 0x1B => {
            let _ = DestroyWindow(h);
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let _ = DestroyWindow(h);
            LRESULT(0)
        }
        WM_ACTIVATE if (wp.0 & 0xffff) as u32 == WA_INACTIVE => {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(h);
            LRESULT(0)
        }
        WM_DESTROY => {
            OPEN.with(|o| o.set(0));
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}
