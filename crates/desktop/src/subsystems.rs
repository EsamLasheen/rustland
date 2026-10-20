//! Owns the background subsystems (system controls, wallpaper, active-window glow, audio) for the UI
//! thread and translates their updates into surface state. Every call here is non-blocking.
use crate::capsule;
use crate::config::ShellConfig;
use crate::glow;
use crate::panel;
use crate::render::Color;
use crate::sysctl::{
    self, Command, MediaAction, PlaybackStatus, RadioPower, RadioType, Sysctl, Update,
};
use crate::wallpaper;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use windows::Win32::{
    Foundation::*,
    Media::Audio::{
        Endpoints::{
            IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
        },
        *,
    },
    System::Com::*,
    UI::WindowsAndMessaging::*,
};

pub const WM_SYSCTL: u32 = WM_APP + 46;
pub const WM_WALLPAPER: u32 = WM_APP + 47;
pub const WM_THUMBS: u32 = WM_APP + 48;
pub const WM_AUDIO: u32 = WM_APP + 50;
pub const WM_TRAY: u32 = WM_APP + 51;
pub const WM_AI: u32 = WM_APP + 52;

/// Core Audio pushes volume/mute changes (any source: keys, mixer, apps) — no polling.
#[windows::core::implement(IAudioEndpointVolumeCallback)]
struct VolumeWatch(isize);
impl IAudioEndpointVolumeCallback_Impl for VolumeWatch_Impl {
    fn OnNotify(&self, _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> windows::core::Result<()> {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.0 as *mut _)), WM_AUDIO, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }
}

/// Default output switched (speakers → headphones…): the volume endpoint must follow it.
pub const WM_AUDIO_DEVICE: u32 = WM_APP + 56;
#[windows::core::implement(IMMNotificationClient)]
struct DeviceWatch(isize);
impl IMMNotificationClient_Impl for DeviceWatch_Impl {
    fn OnDeviceStateChanged(
        &self,
        _id: &windows::core::PCWSTR,
        _state: DEVICE_STATE,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &windows::core::PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &windows::core::PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(
        &self,
        flow: EDataFlow,
        role: ERole,
        _id: &windows::core::PCWSTR,
    ) -> windows::core::Result<()> {
        if flow == eRender && role == eMultimedia {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(self.0 as *mut _)),
                    WM_AUDIO_DEVICE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
        Ok(())
    }
    fn OnPropertyValueChanged(
        &self,
        _id: &windows::core::PCWSTR,
        _key: &windows::Win32::Foundation::PROPERTYKEY,
    ) -> windows::core::Result<()> {
        Ok(())
    }
}

/// Re-acquire the default output's volume control and its change notifications.
pub fn audio_device_changed() {
    S.with(|s| {
        let mut s = s.borrow_mut();
        if let (Some(a), Some(cb)) = (&s.audio, &s.audio_cb) {
            unsafe {
                let _ = a.UnregisterControlChangeNotify(cb);
            }
        }
        s.audio_cb = None;
        s.audio = unsafe { audio_endpoint().ok() };
        watch_audio(&mut s);
    });
}
#[derive(Default)]
struct Subs {
    sys: Option<Sysctl>,
    wall: Option<wallpaper::Handle>,
    glow: Option<glow::Handle>,
    tray: Option<crate::tray::Tray>,
    ai: Option<crate::ai::Ai>,
    proposal: Option<crate::ai::Palette>,
    capture_pending: bool,
    audio: Option<IAudioEndpointVolume>,
    audio_cb: Option<IAudioEndpointVolumeCallback>,
    devices: Option<(IMMDeviceEnumerator, IMMNotificationClient)>,
    root: isize,
    profiles: HashMap<String, String>,
    media_ids: Vec<String>,
    media_index: usize,
    wallpapers: Vec<PathBuf>,
}
thread_local! { static S: RefCell<Subs> = RefCell::new(Subs::default()); }

/// Desktop-changing subsystems stay off while another shell owns the desktop or in validation runs.
pub fn desktop_owned(foreign: bool) -> bool {
    !foreign && std::env::var_os("MONARCH_NO_DESKTOP").is_none()
}

pub fn start(root: HWND, cfg: &ShellConfig, own_desktop: bool) {
    let raw = root.0 as isize;
    S.with(|s| {
        let mut s = s.borrow_mut();
        match Sysctl::start(root, WM_SYSCTL) {
            Ok(sys) => {
                for c in [
                    Command::RefreshRadios,
                    Command::RefreshBrightness,
                    Command::RefreshWifi,
                    Command::MediaSubscribe(true),
                ] {
                    sys.send(c);
                }
                let loc = if cfg.weather_city.trim().is_empty() {
                    sysctl::LocationConfig::Auto
                } else {
                    sysctl::LocationConfig::City(cfg.weather_city.clone())
                };
                sys.send(Command::WeatherLocation(loc));
                sys.send(Command::WeatherSubscribe(true));
                s.sys = Some(sys);
            }
            Err(e) => log(&format!("system controls: {e}")),
        }
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            s.audio = audio_endpoint().ok();
            if let Ok(e) = CoCreateInstance::<_, IMMDeviceEnumerator>(
                &MMDeviceEnumerator,
                None,
                CLSCTX_INPROC_SERVER,
            ) {
                let client: IMMNotificationClient = DeviceWatch(raw).into();
                if e.RegisterEndpointNotificationCallback(&client).is_ok() {
                    s.devices = Some((e, client));
                }
            }
        }
        s.root = raw;
        match crate::ai::Ai::start(root, WM_AI) {
            Ok(ai) => {
                s.ai = Some(ai);
            }
            Err(e) => log(&format!("gemini: {e}")),
        }
        watch_audio(&mut s);
        if own_desktop && cfg.wallpaper_engine {
            let sink: wallpaper::EventSink = Box::new(move |ev| {
                let raw_ev = Box::into_raw(Box::new(ev)) as isize;
                unsafe {
                    if PostMessageW(
                        Some(HWND(raw as *mut _)),
                        WM_WALLPAPER,
                        WPARAM(0),
                        LPARAM(raw_ev),
                    )
                    .is_err()
                    {
                        drop(Box::from_raw(raw_ev as *mut wallpaper::Event));
                    }
                }
            });
            let accent = Color::hex(&cfg.accent);
            s.wall = Some(wallpaper::start(wallpaper::Config {
                image: (!cfg.wallpaper.is_empty()).then(|| PathBuf::from(&cfg.wallpaper)),
                accent,
                rank: cfg.rank,
                rank_root: rank_root(cfg),
                rotate_hours: if cfg.rotate_enabled {
                    cfg.rotate_hours
                } else {
                    0.
                },
                last_rotation_ms: cfg.last_rotation_ms,
                dynamic_accent: cfg.dynamic_accent,
                on_event: Some(sink),
                ..Default::default()
            }));
            s.glow = Some(glow::start_with(
                glow::Mode::parse(&cfg.focus_pulse),
                accent,
            ));
        }
        if own_desktop {
            // the tray host takes notification-area messages over from Explorer: only as the real shell
            match crate::tray::Tray::start(root, WM_TRAY) {
                Ok(t) => s.tray = Some(t),
                Err(e) => log(&format!("tray: {e}")),
            }
        }
    });
}

pub fn stop() {
    S.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(t) = s.tray.take() {
            t.stop();
        }
        if let Some(g) = s.glow.take() {
            g.stop();
        }
        if let Some(w) = s.wall.take() {
            w.stop();
        }
        s.sys.take();
        s.ai.take();
        if let (Some(a), Some(cb)) = (&s.audio, &s.audio_cb) {
            unsafe {
                let _ = a.UnregisterControlChangeNotify(cb);
            }
        }
        s.audio_cb.take();
        if let Some((e, c)) = s.devices.take() {
            unsafe {
                let _ = e.UnregisterEndpointNotificationCallback(&c);
            }
        }
        s.audio.take();
    });
}

fn log(m: &str) {
    if let Ok(p) = crate::platform::paths() {
        crate::platform::log(&p, m);
    }
}
fn rank_root(cfg: &ShellConfig) -> Option<PathBuf> {
    if cfg.rank_folders.is_empty() {
        wallpaper::default_rank_root()
    } else {
        Some(PathBuf::from(&cfg.rank_folders))
    }
}

pub fn send(c: Command) {
    S.with(|s| {
        if let Some(sys) = &s.borrow().sys {
            sys.send(c);
        }
    });
}

// ── audio (Core Audio endpoint on the UI thread; calls take microseconds) ──
unsafe fn audio_endpoint() -> windows::core::Result<IAudioEndpointVolume> {
    let e: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_INPROC_SERVER)?;
    e.GetDefaultAudioEndpoint(eRender, eMultimedia)?
        .Activate(CLSCTX_INPROC_SERVER, None)
}
fn with_audio<T>(f: impl Fn(&IAudioEndpointVolume) -> windows::core::Result<T>) -> Option<T> {
    S.with(|s| {
        let mut s = s.borrow_mut();
        if let Some(a) = &s.audio {
            if let Ok(v) = f(a) {
                return Some(v);
            }
        }
        // default device changed: re-acquire once (and follow the new device's changes)
        s.audio = unsafe { audio_endpoint().ok() };
        watch_audio(&mut s);
        s.audio.as_ref().and_then(|a| f(a).ok())
    })
}
pub fn audio_state() -> (Option<u8>, bool) {
    let v = with_audio(|a| unsafe { a.GetMasterVolumeLevelScalar() })
        .map(|v| (v.clamp(0., 1.) * 100.).round() as u8);
    let m = with_audio(|a| unsafe { a.GetMute() })
        .map(|b| b.as_bool())
        .unwrap_or(false);
    (v, m)
}
pub fn set_volume(pct: u8) {
    with_audio(|a| unsafe {
        a.SetMasterVolumeLevelScalar(pct.min(100) as f32 / 100., std::ptr::null())
    });
}
pub fn toggle_mute() {
    with_audio(|a| unsafe { a.SetMute(!a.GetMute()?.as_bool(), std::ptr::null()) });
}

fn watch_audio(s: &mut Subs) {
    if let (Some(a), Some(cb)) = (&s.audio, &s.audio_cb) {
        unsafe {
            let _ = a.UnregisterControlChangeNotify(cb);
        }
    }
    s.audio_cb = None;
    if let Some(a) = &s.audio {
        let cb: IAudioEndpointVolumeCallback = VolumeWatch(s.root).into();
        if unsafe { a.RegisterControlChangeNotify(&cb) }.is_ok() {
            s.audio_cb = Some(cb);
        }
    }
}

/// Notification-area icons for the top bar.
pub fn tray_icons() -> Vec<crate::topbar::TrayIcon> {
    S.with(|s| {
        s.borrow()
            .tray
            .as_ref()
            .map(|t| t.icons())
            .unwrap_or_default()
    })
    .into_iter()
    .map(|i| crate::topbar::TrayIcon {
        id: i.id,
        tooltip: i.tooltip,
        icon: Some(image(i.rgba, i.w, i.h)),
    })
    .collect()
}
pub fn tray_click(id: &str, right: bool) {
    let mut at = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut at);
    }
    S.with(|s| {
        if let Some(t) = &s.borrow().tray {
            t.click(
                id,
                if right {
                    crate::tray::Click::Right
                } else {
                    crate::tray::Click::Left
                },
                at,
            );
        }
    });
}

// ── Gemini ──
pub fn ai_status() -> String {
    S.with(|s| {
        s.borrow()
            .ai
            .as_ref()
            .map(|a| a.status().label().to_string())
            .unwrap_or_else(|| "UNAVAILABLE".into())
    })
}
fn ai_send(r: crate::ai::Request) {
    S.with(|s| {
        if let Some(a) = &s.borrow().ai {
            a.send(r);
        }
    });
}
/// Panel hit → assistant request. Returns true when the panel should hide (screen capture).
pub fn ai_hit(hit: &panel::Hit, pn: &mut panel::State) -> bool {
    use crate::ai::Request as R;
    match hit {
        panel::Hit::AiSaveKey => {
            ai_send(R::SaveKey(std::mem::take(&mut pn.ai_key_input)));
        }
        panel::Hit::AiTest => ai_send(R::TestKey),
        panel::Hit::AiSend => {
            let attach = S.with(|s| {
                s.borrow()
                    .ai
                    .as_ref()
                    .is_some_and(|a| a.status().capture_at.is_some())
            });
            ai_send(R::Ask {
                prompt: pn.ai_prompt.trim().to_string(),
                attach_capture: attach,
            });
        }
        panel::Hit::AiCapture => {
            S.with(|s| s.borrow_mut().capture_pending = true);
            ai_send(R::Capture);
            return true;
        }
        panel::Hit::AiClear => {
            pn.ai_prompt.clear();
            ai_send(R::Clear);
        }
        panel::Hit::AiTheme => {
            // a pending proposal is applied by pressing again (explicit apply, as in the legacy panel)
            if let Some(p) = S.with(|s| s.borrow_mut().proposal.take()) {
                pn.ai_response = format!("Applied · accent {}", p.accent);
                APPLY.with(|a| *a.borrow_mut() = Some(p.accent.clone()));
            } else {
                ai_send(R::ProposeTheme {
                    brief: pn.ai_prompt.trim().to_string(),
                });
            }
        }
        panel::Hit::AiDiagnose => ai_send(R::Diagnose {
            cpu: pn.cpu.unwrap_or(0.) as f64,
            memory: pn.ram.unwrap_or(0.) as f64,
        }),
        _ => {}
    }
    false
}
thread_local! { static APPLY: RefCell<Option<String>> = const { RefCell::new(None) }; }
/// Accent chosen from an applied AI theme proposal (taken once).
pub fn take_ai_accent() -> Option<String> {
    APPLY.with(|a| a.borrow_mut().take())
}

/// Assistant events → panel state. Returns true when a capture finished (reopen the AI panel).
pub fn ai_drain(pn: &mut panel::State) -> bool {
    use crate::ai::Event as E;
    let mut reopen = false;
    loop {
        let ev = S.with(|s| s.borrow().ai.as_ref().and_then(|a| a.try_recv()));
        let Some(ev) = ev else { break };
        match ev {
            E::Busy(b) => {
                pn.ai_busy = b;
                if b {
                    pn.ai_error.clear();
                    pn.ai_response.clear();
                }
            }
            E::KeySaved | E::ModelChanged(_) | E::Verified { .. } | E::CaptureCleared => {}
            E::Answer { text, .. } => pn.ai_response = text,
            E::ThemeProposal(p) => {
                pn.ai_response = format!(
                    "Proposed theme · accent {} · bg {} · surface {}\nPress THEME again to apply.",
                    p.accent, p.bg, p.surface
                );
                S.with(|s| s.borrow_mut().proposal = Some(p));
            }
            E::Captured { .. } => {
                pn.ai_response =
                    "Screenshot ready · it is sent only with your next question.".into();
                reopen = true;
            }
            E::CaptureFailed(e) => {
                pn.ai_error = e;
                reopen = true;
            }
            E::Cleared => {
                pn.ai_response.clear();
                pn.ai_error.clear();
                S.with(|s| s.borrow_mut().proposal = None);
            }
            E::Error(e) => pn.ai_error = e,
        }
    }
    pn.ai_status = ai_status();
    if reopen {
        S.with(|s| s.borrow_mut().capture_pending = false);
    }
    reopen
}

// ── panel / capsule actions ──
pub fn radio(kind: &str, currently_on: bool) {
    let kind = if kind == "bluetooth" {
        RadioType::Bluetooth
    } else {
        RadioType::WiFi
    };
    send(Command::SetRadio {
        kind,
        on: !currently_on,
    });
}
pub fn connect(ssid: &str) {
    let p = S
        .with(|s| s.borrow().profiles.get(ssid).cloned())
        .unwrap_or_else(|| ssid.into());
    send(Command::ConnectWifi { profile: p });
}
pub fn forget(ssid: &str) {
    let p = S
        .with(|s| s.borrow().profiles.get(ssid).cloned())
        .unwrap_or_else(|| ssid.into());
    send(Command::ForgetWifi { profile: p });
}
pub fn media(hit: &capsule::Hit) {
    let app = S.with(|s| {
        let s = s.borrow();
        s.media_ids.get(s.media_index).cloned()
    });
    let action = match hit {
        capsule::Hit::MediaPrev => MediaAction::Previous,
        capsule::Hit::MediaNext => MediaAction::Next,
        _ => MediaAction::PlayPause,
    };
    send(Command::Media {
        action,
        app_id: app,
    });
}
pub fn next_session() {
    S.with(|s| {
        let mut s = s.borrow_mut();
        let n = s.media_ids.len().max(1);
        s.media_index = (s.media_index + 1) % n;
    });
    send(Command::MediaSubscribe(true));
}
pub fn set_wallpaper(id: &str) {
    S.with(|s| {
        let s = s.borrow();
        if let (Some(w), Some(p)) = (
            &s.wall,
            s.wallpapers.iter().find(|p| p.to_string_lossy() == id),
        ) {
            w.set_image(p.clone());
        }
    });
}
pub fn rotate_wallpaper() {
    S.with(|s| {
        if let Some(w) = &s.borrow().wall {
            w.rotate_now();
        }
    });
}
/// Apply the rotation settings to the running engine (no restart needed).
pub fn set_rotation(enabled: bool, hours: f32) {
    S.with(|s| {
        if let Some(w) = &s.borrow().wall {
            w.set_rotation(if enabled { hours } else { 0. });
        }
    });
}
pub fn set_accent(c: Color, rank: char) {
    S.with(|s| {
        let s = s.borrow();
        if let Some(w) = &s.wall {
            w.set_accent(c);
            w.set_rank(rank);
        }
        if let Some(g) = &s.glow {
            g.set_color(c);
        }
    });
}
pub fn set_glow_mode(m: &str) {
    S.with(|s| {
        if let Some(g) = &s.borrow().glow {
            g.set_mode(glow::Mode::parse(m));
        }
    });
}

/// Wallpaper thumbnails for the dashboard: decoded off the UI thread, delivered via WM_THUMBS.
pub fn load_wallpapers(root: HWND, cfg: &ShellConfig) {
    let Some(rr) = rank_root(cfg) else { return };
    let dir = wallpaper::images_dir(&rr, cfg.rank);
    let current = (!cfg.wallpaper.is_empty()).then(|| PathBuf::from(&cfg.wallpaper));
    let raw = root.0 as isize;
    std::thread::spawn(move || {
        let list: Vec<PathBuf> = wallpaper::list_images(&dir).into_iter().take(12).collect();
        let current = current.or_else(|| list.first().cloned());
        let thumbs: Vec<(PathBuf, Option<Vec<u8>>)> = list
            .into_iter()
            .map(|p| {
                let t = wallpaper::thumbnail_rgba(&p, 160, 90).ok();
                (p, t)
            })
            .collect();
        let banner = current.and_then(|p| wallpaper::thumbnail_rgba(&p, BANNER.0, BANNER.1).ok());
        let b = Box::into_raw(Box::new(Thumbs {
            list: thumbs,
            banner,
        })) as isize;
        unsafe {
            if PostMessageW(Some(HWND(raw as *mut _)), WM_THUMBS, WPARAM(0), LPARAM(b)).is_err() {
                drop(Box::from_raw(b as *mut Thumbs));
            }
        }
    });
}
const BANNER: (u32, u32) = (640, 288);
struct Thumbs {
    list: Vec<(PathBuf, Option<Vec<u8>>)>,
    banner: Option<Vec<u8>>,
}
fn image(rgba: Vec<u8>, w: u32, h: u32) -> crate::topbar::Image {
    crate::topbar::Image {
        rgba: std::rc::Rc::new(rgba),
        w,
        h,
    }
}
/// (wallpaper strip, dashboard banner)
pub fn take_thumbs(
    l: LPARAM,
    current: &str,
) -> (Vec<panel::Wallpaper>, Option<crate::topbar::Image>) {
    let v = unsafe { *Box::from_raw(l.0 as *mut Thumbs) };
    S.with(|s| s.borrow_mut().wallpapers = v.list.iter().map(|(p, _)| p.clone()).collect());
    let list = v
        .list
        .into_iter()
        .map(|(p, t)| {
            let id = p.to_string_lossy().to_string();
            panel::Wallpaper {
                on: id.eq_ignore_ascii_case(current),
                thumb: t.map(|rgba| image(rgba, 160, 90)),
                id,
            }
        })
        .collect();
    (list, v.banner.map(|b| image(b, BANNER.0, BANNER.1)))
}

/// Wallpaper events → (image now shown, its Material source colour, rotation timestamp to persist).
/// (image now shown, its Material source colour, rotation time to persist)
pub type Shown = (Option<PathBuf>, Option<[u8; 3]>, Option<u64>);
pub fn take_wallpaper_event(l: LPARAM) -> Option<Shown> {
    match unsafe { *Box::from_raw(l.0 as *mut wallpaper::Event) } {
        wallpaper::Event::Applied {
            image,
            seed,
            rotated,
            at_ms,
            ..
        } => Some((image, seed, rotated.then_some(at_ms))),
        wallpaper::Event::Failed { image, error } => {
            log(&format!("wallpaper {}: {error}", image.display()));
            None
        }
    }
}

fn pretty_player(app_id: &str) -> String {
    let base = app_id.rsplit(['\\', '!']).next().unwrap_or(app_id);
    let base = base
        .trim_end_matches(".exe")
        .split('_')
        .next()
        .unwrap_or(base);
    let known = [
        ("spotify", "Spotify"),
        ("brave", "Brave"),
        ("chrome", "Chrome"),
        ("msedge", "Edge"),
        ("firefox", "Firefox"),
        ("zunemusic", "Media Player"),
        ("vlc", "VLC"),
    ];
    let l = base.to_lowercase();
    known
        .iter()
        .find(|(k, _)| l.contains(k))
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| base.to_string())
}

/// What changed, so the runtime repaints only the affected surfaces.
#[derive(Default)]
pub struct Changed {
    pub bar: bool,
    pub capsule: bool,
    pub panel: bool,
    pub procs: bool,
    pub error: Option<String>,
}

/// Drain every pending update into surface state.
pub fn drain(
    cap: &mut capsule::State,
    pn: &mut panel::State,
    top: &mut crate::topbar::State,
) -> Changed {
    let mut ch = Changed::default();
    loop {
        let u = S.with(|s| s.borrow().sys.as_ref().and_then(|x| x.try_recv()));
        let Some(u) = u else { break };
        match u {
            Update::Radios(r) => {
                pn.wifi_on = r.wifi.map(|p| p == RadioPower::On);
                pn.bt_on = r.bluetooth.map(|p| p == RadioPower::On);
                ch.panel = true;
            }
            Update::Brightness(b) => {
                pn.brightness = b.percent;
                ch.panel = true;
            }
            Update::Wifi(w) => {
                pn.ip = w.default_ipv4.map(|a| a.to_string()).unwrap_or_default();
                if w.available {
                    top.ssid = w.ssid.clone();
                    pn.ssid = w.ssid;
                }
                ch.panel = true;
                ch.bar = true;
            }
            Update::WifiNetworks(list) => {
                S.with(|s| {
                    let mut s = s.borrow_mut();
                    s.profiles = list
                        .iter()
                        .filter_map(|n| n.profile.clone().map(|p| (n.ssid.clone(), p)))
                        .collect();
                });
                pn.networks = Some(
                    list.into_iter()
                        .map(|n| panel::WifiNet {
                            ssid: n.ssid,
                            signal: n.signal,
                            secured: n.secured,
                            known: n.saved,
                            connected: n.connected,
                        })
                        .collect(),
                );
                ch.panel = true;
            }
            Update::Media(m) => {
                let ids: Vec<String> = m.sessions.iter().map(|x| x.app_id.clone()).collect();
                let idx = S.with(|s| {
                    let mut s = s.borrow_mut();
                    // keep the chosen session if it still exists, else follow the system's current one
                    let keep = s
                        .media_ids
                        .get(s.media_index)
                        .and_then(|id| ids.iter().position(|x| x == id));
                    s.media_index = keep
                        .or_else(|| m.sessions.iter().position(|x| x.is_current))
                        .unwrap_or(0);
                    s.media_ids = ids;
                    s.media_index
                });

                cap.media = m.sessions.get(idx).map(|x| capsule::Media {
                    player: pretty_player(&x.app_id),
                    title: x.title.clone(),
                    artist: x.artist.clone(),
                    playing: x.status == PlaybackStatus::Playing,
                    progress: match (x.position_ms, x.end_ms) {
                        (Some(p), Some(e)) if e > 0 => (p as f32 / e as f32).clamp(0., 1.),
                        _ => 0.,
                    },
                    sessions: m.sessions.len(),
                    index: idx,
                    can_prev: x.can_previous,
                    can_next: x.can_next,
                });
                cap.playing = cap.media.as_ref().is_some_and(|x| x.playing);

                ch.capsule = true;
            }
            Update::Weather(w) => {
                if let Some(r) = w.reading {
                    let wx = capsule::Weather {
                        status: r.status.unwrap_or("cloudy_day").into(),
                        celsius: r.celsius as f32,
                        fahrenheit: r.fahrenheit as f32,
                        wind_kmh: r.wind_kmh as f32,
                        is_day: r.is_day,
                    };
                    cap.weather = Some(wx.clone());
                    pn.weather = Some(wx);
                    let mins = w.age_secs.unwrap_or(0) / 60;
                    pn.updated = if mins == 0 {
                        "Updated just now".into()
                    } else {
                        format!("Updated {mins} min ago")
                    };
                }
                ch.capsule = true;
                ch.panel = true;
            }
            Update::Processes(p) => {
                pn.procs = p
                    .into_iter()
                    .map(|x| panel::Proc {
                        name: x.name,
                        count: x.instances,
                        cpu: x.cpu_percent,
                        mem_mb: (x.working_set_bytes / 1_048_576) as u32,
                    })
                    .collect();
                ch.panel = true;
                ch.procs = true;
            }
            Update::Disks(d) => {
                pn.disks = d
                    .into_iter()
                    .map(|x| panel::Disk {
                        name: x.root.trim_end_matches('\\').to_string(),
                        label: x.label,
                        free_gb: x.free_bytes as f32 / 1_073_741_824.,
                        used_pct: if x.total_bytes > 0 {
                            100. - x.free_bytes as f32 / x.total_bytes as f32 * 100.
                        } else {
                            0.
                        },
                    })
                    .collect();
                ch.panel = true;
            }
            Update::Action(a) => {
                if !a.ok {
                    ch.error = Some(format!("{}: {}", a.action, a.error.unwrap_or_default()));
                }
            }
        }
    }
    ch
}
