//! Native launcher (replaces Flow Launcher): Win+R. Fuzzy app search over Start-menu shortcuts and
//! Store apps, inline calculator, web-search fallback. Keyboard-first; closes on Esc or when it
//! loses focus.
use crate::render::{self, Align, Canvas, Radii, Rect};
use crate::theme::{self, Tokens, R};
use crate::topbar::Image;
use monarch_core::launcher as logic;
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::{
    core::w,
    Win32::{
        Foundation::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{HiDpi::GetDpiForWindow, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    },
};

const W: f32 = 600.;
const ROW: f32 = 48.;
const ROWS: usize = 8;
const FIELD: f32 = 56.;
/// Maximum query length in characters (typed or pasted).
const MAX_QUERY: usize = 256;
/// Posted to the open launcher window by the warm thread once the app index is ready.
const WM_APPS_READY: u32 = WM_APP + 1;
/// Posted by the icon worker once a Store app icon requested by `draw` is ready.
const WM_ICON_READY: u32 = WM_APP + 2;

#[derive(Clone)]
enum Item {
    App(usize),
    Calc(String),
    Web(String),
}

struct Launcher {
    hwnd: HWND,
    query: String,
    sel: usize,
    items: Vec<Item>,
    hits: Vec<(Rect, usize)>,
    hover: Option<usize>,
    /// High surrogate from a WM_CHAR waiting for its low half.
    pending_high: Option<u16>,
    /// The index `items` point into; swapped only together with `items` (see `update`), so a
    /// rescan finishing in the background never shifts what a row launches.
    apps: Apps,
}
thread_local! { static L: RefCell<Option<Launcher>> = const { RefCell::new(None) }; }
type Apps = Arc<Vec<crate::platform::Application>>;
/// The app index and when its scan finished.
static APPS: Mutex<Option<(Instant, Apps)>> = Mutex::new(None);
/// Set while a scan is running, so two never overlap.
static WARMING: AtomicBool = AtomicBool::new(false);
/// The open launcher window (0 = none), notified when the index becomes ready.
static OPEN_HWND: AtomicIsize = AtomicIsize::new(0);
/// An index older than this is rescanned (in the background) when the launcher opens, so apps
/// installed or removed since show up without restarting the shell.
const STALE: Duration = Duration::from_secs(60);

/// Scan the Start menu and the shell Apps folder (Store apps) on a background thread (startup), so
/// the first Win+R is instant. No-op while a scan is already running; the previous index stays in
/// use until the new one replaces it.
pub fn warm() {
    if WARMING.swap(true, Ordering::AcqRel) {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("launcher-index".into())
        .spawn(|| {
            let apps = Arc::new(crate::platform::applications());
            // Read the open window under the same lock `show` uses to publish it, so a launcher
            // opened mid-scan either sees the index or receives WM_APPS_READY.
            let h = match APPS.lock() {
                Ok(mut g) => {
                    *g = Some((Instant::now(), apps));
                    OPEN_HWND.load(Ordering::Acquire)
                }
                Err(_) => 0,
            };
            WARMING.store(false, Ordering::Release);
            if h != 0 {
                unsafe {
                    let _ =
                        PostMessageW(Some(HWND(h as *mut _)), WM_APPS_READY, WPARAM(0), LPARAM(0));
                }
            }
        });
    if spawned.is_err() {
        WARMING.store(false, Ordering::Release);
    }
}
/// The app index, or an empty list while the first background scan is still running (never scans
/// on the UI thread; the launcher refreshes on WM_APPS_READY).
fn apps() -> Apps {
    if let Some((_, a)) = APPS.lock().ok().and_then(|g| g.clone()) {
        return a;
    }
    warm();
    Arc::new(Vec::new())
}
/// Start a background rescan if the index is older than `STALE` (the old one keeps serving).
fn refresh_if_stale() {
    let stale = APPS
        .lock()
        .map(|g| g.as_ref().is_some_and(|(at, _)| at.elapsed() >= STALE))
        .unwrap_or(false);
    if stale {
        warm();
    }
}

fn search(apps: &[crate::platform::Application], q: &str) -> Vec<Item> {
    let mut out = vec![];
    if let Some(v) = logic::calc(q) {
        out.push(Item::Calc(logic::format_number(v)));
    }
    if !q.trim().is_empty() || out.is_empty() {
        let names: Vec<&str> = apps.iter().map(|a| a.search_key.as_str()).collect();
        let limit = if q.trim().is_empty() { 0 } else { ROWS };
        out.extend(logic::rank(&names, q, limit).into_iter().map(Item::App));
    }
    if !q.trim().is_empty() && out.len() < ROWS {
        out.push(Item::Web(q.trim().to_string()));
    }
    out.truncate(ROWS);
    out
}

fn height(n: usize) -> f32 {
    FIELD + if n == 0 { 0. } else { 8. + n as f32 * ROW + 8. }
}

pub fn toggle() {
    let open = L.with(|l| l.borrow().as_ref().map(|x| x.hwnd));
    match open {
        Some(h) => unsafe {
            let _ = DestroyWindow(h);
        },
        None => show(),
    }
}

fn show() {
    unsafe {
        let module = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("MonarchNext.Launcher");
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
            w!("Monarch Next · launcher"),
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
        L.with(|l| {
            *l.borrow_mut() = Some(Launcher {
                hwnd: h,
                query: String::new(),
                sel: 0,
                items: vec![],
                hits: vec![],
                hover: None,
                pending_high: None,
                apps: Arc::new(Vec::new()),
            })
        });
        {
            let _g = APPS.lock(); // pairs with the warm thread (see `warm`); released before update()
            OPEN_HWND.store(h.0 as isize, Ordering::Release);
        }
        refresh_if_stale();
        update();
        let _ = ShowWindow(h, SW_SHOW);
        crate::runtime::bring_to_front(h);
    }
}

fn update() {
    L.with(|l| {
        let mut b = l.borrow_mut();
        let Some(s) = b.as_mut() else { return };
        s.apps = apps();
        s.items = search(&s.apps, &s.query);
        s.sel = s.sel.min(s.items.len().saturating_sub(1));
    });
    paint();
}

fn scale(h: HWND) -> f32 {
    unsafe { GetDpiForWindow(h).max(96) as f32 / 96. }
}

fn paint() {
    let t = crate::runtime::tokens();
    L.with(|l| {
        let mut b = l.borrow_mut();
        let Some(s) = b.as_mut() else { return };
        let k = scale(s.hwnd);
        let hgt = height(s.items.len()) + 24.; // shadow room
        let (pw, ph) = (((W + 48.) * k) as i32, (hgt * k) as i32);
        unsafe {
            let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
            let _ = SetWindowPos(
                s.hwnd,
                Some(HWND_TOPMOST),
                (sw - pw) / 2,
                sh / 5,
                pw,
                ph,
                SWP_NOACTIVATE,
            );
        }
        let mut hits = vec![];
        let _ = render::present(s.hwnd, pw, ph, k, |c| hits = draw(c, &t, s, &s.apps));
        s.hits = hits;
    });
}

fn draw(
    c: &Canvas,
    t: &Tokens,
    s: &Launcher,
    apps: &[crate::platform::Application],
) -> Vec<(Rect, usize)> {
    let card = Rect::new(24., 8., W, height(s.items.len()));
    let shadow: &[(f32, f32)] = if t.glass {
        &[]
    } else {
        &[(16., 0.10), (8., 0.16), (3., 0.22)]
    };
    for &(d, a) in shadow {
        c.fill_round(
            Rect::new(
                card.x - d / 2.,
                card.y + d / 3.,
                card.w + d,
                card.h + d / 2.,
            ),
            Radii::all(R + d / 2.),
            crate::render::Color::rgba(0, 0, 0, a),
        );
    }
    c.chip(
        card,
        Radii::all(R + 4.),
        t.surface_solid,
        Some(t.accent.alpha(0.55)),
    );
    // search field
    let f = Rect::new(card.x + 18., card.y, card.w - 36., FIELD);
    c.icon(
        "search",
        Rect::new(f.x, f.y + (FIELD - 20.) / 2., 20., 20.),
        t.accent,
        2.,
    );
    let font = theme::body(18., 500);
    let tx = Rect::new(f.x + 34., f.y, f.w - 34., FIELD);
    if s.query.is_empty() {
        c.text(
            "Search apps, calculate, or search the web",
            tx,
            &font,
            t.muted,
            Align::Start,
        );
    } else {
        c.text(&s.query, tx, &font, t.text, Align::Start);
    }
    let w = c.measure(&s.query, &font, 0.).min(tx.w - 1.);
    let caret_x = if render::is_rtl(&s.query) {
        // RTL text is laid out from the right edge of the box (leading edge), so the insertion
        // point after the last typed character sits at the left end of the run, measured from
        // the right end of the box.
        (tx.right() - w - 3.).max(tx.x)
    } else {
        tx.x + w + 1.
    };
    c.fill_rect(Rect::new(caret_x, f.y + 17., 2., 22.), t.accent);
    let mut hits = vec![];
    if s.items.is_empty() {
        return hits;
    }
    c.fill_rect(
        Rect::new(card.x + 12., card.y + FIELD, card.w - 24., 1.),
        t.line_soft,
    );
    for (i, it) in s.items.iter().enumerate() {
        let r = Rect::new(
            card.x + 8.,
            card.y + FIELD + 8. + i as f32 * ROW,
            card.w - 16.,
            ROW,
        );
        let on = i == s.sel;
        if on {
            c.fill_round(r, Radii::all(R), t.accent_soft);
            c.fill_round(
                Rect::new(r.x + 2., r.y + 12., 3., ROW - 24.),
                Radii::all(2.),
                t.accent,
            );
        } else if s.hover == Some(i) {
            c.fill_round(r, Radii::all(R), t.text.alpha(0.05));
        }
        let ic = Rect::new(r.x + 14., r.y + 10., 28., 28.);
        let (title, sub): (String, &str) = match it {
            Item::App(ix) => {
                let Some(a) = apps.get(*ix) else { continue };
                // Store app icons load in the background (glyph until WM_ICON_READY repaints)
                let path = a.path.to_string_lossy();
                match crate::appicons::for_path_deferred(&path, s.hwnd, WM_ICON_READY) {
                    Some(Image { rgba, w, h }) => c.image(&rgba, w, h, ic, 1.),
                    None => c.icon("app", ic.inset(2.), t.muted, 1.8),
                }
                (a.label.clone(), "Application")
            }
            Item::Calc(v) => {
                c.icon("calculator", ic.inset(4.), t.accent, 2.);
                (format!("= {v}"), "Enter copies the result")
            }
            Item::Web(q) => {
                c.icon("globe", ic.inset(4.), t.muted, 2.);
                (
                    format!("Search the web for “{q}”"),
                    "Opens your default browser",
                )
            }
        };
        let tr = Rect::new(r.x + 54., r.y + 5., r.w - 70., 22.);
        c.text(
            &title,
            tr,
            &theme::body(14.5, if on { 600 } else { 500 }),
            if on { t.text } else { t.text.alpha(0.9) },
            Align::Start,
        );
        c.text(
            sub,
            Rect::new(tr.x, r.y + 25., tr.w, 16.),
            &theme::label(11., 600),
            t.muted,
            Align::Start,
        );
        if on {
            c.text(
                "↵",
                Rect::new(r.right() - 34., r.y, 24., ROW),
                &theme::label(14., 700),
                t.accent,
                Align::Center,
            );
        }
        hits.push((r, i));
    }
    hits
}

fn activate(i: usize) {
    let item = L.with(|l| {
        l.borrow()
            .as_ref()
            .and_then(|s| Some((s.items.get(i).cloned()?, s.apps.clone())))
    });
    let Some((item, a)) = item else { return };
    match item {
        Item::App(ix) => {
            if let Some(app) = a.get(ix) {
                let _ = crate::platform::open(&app.path);
            }
        }
        Item::Calc(v) => crate::runtime::copy_text(&v),
        Item::Web(q) => {
            let enc: String = q
                .bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                        (b as char).to_string()
                    } else if b == b' ' {
                        "+".into()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect();
            let _ = crate::platform::open(std::path::Path::new(&format!(
                "https://www.google.com/search?q={enc}"
            )));
        }
    }
    close();
}
fn close() {
    let h = L.with(|l| l.borrow().as_ref().map(|s| s.hwnd));
    if let Some(h) = h {
        unsafe {
            let _ = DestroyWindow(h);
        }
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ACTIVATE if (wp.0 & 0xffff) as u32 == WA_INACTIVE => {
            let _ = PostMessageW(Some(h), WM_CLOSE, WPARAM(0), LPARAM(0));
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(h);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ =
                OPEN_HWND.compare_exchange(h.0 as isize, 0, Ordering::AcqRel, Ordering::Acquire);
            L.with(|l| {
                l.borrow_mut().take();
            });
            LRESULT(0)
        }
        WM_APPS_READY => {
            update();
            LRESULT(0)
        }
        WM_ICON_READY => {
            paint();
            LRESULT(0)
        }
        WM_CHAR => {
            let unit = wp.0 as u32;
            let mut changed = true;
            L.with(|l| {
                if let Some(s) = l.borrow_mut().as_mut() {
                    // Characters outside the BMP arrive as two WM_CHARs (high then low surrogate).
                    let ch = match unit {
                        0xD800..=0xDBFF => {
                            s.pending_high = Some(unit as u16);
                            changed = false;
                            return;
                        }
                        0xDC00..=0xDFFF => match s.pending_high.take() {
                            Some(hi) => char::decode_utf16([hi, unit as u16])
                                .next()
                                .and_then(|r| r.ok()),
                            None => None, // stray low surrogate
                        },
                        _ => {
                            s.pending_high = None;
                            char::from_u32(unit)
                        }
                    };
                    match ch {
                        Some('\u{8}') => {
                            s.query.pop();
                        }
                        Some('\u{7f}') => {
                            let t = s.query.trim_end().len();
                            let cut = s.query[..t].rfind(' ').map(|i| i + 1).unwrap_or(0);
                            s.query.truncate(cut);
                        }
                        Some(c) if c >= ' ' && s.query.chars().count() < MAX_QUERY => {
                            s.query.push(c)
                        }
                        _ => {}
                    }
                    s.sel = 0;
                }
            });
            if changed {
                update();
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = GetKeyState(VK_CONTROL.0 as i32) < 0;
            match VIRTUAL_KEY(wp.0 as u16) {
                VK_ESCAPE => close(),
                VK_RETURN => {
                    let i = L.with(|l| l.borrow().as_ref().map(|s| s.sel)).unwrap_or(0);
                    activate(i);
                }
                VK_DOWN | VK_TAB => {
                    L.with(|l| {
                        if let Some(s) = l.borrow_mut().as_mut() {
                            s.sel = (s.sel + 1) % s.items.len().max(1);
                        }
                    });
                    paint();
                }
                VK_UP => {
                    L.with(|l| {
                        if let Some(s) = l.borrow_mut().as_mut() {
                            let n = s.items.len().max(1);
                            s.sel = (s.sel + n - 1) % n;
                        }
                    });
                    paint();
                }
                VIRTUAL_KEY(0x56) if ctrl => {
                    if let Some(t) = crate::runtime::paste_text() {
                        L.with(|l| {
                            if let Some(s) = l.borrow_mut().as_mut() {
                                // Cap the total query (existing + pasted) at MAX_QUERY characters.
                                let room = MAX_QUERY.saturating_sub(s.query.chars().count());
                                s.query
                                    .extend(t.chars().filter(|c| !c.is_control()).take(room));
                                s.pending_high = None;
                                s.sel = 0;
                            }
                        });
                        update();
                    }
                }
                _ => return DefWindowProcW(h, msg, wp, lp),
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let k = scale(h);
            let (x, y) = ((lp.0 as i16) as f32 / k, ((lp.0 >> 16) as i16) as f32 / k);
            let hv = L.with(|l| {
                l.borrow().as_ref().and_then(|s| {
                    s.hits
                        .iter()
                        .find(|(r, _)| r.contains(x, y))
                        .map(|(_, i)| *i)
                })
            });
            let changed = L.with(|l| {
                l.borrow_mut()
                    .as_mut()
                    .map(|s| {
                        let c = s.hover != hv;
                        s.hover = hv;
                        c
                    })
                    .unwrap_or(false)
            });
            if changed {
                paint();
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let k = scale(h);
            let (x, y) = ((lp.0 as i16) as f32 / k, ((lp.0 >> 16) as i16) as f32 / k);
            let hit = L.with(|l| {
                l.borrow().as_ref().and_then(|s| {
                    s.hits
                        .iter()
                        .find(|(r, _)| r.contains(x, y))
                        .map(|(_, i)| *i)
                })
            });
            if let Some(i) = hit {
                activate(i);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}
