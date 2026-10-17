//! The shell runtime: one process, one UI thread. Owns the top bar (with its screen reservation),
//! the smart capsule, the auto-hiding left bar and the side panel; routes mouse/keyboard; keeps the
//! data model current with event-driven sources plus one 3 s metric timer. Pure Rust + Windows APIs.
use crate::capsule;
use crate::config::{self, Store};
use crate::leftbar::{self, Tab};
use crate::panel::{self, Input};
use crate::render::{self, Color, Rect};
use crate::subsystems;
use crate::sysctl;
use crate::theme::Tokens;
use crate::topbar;
use monarch_core::local_data::{Clipboard, Tasks};
use monarch_core::usage::{self, Alert, HotDetector, Usage};
use std::cell::RefCell;
use std::time::Instant;
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::*,
        System::{DataExchange::*, LibraryLoader::GetModuleHandleW, Memory::*},
        UI::{
            Accessibility::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*,
            WindowsAndMessaging::*,
        },
    },
};

// timers
const T_CLOCK: usize = 2;
const T_CARET: usize = 3;
const T_ANIM: usize = 4;
const T_REQUEST: usize = 5;
const T_LEFTBAR_HIDE: usize = 6;
const T_TEST_EXIT: usize = 7;
const T_TEST_STEP: usize = 8;
const T_FOCUS: usize = 9;

thread_local! { static TEST_STEPS: RefCell<Vec<(u64, Cmd)>> = const { RefCell::new(Vec::new()) }; }

/// Validation script: MONARCH_TEST_STEPS="1500:panel:dashboard,4000:capsule:weather,6000:launcher,7000:close"
fn parse_steps(s: &str) -> Vec<(u64, Cmd)> {
    s.split(',')
        .filter_map(|step| {
            let mut it = step.splitn(3, ':');
            let ms = it.next()?.trim().parse().ok()?;
            let cmd = match (it.next()?, it.next()) {
                ("panel", Some(t)) => Cmd::Panel(
                    *Tab::ORDER
                        .iter()
                        .find(|x| format!("{x:?}").eq_ignore_ascii_case(t))?,
                ),
                ("capsule", Some(t)) => Cmd::Capsule(
                    *capsule::Tab::ALL
                        .iter()
                        .find(|x| format!("{x:?}").eq_ignore_ascii_case(t))?,
                ),
                ("launcher", _) => Cmd::Launcher,
                ("help", _) => Cmd::Help,
                ("close", _) => Cmd::PanelClose,
                _ => return None,
            };
            Some((ms, cmd))
        })
        .collect()
}
// messages
pub const WM_SHELL_FG: u32 = WM_APP + 41;
pub const WM_SHELL_CMD: u32 = WM_APP + 42;
pub const WM_SHELL_EDGE: u32 = WM_APP + 43;
pub const WM_SHELL_CLICK: u32 = WM_APP + 44;
const WM_APPBAR: u32 = WM_APP + 45;
const WM_SHELL_RELAYOUT: u32 = WM_APP + 54;
const WM_SHELL_REAPPBAR: u32 = WM_APP + 55;
const T_CLIP: usize = 10;
const TME_LEAVE: TRACKMOUSEEVENT_FLAGS = TRACKMOUSEEVENT_FLAGS(2);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Bar,
    Capsule,
    Left,
    Panel,
}

/// Everything the user can trigger from a key binding or a surface.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Panel(Tab),
    PanelClose,
    Capsule(capsule::Tab),
    Launcher,
    Launch(String),
    Help,
    Reload,
    Wm(String),
    Sleep,
    Capture,
}

struct Anim {
    start: Instant,
    ms: f32,
    from: f32,
    to: f32,
}
impl Anim {
    fn value(&self, ease: fn(f32) -> f32) -> (f32, bool) {
        let t = (self.start.elapsed().as_secs_f32() * 1000. / self.ms).clamp(0., 1.);
        (self.from + (self.to - self.from) * ease(t), t >= 1.)
    }
}
fn ease_cubic(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}
fn ease_back(t: f32) -> f32 {
    let (c1, c3) = (1.25, 2.25);
    1. + c3 * (t - 1.).powi(3) + c1 * (t - 1.).powi(2)
}

struct Shell {
    store: Store,
    tokens: Tokens,
    bar: HWND,
    capsule: HWND,
    left: HWND,
    panel: HWND,
    tip: HWND,
    // models
    top: topbar::State,
    cap: capsule::State,
    lb: leftbar::State,
    pn: panel::State,
    tasks: Tasks,
    tasks_path: std::path::PathBuf,
    clipboard: Clipboard,
    usage: Usage,
    usage_dirty: bool,
    alerts: Vec<Alert>,
    hot: HotDetector,
    alert_pending: Option<(f32, f32)>,
    data_dir: std::path::PathBuf,
    focus: Option<monarch_core::focus::FocusState>,
    osd: crate::osd::Triggers,
    // runtime state
    net_before: Option<(Instant, u64, u64)>,
    fg: HWND,
    hits_bar: Vec<(Rect, topbar::Hit)>,
    hits_cap: Vec<(Rect, capsule::Hit)>,
    hits_left: Vec<(Rect, Tab)>,
    hits_panel: Vec<(Rect, panel::Hit)>,
    panel_max_scroll: f32,
    panel_anim: Option<Anim>,
    panel_slide: f32,
    cap_anim: Option<Anim>,
    cap_morph: f32,
    cap_island_w: f32,
    left_shown: bool,
    edge_near: bool,
    request_until: Option<Instant>,
    caret_on: bool,
    started: Instant,
}
thread_local! { static SHELL: RefCell<Option<Shell>> = const { RefCell::new(None) }; }
/// Borrow the shell state. Re-entrant calls (a sent message dispatched while a borrow is active, e.g.
/// during a cross-process SendMessage) get `None` instead of panicking inside a window procedure.
fn with<T>(f: impl FnOnce(&mut Shell) -> T) -> Option<T> {
    SHELL.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

static ROOT: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
fn root() -> HWND {
    HWND(ROOT.load(std::sync::atomic::Ordering::Relaxed) as *mut _)
}

fn scale(h: HWND) -> f32 {
    unsafe { GetDpiForWindow(h).max(96) as f32 / 96. }
}
fn screen() -> (i32, i32) {
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

fn accent_of(cfg: &config::ShellConfig) -> Color {
    Color::hex(if cfg.accent.starts_with('#') {
        &cfg.accent
    } else {
        config::rank_color(cfg.rank)
    })
}

// ── painting ─────────────────────────────────────────────────────────────
fn paint(role: Role) {
    with(|s| unsafe {
        let h = match role {
            Role::Bar => s.bar,
            Role::Capsule => s.capsule,
            Role::Left => s.left,
            Role::Panel => s.panel,
        };
        if h.is_invalid() {
            return;
        }
        let mut r = RECT::default();
        let _ = GetWindowRect(h, &mut r);
        let (w, ht) = (r.right - r.left, r.bottom - r.top);
        let k = scale(h);
        let (wd, hd) = (w as f32 / k, ht as f32 / k);
        let t = s.tokens;
        match role {
            Role::Bar => {
                let st = &s.top;
                let mut hits = vec![];
                let _ = render::present(h, w, ht, k, |c| hits = topbar::paint(c, wd, &t, st));
                s.hits_bar = hits;
            }
            Role::Left => {
                let st = &s.lb;
                let mut hits = vec![];
                let _ = render::present(h, w, ht, k, |c| hits = leftbar::paint(c, hd, &t, st));
                s.hits_left = hits;
            }
            Role::Panel => {
                s.pn.caret_visible = s.caret_on;
                let (st, slide) = (&s.pn, s.panel_slide);
                let mut out = (vec![], 0.);
                let _ = render::present(h, w, ht, k, |c| out = panel::paint(c, hd, &t, st, slide));
                s.hits_panel = out.0;
                s.panel_max_scroll = out.1;
                // content shrank (deleted tasks/alerts): never leave the view scrolled past the end
                if s.pn.scroll > s.panel_max_scroll {
                    s.pn.scroll = s.panel_max_scroll;
                }
            }
            Role::Capsule => {
                let st = &s.cap;
                let morph = s.cap_morph;
                let iw = s.cap_island_w;
                let mut hits = vec![];
                let _ = render::present(h, w, ht, k, |c| {
                    if morph <= 0.001 {
                        hits = capsule::paint_idle(c, wd, &t, st, iw);
                    } else {
                        // morph from the idle pill to the 410×220 card
                        let idle = Rect::new((wd - iw) / 2., 4., iw, capsule::IDLE_H);
                        let card = Rect::new(
                            (wd - capsule::CARD.0) / 2.,
                            capsule::CARD_TOP,
                            capsule::CARD.0,
                            capsule::CARD.1,
                        );
                        let lerp = |a: f32, b: f32| a + (b - a) * morph;
                        let frame = Rect::new(
                            lerp(idle.x, card.x),
                            lerp(idle.y, card.y),
                            lerp(idle.w, card.w),
                            lerp(idle.h, card.h),
                        );
                        hits = capsule::paint_card(
                            c,
                            wd,
                            &t,
                            st,
                            frame,
                            ((morph - 0.35) / 0.65).clamp(0., 1.),
                        );
                    }
                });
                s.hits_cap = hits;
            }
        }
    });
}
/// Left-bar tooltip (name + shortcut), a small click-through chip beside the hovered button.
fn show_tip(tab: Option<Tab>) {
    with(|s| unsafe {
        let Some(tab) = tab.filter(|_| s.left_shown) else {
            let _ = ShowWindow(s.tip, SW_HIDE);
            return;
        };
        let Some(r) = s.hits_left.iter().find(|(_, t)| *t == tab).map(|(r, _)| *r) else {
            return;
        };
        let text = tab.tooltip();
        let font = crate::theme::label(12., 600);
        let tw = render::measure_with(|c| c.measure(text, &font, 0.)).unwrap_or(120.) + 24.;
        let k = scale(s.tip);
        let mut lr = RECT::default();
        let _ = GetWindowRect(s.left, &mut lr);
        let (w, h) = ((tw * k) as i32, (28. * k) as i32);
        let y = lr.top + ((r.y + r.h / 2.) * k) as i32 - h / 2;
        let _ = SetWindowPos(
            s.tip,
            Some(HWND_TOPMOST),
            lr.right + (6. * k) as i32,
            y,
            w,
            h,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
        let t = s.tokens;
        let _ = render::present(s.tip, w, h, k, |c| {
            let rr = Rect::new(0., 0., tw, 28.);
            c.chip(rr, render::Radii::all(8.), t.surface_solid, Some(t.line));
            c.text(text, rr, &font, t.text, render::Align::Center);
        });
    });
}

fn repaint_all() {
    for r in [Role::Bar, Role::Capsule, Role::Left, Role::Panel] {
        paint(r);
    }
}

// ── data ─────────────────────────────────────────────────────────────────
/// One metrics sample, gathered off the UI thread (interface tables and the WLAN service are RPCs
/// that can stall for tens of milliseconds).
#[derive(Clone)]
struct Sample {
    snap: crate::platform::SystemSnapshot,
    net: Option<(u64, u64)>,
    ssid: Option<String>,
    at: Instant,
}
static SAMPLE: std::sync::Mutex<Option<Sample>> = std::sync::Mutex::new(None);
const WM_METRICS: u32 = WM_APP + 57;

/// Sampler thread: every 3 s, then posts WM_METRICS; ends when the shell window is gone.
fn start_metrics(notify: HWND) {
    let raw = notify.0 as isize;
    let _ = std::thread::Builder::new()
        .name("metrics".into())
        .spawn(move || {
            let mut times = None;
            let mut wlan: Option<HANDLE> = None;
            loop {
                let (snap, t) = crate::platform::system_snapshot(times);
                times = t;
                let sample = Sample {
                    snap,
                    net: crate::shell::net_totals(),
                    ssid: crate::shell::ssid(&mut wlan),
                    at: Instant::now(),
                };
                *SAMPLE.lock().unwrap_or_else(|e| e.into_inner()) = Some(sample);
                if unsafe {
                    PostMessageW(Some(HWND(raw as *mut _)), WM_METRICS, WPARAM(0), LPARAM(0))
                }
                .is_err()
                {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_secs(3));
            }
            if let Some(w) = wlan {
                unsafe { windows::Win32::NetworkManagement::WiFi::WlanCloseHandle(w, None) };
            }
        });
}

fn refresh_metrics(s: &mut Shell) {
    let Some(sample) = SAMPLE.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let snap = sample.snap;
    s.top.cpu = snap.cpu.map(|v| v as f32);
    s.top.ram = snap.memory.map(|v| v as f32);
    s.top.battery = snap.battery.map(|p| (p as f32, snap.charging));
    if let Some((p, ch)) = s.top.battery {
        if let Some(r) = s.osd.battery(p, ch) {
            post_request(r);
        }
    }
    s.pn.cpu = s.top.cpu;
    s.pn.ram = s.top.ram;
    s.pn.battery = snap.battery.map(|p| p as f32);
    s.pn.mem_text = format!("{:.1}G / {:.0}G", snap.used_gib, snap.total_gib);
    s.pn.threads = std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(0);
    let now = sample.at;
    match sample.net {
        Some((rx, tx)) => {
            s.top.connected = true;
            if let Some((at, orx, otx)) = s.net_before {
                let secs = now.duration_since(at).as_secs_f64();
                if secs > 0. && secs < 30. {
                    s.top.down_bps = Some(rx.saturating_sub(orx) as f64 / secs);
                    s.top.up_bps = Some(tx.saturating_sub(otx) as f64 / secs);
                }
                if secs < 30. {
                    s.usage.add(
                        &local_day(),
                        rx.saturating_sub(orx) as f64,
                        tx.saturating_sub(otx) as f64,
                        unix_ms(),
                    );
                    s.usage_dirty = true;
                    let (today, month) = s.usage.summary(&local_day());
                    s.pn.today_bytes = today.down + today.up;
                    s.pn.month_bytes = month.down + month.up;
                }
            }
            s.net_before = Some((now, rx, tx));
        }
        None => {
            s.top.connected = false;
            s.top.down_bps = None;
            s.top.up_bps = None;
            s.net_before = None;
        }
    }
    s.top.ssid = sample.ssid;
    s.pn.ssid = s.top.ssid.clone();
    s.pn.down_bps = s.top.down_bps;
    s.pn.up_bps = s.top.up_bps;
    if let (Some(c), Some(m)) = (s.top.cpu, s.top.ram) {
        if s.hot.sample(c as f64, m as f64, unix_ms()) {
            // attach the heaviest process, as the legacy alert did
            s.alert_pending = Some((c, m));
            subsystems::send(sysctl::Command::SampleProcesses);
        }
    }
}

/// Focus timer → capsule + dashboard. Returns whether a 1 s tick is needed (only while running).
fn apply_focus(s: &mut Shell) -> bool {
    let now = unix_ms();
    let f = s.focus.as_ref().and_then(|f| {
        f.remaining(now).map(|rem| capsule::Focus {
            remaining_ms: rem,
            total_ms: f.count.max(1.) as u64,
            running: f.running,
            on_break: f.on_break(),
            task: f.task.clone(),
        })
    });
    let ticking = f.as_ref().is_some_and(|x| x.running && x.remaining_ms > 0);
    s.cap.focus = f.clone();
    s.pn.focus = f;
    ticking
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
fn local_day() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay)
}
/// Read a data file. A file that exists but does not parse is moved aside as `.corrupt` (the last
/// good `.previous` copy is used instead) and is never silently replaced by defaults on the next save.
fn read_json<T: serde::de::DeserializeOwned + Default>(own: &std::path::Path) -> T {
    if let Ok(b) = std::fs::read(own) {
        match serde_json::from_slice(&b) {
            Ok(v) => return v,
            Err(_) => {
                let _ = std::fs::rename(own, own.with_extension("corrupt.json"));
                // fall back to the last good copy, if any
                if let Some(v) = std::fs::read(own.with_extension("previous.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice(&b).ok())
                {
                    return v;
                }
            }
        }
    }
    T::default()
}
/// Durable replace: flushed temp file, previous version kept, then an atomic rename over the target.
fn write_json(path: &std::path::Path, v: &impl serde::Serialize) {
    use std::io::Write;
    let Ok(bytes) = serde_json::to_vec(v) else {
        return;
    };
    let tmp = path.with_extension("tmp");
    let ok = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(&bytes)?;
        f.sync_all()
    });
    if ok.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if path.exists() {
        let _ = std::fs::copy(path, path.with_extension("previous.json"));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        if let Ok(p) = crate::platform::paths() {
            crate::platform::log(&p, &format!("save {}: {e}", path.display()));
        }
    }
}
fn sync_alerts(s: &mut Shell) {
    let now = unix_ms();
    s.pn.alerts = s
        .alerts
        .iter()
        .map(|a| panel::Alert {
            id: a.id,
            title: a.title.clone(),
            message: a.message.clone(),
            ago: usage::ago(now, a.time),
            perf: a.kind == "performance",
        })
        .collect();
    s.lb.alert_count = s.alerts.len();
}
fn save_alerts(s: &mut Shell) {
    write_json(&s.data_dir.join("alerts.json"), &s.alerts);
    sync_alerts(s);
    let newest = s
        .alerts
        .first()
        .map(|a| (a.id, a.title.as_str(), a.message.as_str()));
    if let Some(r) = s.osd.alert(newest) {
        post_request(r);
    }
}
fn save_usage(s: &mut Shell) {
    if s.usage_dirty {
        write_json(&s.data_dir.join("network-usage.json"), &s.usage);
        s.usage_dirty = false;
    }
}

fn refresh_window(s: &mut Shell) {
    let fg = unsafe { GetForegroundWindow() };
    // a window minimized by a workspace switch can stay "foreground" without being on screen
    let on_screen = |h: HWND| unsafe { !IsIconic(h).as_bool() };
    if crate::shell::is_app_window(fg) && !is_own(fg) && on_screen(fg) {
        s.fg = fg;
    } else if !on_screen(s.fg) {
        s.fg = HWND::default();
    } else if !is_own(fg)
        || unsafe { !IsWindow(Some(s.fg)).as_bool() || !IsWindowVisible(s.fg).as_bool() }
    {
        // desktop focused, or the remembered window closed/hidden: show the idle title
        s.fg = HWND::default();
    }
    let title = crate::shell::window_title(s.fg);
    s.top.window_title = if title.is_empty() {
        "Solo Leveling Shell".into()
    } else {
        title
    };
    s.top.window_icon = crate::appicons::for_window(s.fg);
    s.top.window_letter = crate::appicons::exe_of(s.fg).and_then(|p| {
        std::path::Path::new(&p).file_stem().map(|x| {
            x.to_string_lossy()
                .chars()
                .next()
                .unwrap_or('?')
                .to_ascii_uppercase()
        })
    });
}

fn refresh_clock(s: &mut Shell) {
    unsafe {
        let t = windows::Win32::System::SystemInformation::GetLocalTime();
        let (h12, ampm) = match t.wHour {
            0 => (12, "AM"),
            1..=11 => (t.wHour, "AM"),
            12 => (12, "PM"),
            h => (h - 12, "PM"),
        };
        s.cap.time = format!("{h12:02}:{:02} {ampm}", t.wMinute);
        const DAYS: [&str; 7] = [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ];
        const MONTHS: [&str; 12] = [
            "January",
            "February",
            "March",
            "April",
            "May",
            "June",
            "July",
            "August",
            "September",
            "October",
            "November",
            "December",
        ];
        s.cap.date = format!(
            "{}, {} {:02}",
            DAYS[t.wDayOfWeek as usize % 7],
            MONTHS[(t.wMonth as usize).saturating_sub(1) % 12],
            t.wDay
        );
        const AR_DAYS: [&str; 7] = [
            "الأحد",
            "الاثنين",
            "الثلاثاء",
            "الأربعاء",
            "الخميس",
            "الجمعة",
            "السبت",
        ];
        const AR_MONTHS: [&str; 12] = [
            "يناير",
            "فبراير",
            "مارس",
            "أبريل",
            "مايو",
            "يونيو",
            "يوليو",
            "أغسطس",
            "سبتمبر",
            "أكتوبر",
            "نوفمبر",
            "ديسمبر",
        ];
        let digits: String = t
            .wDay
            .to_string()
            .chars()
            .map(|c| char::from_u32(0x660 + c.to_digit(10).unwrap_or(0)).unwrap_or(c))
            .collect();
        s.pn.date_ar = format!(
            "{}، {} {}",
            AR_DAYS[t.wDayOfWeek as usize % 7],
            digits,
            AR_MONTHS[(t.wMonth as usize).saturating_sub(1) % 12]
        );
        s.pn.greeting = if t.wHour < 12 {
            "صباح الخير"
        } else if t.wHour < 18 {
            "مساء الخير"
        } else {
            "مساء النور"
        }
        .to_string();
        let name = s.store.config.user_name.trim();
        if !name.is_empty() {
            s.pn.greeting = format!("{}، {name}", s.pn.greeting);
        }
    }
}

fn sync_tasks(s: &mut Shell) {
    s.pn.tasks = s
        .tasks
        .items
        .iter()
        .map(|t| panel::Task {
            id: t.id,
            text: t.text.clone(),
            done: t.done,
            urgent: t.urgent,
        })
        .collect();
    s.lb.todo_count = s.tasks.items.iter().filter(|t| !t.done).count();
}
fn save_tasks(s: &mut Shell) {
    if let Err(e) = crate::platform::save_tasks(&s.tasks_path, &s.tasks) {
        if let Ok(p) = crate::platform::paths() {
            crate::platform::log(&p, &format!("tasks: {e}"));
        }
    }
    sync_tasks(s);
}

fn apply_config(s: &mut Shell) {
    let c = &s.store.config;
    s.tokens = Tokens::new(accent_of(c));
    s.pn.rank = c.rank;
    s.pn.ranks = config::RANKS
        .iter()
        .map(|(r, h)| (*r, Color::hex(h)))
        .collect();
    s.pn.preset = c.preset.clone();
    s.pn.rotate_hours = c.rotate_hours;
    s.pn.dynamic_accent = c.dynamic_accent;
    s.pn.reduce_motion = c.reduce_motion;
    s.pn.leftbar_autohide = c.leftbar_autohide;
    s.pn.focus_pulse = c.focus_pulse.clone();
    s.pn.rotate_enabled = c.rotate_enabled;
    s.pn.version = env!("CARGO_PKG_VERSION").into();
    s.pn.presets = crate::presets::names();
}

fn is_own(h: HWND) -> bool {
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(h, Some(&mut pid));
    }
    pid == std::process::id()
}

// ── capsule requests ─────────────────────────────────────────────────────
pub fn request(r: capsule::Request) {
    with(|s| {
        if s.cap
            .request
            .as_ref()
            .is_some_and(|q| r.priority < q.priority)
            && s.request_until.is_some_and(|u| u > Instant::now())
        {
            return;
        }
        let ms = r.timeout_ms;
        s.cap.request = Some(r);
        s.request_until = Some(Instant::now() + std::time::Duration::from_millis(ms as u64));
        unsafe {
            SetTimer(Some(s.capsule), T_REQUEST, ms, None);
        }
    });
    resize_capsule();
    paint(Role::Capsule);
}

/// Keep the capsule window just large enough for the island (no dead area over the bar).
fn resize_capsule() {
    with(|s| unsafe {
        let k = scale(s.capsule);
        let (sw, _) = screen();
        let expanded = s.cap.expanded || s.cap_morph > 0.;
        let (w, h) = if expanded {
            (440., 290.)
        } else {
            // measure with a throwaway snapshot-free canvas is not available here: estimate via the renderer
            let iw = render::measure_with(|c| capsule::idle_width(c, &s.cap)).unwrap_or(312.);
            s.cap_island_w = iw;
            (iw + 16., 32.)
        };
        let (pw, ph) = ((w * k) as i32, (h * k) as i32);
        let _ = SetWindowPos(
            s.capsule,
            None,
            (sw - pw) / 2,
            0,
            pw,
            ph,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    });
}

// ── panel / left bar ─────────────────────────────────────────────────────
fn open_panel(tab: Tab) {
    let was_open = with(|s| s.pn.tab.is_some() && s.panel_slide > 0.99).unwrap_or(false);
    with(|s| {
        if s.pn.tab == Some(tab) && was_open {
            return;
        }
        s.pn.tab = Some(tab);
        s.pn.scroll = 0.;
        s.pn.focused_input = None;
        s.lb.open = Some(tab);
        if tab == Tab::Todo {
            s.pn.focused_input = Some(Input::TodoText);
        }
        if tab == Tab::Clipboard {
            s.pn.focused_input = Some(Input::ClipSearch);
            s.pn.clip_sel = 0;
        }
        refresh_tab(s, tab);
        unsafe {
            let _ = ShowWindow(s.panel, SW_SHOWNOACTIVATE);
            focus_window(s.panel);
            if !was_open {
                s.panel_anim = Some(Anim {
                    start: Instant::now(),
                    ms: if s.store.config.reduce_motion {
                        1.
                    } else {
                        200.
                    },
                    from: s.panel_slide,
                    to: 1.,
                });
                SetTimer(Some(s.panel), T_ANIM, 16, None);
            }
        }
    });
    show_left(true);
    arm_caret();
    paint(Role::Panel);
    paint(Role::Left);
}
/// The caret blinks only while a text field has focus (no idle wake-ups otherwise).
fn arm_caret() {
    with(|s| unsafe {
        if s.pn.focused_input.is_some() && s.pn.tab.is_some() {
            s.caret_on = true;
            SetTimer(Some(s.panel), T_CARET, 530, None);
        }
    });
}

fn close_panel() {
    with(|s| unsafe {
        if s.pn.tab.is_none() {
            return;
        }
        s.lb.open = None;
        s.pn.focused_input = None;
        s.panel_anim = Some(Anim {
            start: Instant::now(),
            ms: if s.store.config.reduce_motion {
                1.
            } else {
                150.
            },
            from: s.panel_slide,
            to: 0.,
        });
        SetTimer(Some(s.panel), T_ANIM, 16, None);
    });
    paint(Role::Left);
    let near = with(|s| s.edge_near).unwrap_or(false);
    if !near {
        schedule_left_hide();
    }
}
fn toggle_panel(tab: Tab) {
    let same = with(|s| s.pn.tab == Some(tab) && s.lb.open == Some(tab)).unwrap_or(false);
    if same {
        close_panel()
    } else {
        open_panel(tab)
    }
}

fn show_left(show: bool) {
    with(|s| unsafe {
        let autohide = s.store.config.leftbar_autohide;
        let want = show || !autohide;
        if want == s.left_shown {
            return;
        }
        s.left_shown = want;
        let _ = ShowWindow(s.left, if want { SW_SHOWNOACTIVATE } else { SW_HIDE });
        if !want {
            let _ = ShowWindow(s.tip, SW_HIDE);
        }
    });
    paint(Role::Left);
}
fn schedule_left_hide() {
    with(|s| unsafe {
        SetTimer(Some(s.left), T_LEFTBAR_HIDE, 400, None);
    });
}

/// Fetch what a tab shows only when it opens (nothing is polled for closed tabs).
fn refresh_tab(s: &mut Shell, tab: Tab) {
    use sysctl::Command as C;
    match tab {
        Tab::Dashboard => {
            let (v, m) = subsystems::audio_state();
            s.pn.volume = v;
            s.pn.muted = m;
            subsystems::send(C::RefreshRadios);
            subsystems::send(C::RefreshBrightness);
            if s.pn.wallpapers.is_empty() && s.pn.banner.is_none() {
                subsystems::load_wallpapers(s.bar, &s.store.config);
            }
        }
        Tab::Network => {
            subsystems::send(C::RefreshWifi);
            subsystems::send(C::ScanWifi);
        }
        Tab::Monitoring => {
            subsystems::send(C::SampleProcesses);
            subsystems::send(C::RefreshDisks);
        }
        Tab::Weather => subsystems::send(C::WeatherRefresh),
        Tab::Ai => {
            s.pn.ai_status = subsystems::ai_status();
            s.pn.focused_input = Some(Input::AiPrompt);
        }
        _ => {}
    }
}

/// A background process may take the foreground right after input: tap Alt first.
unsafe fn focus_window(h: HWND) {
    let key = |up: bool| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_MENU,
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: crate::input_hooks::MARK,
            },
        },
    };
    SendInput(
        &[key(false), key(true)],
        std::mem::size_of::<INPUT>() as i32,
    );
    let _ = SetForegroundWindow(h);
}

// ── commands ─────────────────────────────────────────────────────────────
pub fn run_cmd(cmd: Cmd) {
    if debug_enabled() {
        if let Ok(p) = crate::platform::paths() {
            crate::platform::log(&p, &format!("cmd {cmd:?}"));
        }
    }
    match cmd {
        Cmd::Panel(tab) => toggle_panel(tab),
        Cmd::PanelClose => close_panel(),
        Cmd::Capsule(tab) => open_capsule(tab),
        Cmd::Launcher => crate::launcher::toggle(),
        Cmd::Launch(target) => {
            let _ = crate::platform::open(std::path::Path::new(&target));
        }
        Cmd::Help => crate::help::toggle(),
        Cmd::Reload => {
            reload_config();
        }
        Cmd::Wm(action) => crate::wm_bridge::action(&action),
        Cmd::Sleep => crate::wm_bridge::power("sleep"),
        Cmd::Capture => {
            let hide =
                with(|s| subsystems::ai_hit(&panel::Hit::AiCapture, &mut s.pn)).unwrap_or(false);
            if hide {
                close_panel();
            }
        }
    }
}

fn reload_config() {
    with(|s| {
        s.store = Store::open()
            .unwrap_or_else(|_| Store::at(std::env::temp_dir().join("monarch-shell.json")));
        apply_config(s);
        subsystems::set_accent(s.tokens.accent, s.store.config.rank);
        subsystems::set_glow_mode(&s.store.config.focus_pulse);
    });
    place_bar();
    let panel_open = with(|s| s.pn.tab.is_some()).unwrap_or(false);
    show_left(panel_open);
    repaint_all();
}

/// Window verbs aimed at our own surfaces (Win+Q, Alt+F4) dismiss them instead of destroying them.
pub fn dismiss_surfaces() {
    close_panel();
    if with(|s| s.cap.expanded).unwrap_or(false) {
        let t =
            with(|s| s.cap.tab.unwrap_or(capsule::Tab::Weather)).unwrap_or(capsule::Tab::Weather);
        open_capsule(t);
    }
}

fn open_capsule(tab: capsule::Tab) {
    with(|s| unsafe {
        let already = s.cap.expanded && s.cap.tab == Some(tab);
        if already {
            s.cap.expanded = false;
        } else {
            s.cap.expanded = true;
            s.cap.tab = Some(tab);
        }
        let to = if s.cap.expanded { 1. } else { 0. };
        s.cap_anim = Some(Anim {
            start: Instant::now(),
            ms: if s.store.config.reduce_motion {
                1.
            } else {
                340.
            },
            from: s.cap_morph,
            to,
        });
        SetTimer(Some(s.capsule), T_ANIM, 16, None);
    });
    resize_capsule();
    with(|s| unsafe {
        if s.cap.expanded {
            focus_window(s.capsule);
        }
    });
    paint(Role::Capsule);
}

fn save_config(s: &mut Shell) {
    if let Err(e) = s.store.save() {
        request_text(
            "x",
            &format!("Settings not saved: {e}"),
            capsule::Tone::Crit,
        );
    }
}
/// Work for the UI thread from any thread (hooks, subsystems) or from inside a `with` borrow.
/// Only a wake-up message crosses the message queue; the payload never travels as a raw pointer.
enum Inbound {
    Cmd(Cmd),
    Request(capsule::Request),
}
static INBOX: std::sync::Mutex<std::collections::VecDeque<Inbound>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
fn push(i: Inbound) {
    {
        let mut q = INBOX.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() >= 64 {
            q.pop_front();
        }
        q.push_back(i);
    }
    unsafe {
        let _ = PostMessageW(Some(root()), WM_SHELL_CMD, WPARAM(0), LPARAM(0));
    }
}
pub fn post_cmd(c: Cmd) {
    push(Inbound::Cmd(c));
}
fn post_request(r: capsule::Request) {
    push(Inbound::Request(r));
}
fn drain_inbox() {
    loop {
        let next = INBOX.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
        match next {
            Some(Inbound::Cmd(c)) => run_cmd(c),
            Some(Inbound::Request(r)) => request(r),
            None => break,
        }
    }
}
fn request_text(icon: &'static str, text: &str, tone: capsule::Tone) {
    post_request(capsule::Request {
        priority: 3,
        timeout_ms: 2500,
        icon,
        text: text.into(),
        progress: None,
        tone,
    });
}
fn on_panel_hit(hit: panel::Hit, x: f32) {
    use panel::Hit as H;
    match hit {
        H::Close => close_panel(),
        H::Focus(i) => {
            with(|s| s.pn.focused_input = Some(i));
            arm_caret();
        }
        H::Preset(name) => {
            with(|s| {
                s.store.config.preset = name.clone();
                s.store.config.accent = if name == "ShadowMonarch" {
                    config::rank_color(s.store.config.rank).into()
                } else {
                    crate::presets::accent(&name)
                        .unwrap_or(config::rank_color(s.store.config.rank))
                        .into()
                };
                apply_config(s);
                save_config(s);
                subsystems::set_accent(s.tokens.accent, s.store.config.rank);
            });
            repaint_all();
            request_text("crown", &format!("Theme · {name}"), capsule::Tone::Primary);
        }
        H::Rank(r) => {
            with(|s| {
                s.store.config.rank = r;
                s.store.config.accent = config::rank_color(r).into();
                apply_config(s);
                save_config(s);
            });
            with(|s| subsystems::set_accent(s.tokens.accent, s.store.config.rank));
            repaint_all();
        }
        H::Setting(key) => {
            with(|s| {
                let c = &mut s.store.config;
                match key {
                    "dynamicAccent" => c.dynamic_accent = !c.dynamic_accent,
                    "reduceMotion" => c.reduce_motion = !c.reduce_motion,
                    "leftbarAutohide" => c.leftbar_autohide = !c.leftbar_autohide,
                    "rotateEnabled" => c.rotate_enabled = !c.rotate_enabled,
                    "focusPulse" => {
                        let order = ["breath", "heartbeat", "static", "off"];
                        let i = order.iter().position(|m| *m == c.focus_pulse).unwrap_or(0);
                        c.focus_pulse = order[(i + 1) % 4].into();
                    }
                    "rotateHours" => {
                        let order = [0.25, 0.5, 1., 3., 6., 12., 24.];
                        let i = order
                            .iter()
                            .position(|h| (*h - c.rotate_hours).abs() < 0.01)
                            .unwrap_or(3);
                        c.rotate_hours = order[(i + 1) % order.len()];
                    }
                    _ => {}
                }
                apply_config(s);
                save_config(s);
            });
            if key == "leftbarAutohide" {
                place_bar();
                show_left(with(|s| s.pn.tab.is_some()).unwrap_or(false));
            }
            if key == "focusPulse" {
                with(|s| subsystems::set_glow_mode(&s.store.config.focus_pulse));
            }
            repaint_all();
        }
        H::TodoAdd => submit_todo(),
        H::TodoUrgentToggle => {
            with(|s| s.pn.todo_urgent = !s.pn.todo_urgent);
        }
        H::TodoDone(id) => {
            with(|s| {
                if let Some(t) = s.tasks.items.iter_mut().find(|t| t.id == id) {
                    t.done = !t.done;
                }
                save_tasks(s);
            });
        }
        H::TodoUrgent(id) => {
            with(|s| {
                if let Some(t) = s.tasks.items.iter_mut().find(|t| t.id == id) {
                    t.urgent = !t.urgent;
                }
                save_tasks(s);
            });
        }
        H::TodoDelete(id) => {
            with(|s| {
                s.tasks.items.retain(|t| t.id != id);
                save_tasks(s);
            });
        }
        H::ClipCopy(i) => {
            let text = with(|s| s.clipboard.items.get(i).cloned()).flatten();
            if let Some(t) = text {
                set_clipboard(&t);
                close_panel();
            }
        }
        H::ClipDelete(i) => {
            with(|s| {
                if i < s.clipboard.items.len() {
                    s.clipboard.items.remove(i);
                }
                s.pn.clipboard = s.clipboard.items.clone();
            });
        }
        H::ClipClear => {
            with(|s| {
                s.clipboard.items.clear();
                s.pn.clipboard.clear();
            });
        }
        H::Power(act) => {
            let needs = matches!(act, "logout" | "restart" | "shutdown");
            let armed = with(|s| s.pn.armed == Some(act)).unwrap_or(false);
            if needs && !armed {
                with(|s| s.pn.armed = Some(act));
            } else {
                with(|s| s.pn.armed = None);
                close_panel();
                crate::wm_bridge::power(act);
            }
        }
        H::Open(target) => {
            crate::wm_bridge::open(target);
        }
        H::WindowsNotifications => crate::wm_bridge::open("notifications"),
        H::WindowsSettings => crate::wm_bridge::open("settings"),
        H::WifiSettings => crate::wm_bridge::open("wifi"),
        H::AlertsClear => {
            with(|s| {
                s.alerts.clear();
                save_alerts(s);
            });
        }
        H::AlertDismiss(id) => {
            with(|s| {
                s.alerts.retain(|a| a.id != id);
                save_alerts(s);
            });
        }
        H::Net(ssid) => {
            with(|s| {
                s.pn.open_net = if s.pn.open_net.as_deref() == Some(&ssid) {
                    None
                } else {
                    Some(ssid)
                }
            });
        }
        H::Reload => reload_config(),
        H::Slider(key, track) => {
            let v = (((x - track.x) / track.w).clamp(0., 1.) * 100.).round() as u8;
            if key == "volume" {
                subsystems::set_volume(v);
                with(|s| s.pn.volume = Some(v));
            } else {
                subsystems::send(sysctl::Command::SetBrightness(v));
                with(|s| s.pn.brightness = Some(v));
            }
        }
        H::Mute => {
            subsystems::toggle_mute();
            with(|s| {
                let (v, m) = subsystems::audio_state();
                s.pn.volume = v;
                s.pn.muted = m;
            });
        }
        H::Radio(kind) => {
            let on = with(|s| if kind == "bluetooth" { s.pn.bt_on } else { s.pn.wifi_on } == Some(true)).unwrap_or(false);
            subsystems::radio(kind, on);
        }
        H::WifiToggle => {
            let on = with(|s| s.pn.wifi_on == Some(true)).unwrap_or(false);
            subsystems::radio("wifi", on);
        }
        H::Scan => subsystems::send(sysctl::Command::ScanWifi),
        H::Connect(ssid) => {
            subsystems::connect(&ssid);
            request_text(
                "wifi",
                &format!("Connecting · {ssid}"),
                capsule::Tone::Primary,
            );
        }
        H::Disconnect => subsystems::send(sysctl::Command::DisconnectWifi),
        H::Forget(ssid) => {
            subsystems::forget(&ssid);
            with(|s| s.pn.open_net = None);
        }
        H::Wallpaper(id) => {
            subsystems::set_wallpaper(&id);
            with(|s| {
                for w in &mut s.pn.wallpapers {
                    w.on = w.id == id;
                }
            });
        }
        H::NextWallpaper => subsystems::rotate_wallpaper(),
        H::Diagnose => {
            if let Ok(p) = crate::platform::paths() {
                let _ = crate::platform::open(&p.data);
            }
        }
        H::AiSaveKey
        | H::AiTest
        | H::AiSend
        | H::AiCapture
        | H::AiClear
        | H::AiTheme
        | H::AiDiagnose => {
            let hide = with(|s| subsystems::ai_hit(&hit, &mut s.pn)).unwrap_or(false);
            if let Some(accent) = subsystems::take_ai_accent() {
                with(|s| {
                    s.store.config.accent = accent;
                    s.store.config.preset = "Gemini".into();
                    s.store.config.dynamic_accent = false;
                    apply_config(s);
                    save_config(s);
                    subsystems::set_accent(s.tokens.accent, s.store.config.rank);
                });
                repaint_all();
            }
            if hide {
                close_panel();
            }
        }
    }
    paint(Role::Panel);
    paint(Role::Left);
}

fn submit_todo() {
    with(|s| {
        let text = s.pn.todo_input.trim().to_string();
        if text.is_empty() {
            return;
        }
        if s.tasks.add(&text) {
            if s.pn.todo_urgent {
                if let Some(t) = s.tasks.items.last_mut() {
                    t.urgent = true;
                }
            }
            s.pn.todo_input.clear();
            s.pn.todo_urgent = false;
            save_tasks(s);
        }
    });
}

// ── clipboard ────────────────────────────────────────────────────────────
/// One attempt, never sleeps on the UI thread: the caller retries from a timer when the writer
/// still holds the clipboard. `None` also for "do not record" content.
unsafe fn read_clipboard(owner: HWND) -> Option<String> {
    OpenClipboard(Some(owner)).ok()?;
    struct Close;
    impl Drop for Close {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseClipboard();
            }
        }
    }
    let _c = Close;
    // honour "do not record" markers (password managers)
    for name in [
        w!("ExcludeClipboardContentFromMonitorProcessing"),
        w!("Clipboard Viewer Ignore"),
    ] {
        let f = RegisterClipboardFormatW(name);
        if f != 0 && IsClipboardFormatAvailable(f).is_ok() {
            return None;
        }
    }
    // Windows' own history opt-out: a DWORD 0 means "never keep this"
    let f = RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory"));
    if f != 0 && IsClipboardFormatAvailable(f).is_ok() {
        if let Ok(h) = GetClipboardData(f) {
            let m = HGLOBAL(h.0);
            let v = GlobalLock(m) as *const u32;
            let deny = !v.is_null() && GlobalSize(m) >= 4 && *v == 0;
            let _ = GlobalUnlock(m);
            if deny {
                return None;
            }
        }
    }
    let data = GetClipboardData(13).ok()?;
    let mem = HGLOBAL(data.0);
    let p = GlobalLock(mem) as *const u16;
    if p.is_null() {
        return None;
    }
    // bounded by the allocation, not by trusting a terminator
    let cap = (GlobalSize(mem) / 2).min(20_000);
    let mut n = 0usize;
    while n < cap && *p.add(n) != 0 {
        n += 1;
    }
    let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
    let _ = GlobalUnlock(mem);
    Some(s)
}
fn set_clipboard(text: &str) {
    unsafe {
        let data = crate::platform::wide(text);
        let Ok(mem) = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2) else {
            return;
        };
        let p = GlobalLock(mem) as *mut u16;
        if p.is_null() {
            return;
        }
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        let _ = GlobalUnlock(mem);
        // the system owns `mem` only after SetClipboardData succeeds
        for attempt in 0..4 {
            if OpenClipboard(Some(root())).is_ok() {
                let _ = EmptyClipboard();
                let ok = SetClipboardData(13, Some(HANDLE(mem.0))).is_ok();
                let _ = CloseClipboard();
                if ok {
                    return;
                }
                break;
            }
            if attempt < 3 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        let _ = GlobalFree(Some(mem));
        request_text("x", "Clipboard is busy — try again", capsule::Tone::Warn);
    }
}

/// Record a clipboard change; if the writer still holds the clipboard, retry shortly from a timer.
unsafe fn capture_clipboard(h: HWND) {
    if OpenClipboard(Some(h)).is_err() {
        let tries = CLIP_TRIES.with(|t| {
            t.set(t.get() + 1);
            t.get()
        });
        if tries <= 5 {
            SetTimer(Some(h), T_CLIP, 40, None);
        }
        return;
    }
    let _ = CloseClipboard();
    if let Some(text) = read_clipboard(h) {
        if !text.trim().is_empty() && !text.trim_start().starts_with("AIza") {
            with(|s| {
                s.clipboard.push(text);
                s.pn.clipboard = s.clipboard.items.clone();
            });
            if with(|s| s.pn.tab == Some(Tab::Clipboard)).unwrap_or(false) {
                paint(Role::Panel);
            }
        }
    }
}
thread_local! { static CLIP_TRIES: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }

// ── input ────────────────────────────────────────────────────────────────
fn hit_point(h: HWND, l: LPARAM) -> (f32, f32) {
    let k = scale(h);
    ((l.0 as i16) as f32 / k, ((l.0 >> 16) as i16) as f32 / k)
}

fn on_char(c: char) {
    with(|s| {
        let Some(input) = s.pn.focused_input else {
            return;
        };
        let field = match input {
            Input::TodoText => &mut s.pn.todo_input,
            Input::ClipSearch => &mut s.pn.clip_query,
            Input::AiKey => &mut s.pn.ai_key_input,
            Input::AiPrompt => &mut s.pn.ai_prompt,
        };
        match c {
            '\u{8}' => {
                field.pop();
            }
            '\n' if input == Input::AiPrompt && field.len() < 8000 => field.push('\n'),
            c if !c.is_control() && field.len() < 8000 => field.push(c),
            _ => {}
        }
        if input == Input::ClipSearch {
            s.pn.clip_sel = 0;
        }
        s.caret_on = true;
    });
    paint(Role::Panel);
}

/// Paste into the focused field in one edit and one repaint (not one per character).
fn insert_text(t: &str) {
    with(|s| {
        let Some(input) = s.pn.focused_input else {
            return;
        };
        let multiline = input == Input::AiPrompt;
        let field = match input {
            Input::TodoText => &mut s.pn.todo_input,
            Input::ClipSearch => &mut s.pn.clip_query,
            Input::AiKey => &mut s.pn.ai_key_input,
            Input::AiPrompt => &mut s.pn.ai_prompt,
        };
        for c in t.chars().filter(|c| *c != '\r') {
            if field.len() >= 8000 {
                break;
            }
            match c {
                '\n' if multiline => field.push('\n'),
                '\n' | '\t' => field.push(' '),
                c if !c.is_control() => field.push(c),
                _ => {}
            }
        }
        if input == Input::ClipSearch {
            s.pn.clip_sel = 0;
        }
        s.caret_on = true;
    });
    paint(Role::Panel);
}

fn on_key(vk: u16) -> bool {
    let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
    let input = with(|s| s.pn.focused_input).flatten();
    match VIRTUAL_KEY(vk) {
        VK_ESCAPE => {
            close_panel();
            true
        }
        VK_RETURN => {
            match input {
                Some(Input::TodoText) => submit_todo(),
                Some(Input::ClipSearch) => {
                    let pick = with(|s| {
                        let q = s.pn.clip_query.to_lowercase();
                        s.clipboard
                            .items
                            .iter()
                            .filter(|x| q.is_empty() || x.to_lowercase().contains(&q))
                            .nth(s.pn.clip_sel)
                            .cloned()
                    })
                    .flatten();
                    if let Some(t) = pick {
                        set_clipboard(&t);
                        close_panel();
                    }
                }
                Some(Input::AiKey) => {
                    with(|s| subsystems::ai_hit(&panel::Hit::AiSaveKey, &mut s.pn));
                }
                Some(Input::AiPrompt) if ctrl => {
                    with(|s| subsystems::ai_hit(&panel::Hit::AiSend, &mut s.pn));
                }
                Some(Input::AiPrompt) => {
                    on_char('\n');
                }
                None => {}
            }
            paint(Role::Panel);
            true
        }
        VK_DOWN | VK_UP if input == Some(Input::ClipSearch) => {
            with(|s| {
                let q = s.pn.clip_query.to_lowercase();
                let n = s
                    .clipboard
                    .items
                    .iter()
                    .filter(|x| q.is_empty() || x.to_lowercase().contains(&q))
                    .count()
                    .max(1);
                s.pn.clip_sel = if vk == VK_DOWN.0 {
                    (s.pn.clip_sel + 1).min(n - 1)
                } else {
                    s.pn.clip_sel.saturating_sub(1)
                };
            });
            paint(Role::Panel);
            true
        }
        VIRTUAL_KEY(0x56) if ctrl && input.is_some() => {
            if let Some(t) = unsafe { read_clipboard(root()) } {
                insert_text(&t);
            }
            true
        }
        _ => false,
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let role = match GetWindowLongPtrW(h, GWLP_USERDATA) {
        1 => Some(Role::Bar),
        2 => Some(Role::Capsule),
        3 => Some(Role::Left),
        4 => Some(Role::Panel),
        _ => None,
    };
    match msg {
        WM_TIMER => {
            match w.0 {
                T_CLOCK => { with(|s| { refresh_clock(s); save_usage(s); if s.pn.tab == Some(Tab::Notifications) { sync_alerts(s); } }); arm_clock(h); resize_capsule(); paint(Role::Capsule); }
                T_CARET => { let on_panel = with(|s| { s.caret_on = !s.caret_on; s.pn.focused_input.is_some() && s.pn.tab.is_some() && s.panel_slide > 0. }).unwrap_or(false); if on_panel { paint(Role::Panel); } else { let _ = KillTimer(Some(h), T_CARET); } }
                T_CLIP => { let _ = KillTimer(Some(h), T_CLIP); capture_clipboard(h); }
                T_FOCUS => {
                    if !with(apply_focus).unwrap_or(false) { let _ = KillTimer(Some(h), T_FOCUS); }
                    paint(Role::Capsule);
                    if with(|s| s.pn.tab == Some(Tab::Dashboard)).unwrap_or(false) { paint(Role::Panel); }
                }
                T_TEST_STEP => {
                    let _ = KillTimer(Some(h), T_TEST_STEP);
                    let next = TEST_STEPS.with(|t| { let mut t = t.borrow_mut(); if t.is_empty() { None } else { Some(t.remove(0)) } });
                    if let Some((_, cmd)) = next {
                        run_cmd(cmd);
                        let up = with(|s| s.started.elapsed().as_millis() as u64).unwrap_or(0);
                        if let Some(at) = TEST_STEPS.with(|t| t.borrow().first().map(|x| x.0)) { SetTimer(Some(h), T_TEST_STEP, at.saturating_sub(up).max(10) as u32, None); }
                    }
                }
                T_TEST_EXIT => { report_usage(); let _ = DestroyWindow(h); }
                T_REQUEST => { let _ = KillTimer(Some(h), T_REQUEST); with(|s| { s.cap.request = None; s.request_until = None; }); resize_capsule(); paint(Role::Capsule); }
                T_LEFTBAR_HIDE => { let _ = KillTimer(Some(h), T_LEFTBAR_HIDE); let keep = with(|s| s.edge_near || s.pn.tab.is_some() && s.panel_slide > 0.).unwrap_or(true); if !keep { show_left(false); } }
                T_ANIM => {
                    match role {
                        Some(Role::Panel) => {
                            let done = with(|s| { if let Some(a) = &s.panel_anim { let (v, d) = a.value(ease_cubic); s.panel_slide = v; if d { s.panel_anim = None; } d } else { true } }).unwrap_or(true);
                            if done {
                                let _ = KillTimer(Some(h), T_ANIM);
                                let closed = with(|s| s.panel_slide <= 0.001).unwrap_or(false);
                                if closed { with(|s| s.pn.tab = None); let _ = ShowWindow(h, SW_HIDE); }
                            }
                            paint(Role::Panel);
                        }
                        Some(Role::Capsule) => {
                            let done = with(|s| { if let Some(a) = &s.cap_anim { let (v, d) = a.value(ease_back); let to = a.to; s.cap_morph = v.max(0.); if d { s.cap_anim = None; s.cap_morph = to; } d } else { true } }).unwrap_or(true);
                            if done { let _ = KillTimer(Some(h), T_ANIM); resize_capsule(); }
                            paint(Role::Capsule);
                        }
                        _ => { let _ = KillTimer(Some(h), T_ANIM); }
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = hit_point(h, l);
            match role {
                Some(Role::Bar) => {
                    let hit = with(|s| s.hits_bar.iter().find(|(r, _)| r.contains(x, y)).map(|(_, hh)| hh.clone())).flatten();
                    match hit {
                        Some(topbar::Hit::Network) => open_panel(Tab::Network),
                        Some(topbar::Hit::Monitor) => open_panel(Tab::Monitoring),
                        Some(topbar::Hit::Workspace(i)) => crate::wm_bridge::action(&format!("focus-workspace {i}")),
                        Some(topbar::Hit::TrayMore) => { with(|s| s.top.tray_expanded = !s.top.tray_expanded); paint(Role::Bar); }
                        Some(topbar::Hit::Tray(id)) => subsystems::tray_click(&id, false),
                        None => {}
                    }
                }
                Some(Role::Left) => { let hit = with(|s| s.hits_left.iter().find(|(r, _)| r.contains(x, y)).map(|(_, t)| *t)).flatten(); if let Some(t) = hit { toggle_panel(t); } }
                Some(Role::Panel) => { let hit = with(|s| s.hits_panel.iter().find(|(r, _)| r.contains(x, y)).map(|(_, hh)| hh.clone())).flatten(); if let Some(hh) = hit { on_panel_hit(hh, x); } }
                Some(Role::Capsule) => {
                    let hit = with(|s| s.hits_cap.iter().find(|(r, _)| r.contains(x, y)).map(|(_, hh)| hh.clone())).flatten();
                    match hit {
                        Some(capsule::Hit::Open(t) | capsule::Hit::SwitchTab(t)) => { let exp = with(|s| s.cap.expanded).unwrap_or(false); if exp { with(|s| s.cap.tab = Some(t)); paint(Role::Capsule); } else { open_capsule(t); } }
                        Some(capsule::Hit::OpenDefault) => { let t = with(|s| if s.cap.focus.as_ref().is_some_and(|f| f.running) { capsule::Tab::Focus } else if s.cap.media.is_some() { capsule::Tab::Media } else { capsule::Tab::Weather }).unwrap_or(capsule::Tab::Weather); open_capsule(t); }
                        Some(capsule::Hit::Close) => { let t = with(|s| s.cap.tab.unwrap_or(capsule::Tab::Weather)).unwrap_or(capsule::Tab::Weather); open_capsule(t); }
                        Some(capsule::Hit::OpenObsidian) => crate::wm_bridge::open("obsidian"),
                        Some(capsule::Hit::NextSession) => subsystems::next_session(),
                        Some(other) => subsystems::media(&other),
                        None => { if with(|s| s.cap.expanded).unwrap_or(false) { let t = with(|s| s.cap.tab.unwrap_or(capsule::Tab::Weather)).unwrap_or(capsule::Tab::Weather); open_capsule(t); } }
                    }
                }
                None => {}
            }
            LRESULT(0)
        }
        WM_RBUTTONUP if role == Some(Role::Bar) => {
            let (x, y) = hit_point(h, l);
            if let Some(Some(topbar::Hit::Tray(id))) = with(|s| s.hits_bar.iter().find(|(r, _)| r.contains(x, y)).map(|(_, hh)| hh.clone())) { subsystems::tray_click(&id, true); }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = hit_point(h, l);
            match role {
                Some(Role::Left) => {
                    let hv = with(|s| s.hits_left.iter().find(|(r, _)| r.contains(x, y)).map(|(_, t)| *t)).flatten();
                    if with(|s| { let ch = s.lb.hover != hv; s.lb.hover = hv; ch }).unwrap_or(false) { paint(Role::Left); show_tip(hv); }
                }
                Some(Role::Panel) => {
                    let hv = with(|s| s.hits_panel.iter().find(|(r, _)| r.contains(x, y)).map(|(_, hh)| hh.clone())).flatten();
                    if with(|s| { let ch = s.pn.hover != hv; s.pn.hover = hv; ch }).unwrap_or(false) { paint(Role::Panel); }
                }
                _ => {}
            }
            let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
            let _ = TrackMouseEvent(&mut tme);
            LRESULT(0)
        }
        0x02A3 /* WM_MOUSELEAVE */ => {
            if role == Some(Role::Left) { with(|s| s.lb.hover = None); paint(Role::Left); show_tip(None); }
            if role == Some(Role::Panel) { with(|s| s.pn.hover = None); paint(Role::Panel); }
            LRESULT(0)
        }
        WM_MOUSEWHEEL if role == Some(Role::Panel) => {
            let delta = ((w.0 >> 16) as u16 as i16) as f32;
            with(|s| s.pn.scroll = (s.pn.scroll - delta / 120. * 48.).clamp(0., s.panel_max_scroll));
            paint(Role::Panel);
            LRESULT(0)
        }
        WM_MOUSEWHEEL if role == Some(Role::Bar) => {
            let delta = ((w.0 >> 16) as u16 as i16) as i32;
            crate::wm_bridge::action(if delta < 0 { "cycle-workspace next" } else { "cycle-workspace previous" });
            LRESULT(0)
        }
        WM_CHAR if role == Some(Role::Panel) => {
            let unit = w.0 as u16;
            if (0xD800..0xDC00).contains(&unit) {
                HIGH_SURROGATE.with(|h| h.set(unit));
            } else {
                let high = HIGH_SURROGATE.with(|h| h.replace(0));
                let units: &[u16] = if (0xDC00..0xE000).contains(&unit) && high != 0 { &[high, unit] } else { &[unit] };
                for c in char::decode_utf16(units.iter().copied()).flatten() { on_char(c); }
            }
            LRESULT(0)
        }
        WM_KEYDOWN if role == Some(Role::Panel) || role == Some(Role::Capsule) => {
            if role == Some(Role::Capsule) {
                if w.0 as u16 == VK_ESCAPE.0 && with(|s| s.cap.expanded).unwrap_or(false) { let t = with(|s| s.cap.tab.unwrap_or(capsule::Tab::Weather)).unwrap_or(capsule::Tab::Weather); open_capsule(t); }
                return LRESULT(0);
            }
            if on_key(w.0 as u16) { LRESULT(0) } else { DefWindowProcW(h, msg, w, l) }
        }
        WM_CLIPBOARDUPDATE => { CLIP_TRIES.with(|t| t.set(0)); capture_clipboard(h); LRESULT(0) }
        subsystems::WM_SYSCTL => {
            let ch = with(|s| {
                let ch = subsystems::drain(&mut s.cap, &mut s.pn, &mut s.top);
                if let Some(b) = s.pn.brightness { if let Some(r) = s.osd.brightness(b) { post_request(r); } }
                if let Some(m) = &s.cap.media { if let Some(r) = s.osd.track(&m.title, &m.artist) { post_request(r); } }
                ch
            });
            if let Some(ch) = ch {
                if let Some(e) = ch.error { request_text("x", &e, capsule::Tone::Crit); }
                if with(|s| s.cap.request.is_some() && s.request_until.is_none()).unwrap_or(false) {
                    // a request raised by the update itself (track change)
                    with(|s| unsafe { s.request_until = Some(Instant::now() + std::time::Duration::from_millis(2500)); SetTimer(Some(s.capsule), T_REQUEST, 2500, None); });
                    resize_capsule();
                }
                if ch.procs {
                    with(|s| {
                        if let Some((c, m)) = s.alert_pending.take() {
                            let top = s.pn.procs.first().map(|p| format!(" · {}", p.name.trim_end_matches(".exe"))).unwrap_or_default();
                            let now = unix_ms();
                            usage::push_alert(&mut s.alerts, Alert { id: now, kind: "performance".into(), title: "استهلاك مرتفع".into(), message: format!("CPU {}% · RAM {}%{top}", c.round(), m.round()), time: now });
                            save_alerts(s);
                        }
                    });
                    paint(Role::Left);
                }
                if ch.bar { paint(Role::Bar); }
                if ch.capsule { paint(Role::Capsule); }
                if ch.panel && with(|s| s.pn.tab.is_some()).unwrap_or(false) { paint(Role::Panel); }
            }
            LRESULT(0)
        }
        subsystems::WM_WALLPAPER => {
            if let Some((image, accent, rotated)) = subsystems::take_wallpaper_event(l) {
                with(|s| {
                    let c = &mut s.store.config;
                    if let Some(i) = &image { c.wallpaper = i.to_string_lossy().into(); }
                    let shown = c.wallpaper.clone();
                    if let Some(r) = s.osd.wallpaper(&shown) { post_request(r); }
                    if let Some(at) = rotated { c.last_rotation_ms = at; }
                    if let (Some(a), true) = (accent, c.dynamic_accent) { c.accent = a.to_hex(); }
                    apply_config(s);
                    save_config(s);
                    let id = s.store.config.wallpaper.clone();
                    for w in &mut s.pn.wallpapers { w.on = w.id.eq_ignore_ascii_case(&id); }
                    subsystems::set_accent(s.tokens.accent, s.store.config.rank);
                    // new banner: reload now if the dashboard is visible, else on next open
                    s.pn.banner = None;
                    s.pn.wallpapers.clear();
                    if s.pn.tab == Some(Tab::Dashboard) { subsystems::load_wallpapers(s.bar, &s.store.config); }
                });
                repaint_all();
            }
            LRESULT(0)
        }
        subsystems::WM_THUMBS => {
            with(|s| { let cur = s.store.config.wallpaper.clone(); let (list, banner) = subsystems::take_thumbs(l, &cur); s.pn.wallpapers = list; s.pn.banner = banner; });
            if with(|s| s.pn.tab.is_some()).unwrap_or(false) { paint(Role::Panel); }
            LRESULT(0)
        }
        subsystems::WM_AI => {
            let reopen = with(|s| subsystems::ai_drain(&mut s.pn)).unwrap_or(false);
            if reopen { open_panel(Tab::Ai); } else if with(|s| s.pn.tab == Some(Tab::Ai)).unwrap_or(false) { paint(Role::Panel); }
            LRESULT(0)
        }
        subsystems::WM_TRAY => { with(|s| s.top.tray = subsystems::tray_icons()); paint(Role::Bar); LRESULT(0) }
        WM_METRICS => { with(refresh_metrics); if with(|s| s.pn.tab == Some(Tab::Monitoring) && s.panel_slide > 0.).unwrap_or(false) { subsystems::send(sysctl::Command::SampleProcesses); } paint(Role::Bar); if with(|s| s.pn.tab == Some(Tab::Monitoring) || s.pn.tab == Some(Tab::Network)).unwrap_or(false) { paint(Role::Panel); }; LRESULT(0) }
        subsystems::WM_AUDIO_DEVICE => {
            subsystems::audio_device_changed();
            let (v, m) = subsystems::audio_state();
            with(|s| { s.pn.volume = v; s.pn.muted = m; if let Some(v) = v { let _ = s.osd.volume(v, m); } });
            LRESULT(0)
        }
        subsystems::WM_AUDIO => {
            let (v, m) = subsystems::audio_state();
            with(|s| { s.pn.volume = v; s.pn.muted = m; if let Some(v) = v { if let Some(r) = s.osd.volume(v, m) { post_request(r); } } });
            if with(|s| s.pn.tab == Some(Tab::Dashboard)).unwrap_or(false) { paint(Role::Panel); }
            LRESULT(0)
        }
        crate::winkey_watch::WM_WINKEY => {
            if w.0 == 0 {
                // the keyboard controller dropped the Windows key: every Win shortcut is dead until it is unlocked
                request(capsule::Request { priority: 5, timeout_ms: 9000, icon: "lock", text: "Win key locked · press Fn+Win".into(), progress: None, tone: capsule::Tone::Crit });
                with(|s| {
                    let now = unix_ms();
                    usage::push_alert(&mut s.alerts, Alert { id: now, kind: "system".into(), title: "زرار Win مقفول".into(), message: "Fn+Win · أو Armoury Crate ← System Settings ← Win Key".into(), time: now });
                    save_alerts(s);
                });
                paint(Role::Left);
            }
            LRESULT(0)
        }
        crate::focus_watch::WM_FOCUS_STATE => {
            let new = crate::focus_watch::take(l);
            let mode = |f: &Option<monarch_core::focus::FocusState>| f.as_ref().filter(|x| x.remaining(unix_ms()).is_some()).map(|x| x.on_break());
            let (old_mode, new_mode) = with(|s| (mode(&s.focus), mode(&new))).unwrap_or((None, None));
            with(|s| s.focus = new);
            if let (Some(brk), true) = (new_mode, new_mode != old_mode) {
                request(capsule::Request { priority: 3, timeout_ms: 4000, icon: "timer", text: if brk { "Break time · استراحة".into() } else { "Focus session · ركّز".into() }, progress: None, tone: if brk { capsule::Tone::Ok } else { capsule::Tone::Primary } });
            }
            let tick = with(apply_focus).unwrap_or(false);
            if tick { SetTimer(Some(h), T_FOCUS, 1000, None); } else { let _ = KillTimer(Some(h), T_FOCUS); }
            resize_capsule();
            paint(Role::Capsule);
            LRESULT(0)
        }
        WM_SHELL_FG => { with(refresh_window); paint(Role::Bar); LRESULT(0) }
        crate::wm::WM_STATE_CHANGED => { with(refresh_workspaces); paint(Role::Bar); LRESULT(0) }
        WM_SHELL_EDGE => {
            let near = w.0 != 0;
            with(|s| s.edge_near = near);
            if near { show_left(true); } else if with(|s| s.pn.tab.is_none()).unwrap_or(true) { schedule_left_hide(); }
            LRESULT(0)
        }
        WM_SHELL_CLICK => {
            // click outside every shell surface closes the panel and the capsule card
            let (x, y) = ((l.0 & 0xffff) as i16 as i32, (l.0 >> 16) as i16 as i32);
            let target = WindowFromPoint(POINT { x, y });
            let root_target = GetAncestor(target, GA_ROOT);
            if !is_own(root_target) {
                let (panel_open, cap_open) = with(|s| (s.pn.tab.is_some(), s.cap.expanded)).unwrap_or((false, false));
                if panel_open { close_panel(); }
                if cap_open { let t = with(|s| s.cap.tab.unwrap_or(capsule::Tab::Weather)).unwrap_or(capsule::Tab::Weather); open_capsule(t); }
            }
            LRESULT(0)
        }
        WM_SHELL_CMD => { drain_inbox(); LRESULT(0) }

        WM_APPBAR => { if w.0 as u32 == ABN_POSCHANGED { place_bar(); } LRESULT(0) }
        WM_DISPLAYCHANGE | WM_DPICHANGED if role == Some(Role::Bar) => { let _ = PostMessageW(Some(h), WM_SHELL_RELAYOUT, WPARAM(0), LPARAM(0)); LRESULT(0) }
        WM_SHELL_REAPPBAR => { register_appbar(); place_bar(); LRESULT(0) }
        WM_SHELL_RELAYOUT => { place_bar(); resize_capsule(); repaint_all(); LRESULT(0) }
        WM_CLOSE if role != Some(Role::Bar) => { dismiss_surfaces(); LRESULT(0) }
        WM_QUERYENDSESSION => LRESULT(1),
        WM_ENDSESSION if role == Some(Role::Bar) && w.0 != 0 => { with(save_usage); crate::taskbar::restore(); LRESULT(0) }
        WM_DESTROY if role == Some(Role::Bar) => { PostQuitMessage(0); LRESULT(0) }
        _ => {
            let tc = TASKBAR_CREATED.load(std::sync::atomic::Ordering::Relaxed);
            if tc != 0 && msg == tc && role == Some(Role::Bar) { crate::taskbar::explorer_restarted(); let _ = PostMessageW(Some(h), WM_SHELL_REAPPBAR, WPARAM(0), LPARAM(0)); return LRESULT(0); }
            DefWindowProcW(h, msg, w, l)
        }
    }
}

static TASKBAR_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
thread_local! { static HIGH_SURROGATE: std::cell::Cell<u16> = const { std::cell::Cell::new(0) }; }
thread_local! { static LEFT_APPBAR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

unsafe fn arm_clock(h: HWND) {
    let t = windows::Win32::System::SystemInformation::GetLocalTime();
    let ms = (60 - t.wSecond as u32) * 1000 - t.wMilliseconds as u32 + 30;
    SetTimer(Some(h), T_CLOCK, ms.max(100), None);
}

// ── screen reservation (top 32 px; left 44 px when the left bar does not auto-hide) ──
fn appbar_data(h: HWND) -> APPBARDATA {
    APPBARDATA {
        cbSize: std::mem::size_of::<APPBARDATA>() as u32,
        hWnd: h,
        uCallbackMessage: WM_APPBAR,
        ..Default::default()
    }
}
fn register_appbar() {
    with(|s| unsafe {
        let mut d = appbar_data(s.bar);
        SHAppBarMessage(ABM_REMOVE, &mut d);
        let mut d = appbar_data(s.bar);
        SHAppBarMessage(ABM_NEW, &mut d);
    });
}
fn place_bar() {
    with(|s| unsafe {
        let k = scale(s.bar);
        let (sw, sh) = screen();
        let bh = (crate::theme::BAR_H * k).round() as i32;
        let mut d = appbar_data(s.bar);
        d.uEdge = ABE_TOP;
        d.rc = RECT {
            left: 0,
            top: 0,
            right: sw,
            bottom: bh,
        };
        SHAppBarMessage(ABM_QUERYPOS, &mut d);
        d.rc.bottom = d.rc.top + bh;
        SHAppBarMessage(ABM_SETPOS, &mut d);
        let _ = SetWindowPos(
            s.bar,
            Some(HWND_TOPMOST),
            d.rc.left,
            d.rc.top,
            sw,
            bh,
            SWP_NOACTIVATE,
        );
        let lw = (leftbar::WIDTH * k).round() as i32;
        let _ = SetWindowPos(
            s.left,
            Some(HWND_TOPMOST),
            0,
            bh,
            lw,
            sh - bh,
            SWP_NOACTIVATE,
        );
        // a pinned left bar reserves its column so tiled windows never sit underneath it
        let pinned = !s.store.config.leftbar_autohide;
        let registered = LEFT_APPBAR.with(|c| c.get());
        let mut d = appbar_data(s.left);
        if pinned {
            if !registered {
                SHAppBarMessage(ABM_NEW, &mut d);
                LEFT_APPBAR.with(|c| c.set(true));
            }
            d.uEdge = ABE_LEFT;
            d.rc = RECT {
                left: 0,
                top: bh,
                right: lw,
                bottom: sh,
            };
            SHAppBarMessage(ABM_QUERYPOS, &mut d);
            d.rc.right = d.rc.left + lw;
            SHAppBarMessage(ABM_SETPOS, &mut d);
        } else if registered {
            SHAppBarMessage(ABM_REMOVE, &mut d);
            LEFT_APPBAR.with(|c| c.set(false));
        }
        let pw = (panel::WIDTH * k).round() as i32;
        let _ = SetWindowPos(
            s.panel,
            Some(HWND_TOPMOST),
            (panel::X * k) as i32,
            (panel::Y * k) as i32,
            pw,
            sh - (panel::Y * k) as i32 - (15. * k) as i32,
            SWP_NOACTIVATE,
        );
    });
    crate::wm_bridge::work_area_changed();
}

unsafe extern "system" fn on_fg(
    _: HWINEVENTHOOK,
    ev: u32,
    h: HWND,
    obj: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    if ev == EVENT_OBJECT_NAMECHANGE && (obj != 0 || h != GetForegroundWindow()) {
        return;
    }
    let _ = PostMessageW(Some(root()), WM_SHELL_FG, WPARAM(0), LPARAM(0));
}

/// Current theme tokens (other surfaces such as the launcher paint with the same palette).
pub fn tokens() -> Tokens {
    with(|s| s.tokens).unwrap_or_else(|| Tokens::new(Color::hex("#62baf8")))
}
/// Activate one of our windows even though the shell is a background process.
pub fn bring_to_front(h: HWND) {
    unsafe { focus_window(h) }
}
pub fn copy_text(t: &str) {
    set_clipboard(t)
}
pub fn paste_text() -> Option<String> {
    unsafe { read_clipboard(root()) }
}

fn refresh_workspaces(s: &mut Shell) {
    s.top.workspaces = crate::wm_bridge::workspaces();
}

/// Self-measurement for validation runs (MONARCH_TEST_SECONDS): CPU time and private memory.
fn report_usage() {
    use windows::Win32::System::{ProcessStatus::*, Threading::*};
    unsafe {
        let p = GetCurrentProcess();
        let (mut c, mut e, mut k, mut u) = Default::default();
        let _ = GetProcessTimes(p, &mut c, &mut e, &mut k, &mut u);
        let ft =
            |t: FILETIME| (((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64) as f64 / 1e7;
        let mut m = PROCESS_MEMORY_COUNTERS_EX::default();
        let _ = GetProcessMemoryInfo(
            p,
            &mut m as *mut _ as *mut PROCESS_MEMORY_COUNTERS,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        );
        let up = with(|s| s.started.elapsed().as_secs_f64()).unwrap_or(1.);
        let line = format!(
            "usage: uptime {:.1}s cpu {:.3}s ({:.2}%) private {:.1} MiB working-set {:.1} MiB",
            up,
            ft(k) + ft(u),
            (ft(k) + ft(u)) / up * 100.
                / std::thread::available_parallelism()
                    .map(|n| n.get() as f64)
                    .unwrap_or(1.),
            m.PrivateUsage as f64 / 1048576.,
            m.WorkingSetSize as f64 / 1048576.
        );
        if let Ok(paths) = crate::platform::paths() {
            crate::platform::log(&paths, &line);
        }
    }
}

/// Verbose logging when %LOCALAPPDATA%\MonarchNext\debug exists (checked once).
pub fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| crate::platform::paths().is_ok_and(|p| p.data.join("debug").exists()))
}

/// Run the shell until the bar window closes.
pub fn run() -> std::io::Result<()> {
    let paths = crate::platform::paths()?;
    // a crash must never leave the user without a taskbar or screen space
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // only the UI thread owns the desktop changes; a worker panic is logged and that worker stops
        if std::thread::current().name() == Some("main") {
            crate::taskbar::restore();
        }
        if let Ok(p) = crate::platform::paths() {
            crate::platform::log(&p, &format!("panic: {info}"));
        }
        previous(info);
    }));
    let store = Store::open()?;
    let tasks_path = paths.data.join("todo.json");
    let tasks = crate::platform::load_tasks(&tasks_path).unwrap_or_default();
    let usage_store: Usage = read_json(&paths.data.join("network-usage.json"));
    let alerts: Vec<Alert> = read_json(&paths.data.join("alerts.json"));
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let module = GetModuleHandleW(None).map_err(std::io::Error::other)?;
        let class = w!("MonarchNext.Surface");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        });
        TASKBAR_CREATED.store(
            RegisterWindowMessageW(w!("TaskbarCreated")),
            std::sync::atomic::Ordering::Relaxed,
        );
        let mk = |title: PCWSTR, ex: WINDOW_EX_STYLE, role: isize| -> std::io::Result<HWND> {
            let h = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | ex,
                class,
                title,
                WS_POPUP,
                0,
                0,
                10,
                10,
                None,
                None,
                Some(module.into()),
                None,
            )
            .map_err(std::io::Error::other)?;
            SetWindowLongPtrW(h, GWLP_USERDATA, role);
            Ok(h)
        };
        let bar = mk(w!("Monarch Next · bar"), WS_EX_NOACTIVATE, 1)?;
        let capsule = mk(w!("Monarch Next · capsule"), WINDOW_EX_STYLE(0), 2)?;
        let left = mk(w!("Monarch Next · left bar"), WS_EX_NOACTIVATE, 3)?;
        let panel_w = mk(w!("Monarch Next · panel"), WINDOW_EX_STYLE(0), 4)?;
        let tip = mk(
            w!("Monarch Next · tooltip"),
            WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            5,
        )?;
        ROOT.store(bar.0 as isize, std::sync::atomic::Ordering::Relaxed);
        let mut sh = Shell {
            tokens: Tokens::new(accent_of(&store.config)),
            store,
            bar,
            capsule,
            left,
            panel: panel_w,
            top: topbar::State::default(),
            cap: capsule::State::default(),
            lb: leftbar::State::default(),
            pn: panel::State::default(),
            tip,
            tasks,
            tasks_path,
            clipboard: Clipboard::default(),
            usage: usage_store,
            usage_dirty: false,
            alerts,
            hot: HotDetector::default(),
            alert_pending: None,
            data_dir: paths.data.clone(),
            focus: None,
            osd: Default::default(),
            net_before: None,
            fg: HWND::default(),
            hits_bar: vec![],
            hits_cap: vec![],
            hits_left: vec![],
            hits_panel: vec![],
            panel_max_scroll: 0.,
            panel_anim: None,
            panel_slide: 0.,
            cap_anim: None,
            cap_morph: 0.,
            cap_island_w: 312.,
            left_shown: false,
            edge_near: false,
            request_until: None,
            caret_on: true,
            started: Instant::now(),
        };
        apply_config(&mut sh);
        sync_tasks(&mut sh);
        sync_alerts(&mut sh);
        let newest = sh
            .alerts
            .first()
            .map(|a| (a.id, a.title.clone(), a.message.clone()));
        let _ = sh
            .osd
            .alert(newest.as_ref().map(|a| (a.0, a.1.as_str(), a.2.as_str())));
        refresh_clock(&mut sh);
        SHELL.with(|s| *s.borrow_mut() = Some(sh));
        register_appbar();
        place_bar();
        resize_capsule();
        let _ = ShowWindow(bar, SW_SHOWNOACTIVATE);
        let _ = ShowWindow(capsule, SW_SHOWNOACTIVATE);
        show_left(false);
        repaint_all();
        let first_paint = with(|s| s.started.elapsed().as_millis()).unwrap_or(0);
        // slower sources (WLAN, interface tables, icons) after the first frame
        // slower sources (WLAN, interface tables, icons) after the first frame, off the UI thread
        start_metrics(bar);
        with(refresh_window);
        paint(Role::Bar);
        let _ = AddClipboardFormatListener(bar);
        arm_clock(capsule);
        let hooks = [
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                Some(on_fg),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            ),
            SetWinEventHook(
                EVENT_OBJECT_NAMECHANGE,
                EVENT_OBJECT_NAMECHANGE,
                None,
                Some(on_fg),
                0,
                0,
                WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
            ),
        ];
        if std::env::var_os("MONARCH_NO_HOOKS").is_none() {
            crate::input_hooks::start(root());
        }
        if let Some(secs) = std::env::var("MONARCH_TEST_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
        {
            SetTimer(Some(bar), T_TEST_EXIT, secs * 1000, None);
        }
        if let Ok(steps) = std::env::var("MONARCH_TEST_STEPS") {
            let steps = parse_steps(&steps);
            if let Some(first) = steps.first().map(|x| x.0) {
                SetTimer(Some(bar), T_TEST_STEP, first as u32, None);
            }
            TEST_STEPS.with(|t| *t.borrow_mut() = steps);
        }
        let wm_enabled = with(|s| s.store.config.window_manager).unwrap_or(true);
        crate::wm_bridge::start(root(), wm_enabled);
        crate::launcher::warm();
        crate::winkey_watch::start(bar);
        if let Some(f) = with(|s| crate::focus_watch::resolve(&s.store.config.focus_file)).flatten()
        {
            crate::focus_watch::start(bar, f);
        }
        let own_desktop = subsystems::desktop_owned(crate::wm_bridge::foreign_running());
        with(|s| subsystems::start(s.bar, &s.store.config, own_desktop));
        if own_desktop && with(|s| s.store.config.hide_taskbar).unwrap_or(true) {
            crate::taskbar::hide();
        }
        let (v, m) = subsystems::audio_state();
        with(|s| {
            s.pn.volume = v;
            s.pn.muted = m;
            if let Some(v) = v {
                let _ = s.osd.volume(v, m);
            }
            let w = s.store.config.wallpaper.clone();
            let _ = s.osd.wallpaper(&w);
        });
        with(refresh_workspaces);
        paint(Role::Bar);
        let started = with(|s| s.started.elapsed().as_millis()).unwrap_or(0);
        crate::platform::log(
            &paths,
            &format!("first paint {first_paint} ms, ready {started} ms"),
        );
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // orderly exit: give the screen space back, restore managed windows
        with(save_usage);
        crate::taskbar::restore();
        subsystems::stop();
        crate::wm_bridge::stop();
        crate::input_hooks::stop();
        for k in hooks {
            let _ = UnhookWinEvent(k);
        }
        with(|s| {
            let mut d = appbar_data(s.bar);
            SHAppBarMessage(ABM_REMOVE, &mut d);
            if LEFT_APPBAR.with(|c| c.replace(false)) {
                let mut d = appbar_data(s.left);
                SHAppBarMessage(ABM_REMOVE, &mut d);
            }
        });
        SHELL.with(|s| {
            s.borrow_mut().take();
        });
        render::shutdown();
        Ok(())
    }
}
