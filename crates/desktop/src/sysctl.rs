//! Native system controls for the shell, all in-process (no PowerShell, netsh,
//! external programs or browser runtime).
//!
//! One worker thread owns every Win32/WinRT/COM object (radios, brightness,
//! WLAN, media sessions, power, processes, disks); a second thread performs the
//! occasional blocking HTTPS weather request so it can never delay a toggle.
//!
//! ```ignore
//! let sys = Sysctl::start(hwnd, WM_APP + 30)?;      // io::Result<Sysctl>
//! sys.send(Command::RefreshRadios);                  // -> bool, never blocks
//! // in the window procedure, on WM_APP + 30:
//! while let Some(update) = sys.try_recv() { /* apply Update */ }
//! ```
//!
//! * `Sysctl::start(notify: HWND, msg: u32) -> io::Result<Sysctl>`
//! * `Sysctl::send(&self, command: Command) -> bool` — `false` when the bounded
//!   queue is full or the worker stopped; the command is dropped, never queued
//!   unboundedly.
//! * `Sysctl::try_recv(&self) -> Option<Update>` — drain until `None` after each
//!   `msg`. `msg` (WPARAM 0, LPARAM 0) is posted once when the mailbox goes from
//!   empty to non-empty. Snapshots coalesce (latest per kind wins); action
//!   results keep order and are capped at 16.
//!
//! Readings that cannot be obtained are reported as `None`/`available: false`;
//! nothing is fabricated. Change notifications (radio state, media sessions,
//! WLAN scan/connection, IPv4 address) are event-driven; the only timers are
//! the 600 ms process-CPU window and the 30 min weather refresh while subscribed.
#![allow(dead_code)] // Public API is consumed once the UI wires the module in.

use std::{
    collections::{HashMap, VecDeque},
    ffi::c_void,
    net::Ipv4Addr,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows::{
    core::{w, IInspectable, RuntimeType, BSTR, GUID, HRESULT, PCWSTR},
    Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState},
    Foundation::TypedEventHandler,
    Media::Control::{
        GlobalSystemMediaTransportControlsSession as Session,
        GlobalSystemMediaTransportControlsSessionManager as SessionManager,
        GlobalSystemMediaTransportControlsSessionPlaybackStatus as RawStatus,
        MediaPropertiesChangedEventArgs, PlaybackInfoChangedEventArgs, SessionsChangedEventArgs,
        TimelinePropertiesChangedEventArgs,
    },
    Win32::{
        Devices::Display::{
            DestroyPhysicalMonitors, GetMonitorBrightness, GetNumberOfPhysicalMonitorsFromHMONITOR,
            GetPhysicalMonitorsFromHMONITOR, SetMonitorBrightness, PHYSICAL_MONITOR,
        },
        Foundation::{
            CloseHandle, GetLastError, ERROR_NOT_ALL_ASSIGNED, FILETIME, HANDLE, HWND, LPARAM,
            LUID, NO_ERROR, RECT, WPARAM,
        },
        Graphics::Gdi::{EnumDisplayMonitors, HDC, HMONITOR},
        NetworkManagement::{
            IpHelper::{
                CancelMibChangeNotify2, GetAdaptersAddresses, GetBestInterface,
                NotifyUnicastIpAddressChange, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
                GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH, MIB_NOTIFICATION_TYPE,
                MIB_UNICASTIPADDRESS_ROW,
            },
            WiFi::*,
        },
        Networking::{
            WinHttp::{
                WinHttpCloseHandle, WinHttpConnect, WinHttpOpen, WinHttpOpenRequest,
                WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
                WinHttpSetTimeouts, INTERNET_DEFAULT_HTTPS_PORT,
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE,
                WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
            },
            WinSock::{AF_INET, SOCKADDR_IN},
        },
        Security::{
            AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES,
            SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
            TOKEN_QUERY,
        },
        Storage::FileSystem::{
            GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
        },
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoUninitialize,
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
            },
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            Power::SetSuspendState,
            ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
            Shutdown::{
                ExitWindowsEx, LockWorkStation, EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT,
                SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER, SHUTDOWN_REASON,
            },
            Threading::{
                GetCurrentProcess, GetProcessTimes, GetSystemTimes, OpenProcess, OpenProcessToken,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
            Variant::{VariantClear, VARIANT, VT_BSTR, VT_I2, VT_I4, VT_UI1, VT_UI2, VT_UI4},
            Wmi::{
                IWbemClassObject, IWbemContext, IWbemLocator, IWbemServices, WbemLocator,
                WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE,
            },
        },
        UI::WindowsAndMessaging::PostMessageW,
    },
};

const COMMAND_QUEUE: usize = 16;
const WEATHER_QUEUE: usize = 4;
const MAX_ACTION_RESULTS: usize = 16;
const MAX_SESSIONS: u32 = 8;
const MAX_RADIOS: u32 = 16;
const MAX_NETWORKS: usize = 64;
const MAX_PROCESSES: usize = 4096;
const MAX_MONITORS: usize = 16;
/// Number of process groups reported by [`Command::SampleProcesses`].
pub const TOP_PROCESSES: usize = 9;
const PROCESS_WINDOW: Duration = Duration::from_millis(600);
const WEATHER_REFRESH: Duration = Duration::from_secs(30 * 60);
const WEATHER_RETRY: Duration = Duration::from_secs(5 * 60);
const HTTP_MAX_BODY: usize = 256 * 1024;
const WINRT_TIMEOUT: Duration = Duration::from_secs(5);
const MEDIA_TIMEOUT: Duration = Duration::from_secs(2);
const WMI_TIMEOUT_MS: i32 = 3000;
// rpcdce.h values (avoids pulling in the whole Win32_System_Rpc feature).
const RPC_C_AUTHN_WINNT: u32 = 10;
const RPC_C_AUTHZ_NONE: u32 = 0;
const USER_AGENT: PCWSTR = w!("MonarchNext/0.5 (Windows)");

const DIRTY_RADIO: u32 = 1;
const DIRTY_MEDIA: u32 = 2;
const DIRTY_MEDIA_SESSIONS: u32 = 4;
const DIRTY_SCAN: u32 = 8;
const DIRTY_WIFI: u32 = 16;
/// Timeline-only change (position/seek): cheap for the app, expensive for us (a cross-process
/// snapshot of every session), so it is coalesced to at most one publish per MEDIA_TIMELINE_GAP.
const DIRTY_MEDIA_TIMELINE: u32 = 32;
const MEDIA_TIMELINE_GAP: Duration = Duration::from_secs(5);
/// Minimum spacing of network-triggered weather retries (flapping links).
const WEATHER_NET_RETRY_GAP: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Public data types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum RadioType {
    WiFi,
    Bluetooth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum RadioPower {
    On,
    Off,
    /// Hardware switch or airplane-mode policy prevents control.
    Disabled,
    Unknown,
}

/// `wifi`/`bluetooth` are `None` when no such radio exists.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct RadioSnapshot {
    pub available: bool,
    pub access_denied: bool,
    pub wifi: Option<RadioPower>,
    pub bluetooth: Option<RadioPower>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum BrightnessSource {
    /// Internal panel via `root\WMI` `WmiMonitorBrightness`.
    Wmi,
    /// External monitor via DDC/CI (Dxva2 monitor configuration).
    Ddc,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct BrightnessSnapshot {
    pub percent: Option<u8>,
    pub source: Option<BrightnessSource>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct WifiStatus {
    /// A WLAN interface exists and the WLAN service answered.
    pub available: bool,
    pub connected: bool,
    pub ssid: Option<String>,
    pub profile: Option<String>,
    pub signal: Option<u8>,
    /// IPv4 address of the interface holding the default route (any medium).
    pub default_ipv4: Option<Ipv4Addr>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct WifiNetwork {
    pub ssid: String,
    pub signal: u8,
    pub secured: bool,
    pub saved: bool,
    pub connected: bool,
    /// Saved profile name (use it for connect/forget); may differ from the SSID.
    pub profile: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub enum PlaybackStatus {
    Closed,
    Opened,
    Changing,
    Stopped,
    Playing,
    Paused,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct MediaSession {
    pub app_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub status: PlaybackStatus,
    /// Position at `timeline_updated_unix_ms`; extrapolate while `Playing`.
    pub position_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub timeline_updated_unix_ms: Option<i64>,
    pub can_play_pause: bool,
    pub can_next: bool,
    pub can_previous: bool,
    pub is_current: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct MediaSnapshot {
    pub available: bool,
    pub sessions: Vec<MediaSession>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaAction {
    PlayPause,
    Next,
    Previous,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Location {
    pub latitude: f64,
    pub longitude: f64,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum LocationConfig {
    /// IP geolocation (ipapi.co, falling back to get.geojs.io); resolved once and cached.
    #[default]
    Auto,
    /// Open-Meteo geocoding of a city name; resolved once and cached.
    City(String),
    Coordinates(Location),
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct WeatherReading {
    /// UI status key (`clear_day`, `heavy_rain_night`, ...); `None` for unmapped WMO codes.
    pub status: Option<&'static str>,
    pub weather_code: u32,
    pub celsius: f64,
    pub fahrenheit: f64,
    pub wind_kmh: f64,
    pub is_day: bool,
}

/// On failure `available` is false, `error` explains why, and `reading` holds
/// the last good value (if any) with its `age_secs`.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct WeatherState {
    pub available: bool,
    pub reading: Option<WeatherReading>,
    pub location: Option<Location>,
    pub fetched_unix: Option<u64>,
    pub age_secs: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub enum PowerAction {
    Lock,
    Sleep,
    Hibernate,
    SignOut,
    Restart,
    Shutdown,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ProcessUsage {
    pub name: String,
    /// Share of total machine CPU over the 600 ms window (0..=100).
    pub cpu_percent: f32,
    pub working_set_bytes: u64,
    pub instances: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct DiskInfo {
    pub root: String,
    pub label: String,
    pub filesystem: String,
    pub total_bytes: u64,
    /// Free bytes available to the current user (respects quotas).
    pub free_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ActionResult {
    pub action: &'static str,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Opens the radio API on first use and subscribes to state changes.
    RefreshRadios,
    SetRadio {
        kind: RadioType,
        on: bool,
    },
    RefreshBrightness,
    SetBrightness(u8),
    /// Posts [`Update::Wifi`]; afterwards connection/IP changes are pushed.
    RefreshWifi,
    /// Posts the cached list now and a fresh [`Update::WifiNetworks`] when the scan completes.
    ScanWifi,
    ConnectWifi {
        profile: String,
    },
    DisconnectWifi,
    ForgetWifi {
        profile: String,
    },
    /// `true` starts event-driven [`Update::Media`]; `false` drops all subscriptions.
    MediaSubscribe(bool),
    /// `app_id: None` targets the system's current session.
    Media {
        action: MediaAction,
        app_id: Option<String>,
    },
    /// While subscribed weather refreshes every 30 min (5 min retry after failure).
    WeatherSubscribe(bool),
    WeatherRefresh,
    WeatherLocation(LocationConfig),
    /// The caller is responsible for confirming destructive actions with the user.
    Power(PowerAction),
    /// Two-phase 600 ms sample; posts [`Update::Processes`] with the top 9 groups.
    SampleProcesses,
    RefreshDisks,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub enum Update {
    Radios(RadioSnapshot),
    Brightness(BrightnessSnapshot),
    Wifi(WifiStatus),
    WifiNetworks(Vec<WifiNetwork>),
    Media(MediaSnapshot),
    Weather(WeatherState),
    Processes(Vec<ProcessUsage>),
    Disks(Vec<DiskInfo>),
    Action(ActionResult),
}

const SLOTS: usize = 8;

impl Update {
    fn slot(&self) -> Option<usize> {
        Some(match self {
            Update::Radios(_) => 0,
            Update::Brightness(_) => 1,
            Update::Wifi(_) => 2,
            Update::WifiNetworks(_) => 3,
            Update::Media(_) => 4,
            Update::Weather(_) => 5,
            Update::Processes(_) => 6,
            Update::Disks(_) => 7,
            Update::Action(_) => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Handle, mailbox and shared state
// ---------------------------------------------------------------------------

enum Msg {
    Command(Command),
    Wake,
}

enum WeatherMsg {
    Subscribe(bool),
    Refresh,
    Location(LocationConfig),
    /// Connectivity / default IPv4 changed: retry at once if the last fetch failed.
    NetworkChanged,
}

#[derive(Default)]
struct Mailbox {
    latest: [Option<Update>; SLOTS],
    actions: VecDeque<ActionResult>,
    posted: bool,
}

struct Shared {
    mailbox: Mutex<Mailbox>,
    hwnd: isize,
    msg: u32,
    quit: AtomicBool,
    dirty: AtomicU32,
    wake: SyncSender<Msg>,
    /// The weather thread's queue (network-change nudges from the worker; quit wake-up on drop).
    weather: SyncSender<WeatherMsg>,
}

impl Shared {
    fn publish(&self, update: Update) {
        let post = {
            let mut mailbox = self.mailbox.lock().unwrap_or_else(|e| e.into_inner());
            match update.slot() {
                Some(slot) => mailbox.latest[slot] = Some(update),
                None => {
                    if let Update::Action(result) = update {
                        if mailbox.actions.len() >= MAX_ACTION_RESULTS {
                            mailbox.actions.pop_front();
                        }
                        mailbox.actions.push_back(result);
                    }
                }
            }
            !std::mem::replace(&mut mailbox.posted, true)
        };
        if post {
            let posted = unsafe {
                PostMessageW(
                    Some(HWND(self.hwnd as *mut c_void)),
                    self.msg,
                    WPARAM(0),
                    LPARAM(0),
                )
            };
            if posted.is_err() {
                // Allow a later publish to retry the notification.
                self.mailbox
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .posted = false;
            }
        }
    }

    fn action(&self, action: &'static str, result: Result<(), String>) {
        self.publish(Update::Action(ActionResult {
            action,
            ok: result.is_ok(),
            error: result.err(),
        }));
    }

    /// Called from event threads: flag the work and wake the worker at most once per burst.
    fn mark(&self, bit: u32) {
        if self.dirty.fetch_or(bit, Ordering::AcqRel) == 0 {
            let _ = self.wake.try_send(Msg::Wake);
        }
    }
}

/// Handle to the native system-control workers. Dropping it stops them
/// (asynchronously; the UI thread is never blocked).
pub struct Sysctl {
    shared: Arc<Shared>,
    weather: SyncSender<WeatherMsg>,
}

impl Sysctl {
    /// Spawns the workers. `msg` is posted to `notify` whenever updates are ready.
    pub fn start(notify: HWND, msg: u32) -> std::io::Result<Sysctl> {
        let (wake, commands) = mpsc::sync_channel(COMMAND_QUEUE);
        let (weather, weather_rx) = mpsc::sync_channel(WEATHER_QUEUE);
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox::default()),
            hwnd: notify.0 as isize,
            msg,
            quit: AtomicBool::new(false),
            dirty: AtomicU32::new(0),
            wake,
            weather: weather.clone(),
        });
        let worker_shared = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("sysctl".into())
            .spawn(move || worker(worker_shared, commands))?;
        let weather_shared = Arc::clone(&shared);
        if let Err(error) = std::thread::Builder::new()
            .name("sysctl-weather".into())
            .spawn(move || weather_worker(weather_shared, weather_rx))
        {
            shared.quit.store(true, Ordering::Release);
            let _ = shared.wake.try_send(Msg::Wake);
            let _ = weather.try_send(WeatherMsg::Refresh);
            return Err(error);
        }
        Ok(Sysctl { shared, weather })
    }

    /// Non-blocking. Returns `false` (and drops the command) when the bounded
    /// queue is full or the worker has stopped.
    pub fn send(&self, command: Command) -> bool {
        match command {
            Command::WeatherSubscribe(on) => {
                self.weather.try_send(WeatherMsg::Subscribe(on)).is_ok()
            }
            Command::WeatherRefresh => self.weather.try_send(WeatherMsg::Refresh).is_ok(),
            Command::WeatherLocation(config) => {
                self.weather.try_send(WeatherMsg::Location(config)).is_ok()
            }
            other => self.shared.wake.try_send(Msg::Command(other)).is_ok(),
        }
    }

    /// Next pending update; call repeatedly until `None` after each notification.
    pub fn try_recv(&self) -> Option<Update> {
        let mut mailbox = self
            .shared
            .mailbox
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for slot in mailbox.latest.iter_mut() {
            if let Some(update) = slot.take() {
                return Some(update);
            }
        }
        if let Some(result) = mailbox.actions.pop_front() {
            return Some(Update::Action(result));
        }
        mailbox.posted = false;
        None
    }
}

impl Drop for Sysctl {
    fn drop(&mut self) {
        self.shared.quit.store(true, Ordering::Release);
        let _ = self.shared.wake.try_send(Msg::Wake);
        // `Shared` holds a weather sender, so the channel never disconnects: wake the thread so
        // it observes `quit` (a full queue means it will wake anyway).
        let _ = self.weather.try_send(WeatherMsg::Refresh);
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

/// Waits for a WinRT async operation with a timeout (a hung app must not stall the worker).
macro_rules! wait {
    ($op:expr, $timeout:expr) => {{
        match $op {
            Ok(op) => {
                // Only a completion signal crosses threads; results are read here
                // (some WinRT collection types are not `Send`).
                let (tx, rx) = mpsc::sync_channel::<()>(1);
                if op
                    .when(move |_| {
                        let _ = tx.try_send(());
                    })
                    .is_ok()
                {
                    match rx.recv_timeout($timeout) {
                        Ok(()) => op.GetResults().ok(),
                        Err(_) => {
                            let _ = op.Cancel();
                            None
                        }
                    }
                } else {
                    None
                }
            }
            Err(_) => None,
        }
    }};
}

#[derive(Default)]
struct WorkerState {
    radios: Option<Radios>,
    media: Option<Media>,
    media_wanted: bool,
    wlan: Option<Wlan>,
    ip_notify: Option<IpNotify>,
    networks_wanted: bool,
    wmi: Option<IWbemServices>,
    pending_processes: Option<(Instant, ProcFrame)>,
    /// When the last media snapshot was published.
    media_published: Option<Instant>,
    /// Deferred publish for coalesced timeline-only media events.
    media_timeline_due: Option<Instant>,
    /// Last observed (default IPv4, Wi-Fi connected), to detect connectivity changes.
    last_net: Option<(Option<Ipv4Addr>, bool)>,
}

fn worker(shared: Arc<Shared>, commands: Receiver<Msg>) {
    let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
    let mut state = WorkerState::default();
    loop {
        let due = [
            state.pending_processes.as_ref().map(|(due, _)| *due),
            state.media_timeline_due,
        ]
        .into_iter()
        .flatten()
        .min();
        let message = match due {
            Some(due) => {
                match commands.recv_timeout(due.saturating_duration_since(Instant::now())) {
                    Ok(message) => Some(message),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match commands.recv() {
                Ok(message) => Some(message),
                Err(_) => break,
            },
        };
        if shared.quit.load(Ordering::Acquire) {
            break;
        }
        if let Some(Msg::Command(command)) = message {
            state.handle(&shared, command);
        }
        let bits = shared.dirty.swap(0, Ordering::AcqRel);
        if bits != 0 {
            state.dirty(&shared, bits);
        }
        if matches!(&state.pending_processes, Some((due, _)) if *due <= Instant::now()) {
            if let Some((_, first)) = state.pending_processes.take() {
                shared.publish(Update::Processes(finish_processes(first)));
            }
        }
        if state
            .media_timeline_due
            .is_some_and(|due| due <= Instant::now())
        {
            state.media_timeline_due = None;
            state.publish_media(&shared);
        }
    }
    // Unregister every callback before COM goes away.
    drop(state);
    if com {
        unsafe { CoUninitialize() };
    }
}

impl WorkerState {
    fn handle(&mut self, shared: &Arc<Shared>, command: Command) {
        match command {
            Command::RefreshRadios => {
                self.ensure_radios(shared);
                shared.publish(Update::Radios(self.radio_snapshot()));
            }
            Command::SetRadio { kind, on } => {
                self.ensure_radios(shared);
                let result = match &self.radios {
                    Some(radios) => radios.set(kind, on),
                    None => Err("Radio API unavailable".into()),
                };
                shared.action("radio", result);
                shared.publish(Update::Radios(self.radio_snapshot()));
            }
            Command::RefreshBrightness => {
                shared.publish(Update::Brightness(self.brightness_get()));
            }
            Command::SetBrightness(percent) => {
                let result = self.brightness_set(percent.min(100));
                shared.action("brightness", result);
                shared.publish(Update::Brightness(self.brightness_get()));
            }
            Command::RefreshWifi => {
                let status = self.wifi_status(shared);
                shared.publish(Update::Wifi(status));
            }
            Command::ScanWifi => {
                self.networks_wanted = true;
                self.ensure_wlan(shared);
                match &self.wlan {
                    Some(wlan) => {
                        shared.publish(Update::WifiNetworks(wlan.networks()));
                        if let Err(error) = wlan.scan() {
                            shared.action("wifi_scan", Err(error));
                        }
                    }
                    None => {
                        shared.publish(Update::WifiNetworks(Vec::new()));
                        shared.action("wifi_scan", Err("WLAN service unavailable".into()));
                    }
                }
            }
            Command::ConnectWifi { profile } => {
                self.ensure_wlan(shared);
                let result = self.with_wlan(|w| w.connect(&profile));
                shared.action("wifi_connect", result);
            }
            Command::DisconnectWifi => {
                self.ensure_wlan(shared);
                let result = self.with_wlan(Wlan::disconnect);
                shared.action("wifi_disconnect", result);
            }
            Command::ForgetWifi { profile } => {
                self.ensure_wlan(shared);
                let result = self.with_wlan(|w| w.forget(&profile));
                shared.action("wifi_forget", result);
                if let Some(wlan) = &self.wlan {
                    shared.publish(Update::WifiNetworks(wlan.networks()));
                }
            }
            Command::MediaSubscribe(on) => {
                self.media_wanted = on;
                if on {
                    if !self.media.as_ref().is_some_and(|m| m.events) {
                        self.media = Media::open(shared, true);
                    }
                    let snapshot = self.media.as_ref().map(Media::snapshot).unwrap_or_default();
                    shared.publish(Update::Media(snapshot));
                    self.media_published = Some(Instant::now());
                } else {
                    self.media = None;
                    self.media_timeline_due = None;
                }
            }
            Command::Media { action, app_id } => {
                if self.media.is_none() {
                    self.media = Media::open(shared, false);
                }
                let result = match &self.media {
                    Some(media) => media.command(action, app_id.as_deref()),
                    None => Err("media session manager unavailable".into()),
                };
                shared.action("media", result);
                if !self.media_wanted {
                    self.media = None;
                }
            }
            Command::Power(action) => shared.action("power", power(action)),
            Command::SampleProcesses => {
                if self.pending_processes.is_none() {
                    self.pending_processes =
                        Some((Instant::now() + PROCESS_WINDOW, ProcFrame::take()));
                }
            }
            Command::RefreshDisks => shared.publish(Update::Disks(disks())),
            // Routed to the weather thread by `Sysctl::send`.
            Command::WeatherSubscribe(_)
            | Command::WeatherRefresh
            | Command::WeatherLocation(_) => {}
        }
    }

    fn dirty(&mut self, shared: &Arc<Shared>, bits: u32) {
        if bits & DIRTY_RADIO != 0 && self.radios.is_some() {
            shared.publish(Update::Radios(self.radio_snapshot()));
        }
        if self.media_wanted && self.media.is_some() {
            if bits & DIRTY_MEDIA_SESSIONS != 0 {
                if let Some(media) = &mut self.media {
                    media.refresh_sessions(shared);
                }
            }
            if bits & (DIRTY_MEDIA | DIRTY_MEDIA_SESSIONS) != 0 {
                // A full refresh also covers any pending timeline update.
                self.media_timeline_due = None;
                self.publish_media(shared);
            } else if bits & DIRTY_MEDIA_TIMELINE != 0 && self.media_timeline_due.is_none() {
                let now = Instant::now();
                match self.media_published.map(|at| at + MEDIA_TIMELINE_GAP) {
                    Some(next) if next > now => self.media_timeline_due = Some(next),
                    _ => self.publish_media(shared),
                }
            }
        }
        if bits & DIRTY_WIFI != 0 {
            let status = self.wifi_status(shared);
            shared.publish(Update::Wifi(status));
        }
        if bits & (DIRTY_SCAN | DIRTY_WIFI) != 0 && self.networks_wanted {
            if let Some(wlan) = &self.wlan {
                shared.publish(Update::WifiNetworks(wlan.networks()));
            }
        }
    }

    fn publish_media(&mut self, shared: &Arc<Shared>) {
        if !self.media_wanted {
            return;
        }
        if let Some(media) = &self.media {
            shared.publish(Update::Media(media.snapshot()));
            self.media_published = Some(Instant::now());
        }
    }

    /// The radio list is re-read when a radio is missing (Bluetooth adapters appear late after
    /// resume, USB dongles come and go).
    fn ensure_radios(&mut self, shared: &Arc<Shared>) {
        if self.radios.as_ref().is_some_and(|r| {
            r.find(RadioType::WiFi).is_none() || r.find(RadioType::Bluetooth).is_none()
        }) {
            self.radios = None;
        }
        if self.radios.is_none() {
            self.radios = Radios::open(shared);
        }
    }

    fn radio_snapshot(&self) -> RadioSnapshot {
        self.radios
            .as_ref()
            .map(Radios::snapshot)
            .unwrap_or_default()
    }

    fn ensure_wlan(&mut self, shared: &Arc<Shared>) {
        if self.wlan.is_none() {
            self.wlan = Wlan::open(shared);
        }
        if self.ip_notify.is_none() {
            self.ip_notify = IpNotify::open(shared);
        }
    }

    fn with_wlan(&self, f: impl FnOnce(&Wlan) -> Result<(), String>) -> Result<(), String> {
        match &self.wlan {
            Some(wlan) => f(wlan),
            None => Err("WLAN service unavailable".into()),
        }
    }

    fn wifi_status(&mut self, shared: &Arc<Shared>) -> WifiStatus {
        self.ensure_wlan(shared);
        let mut status = WifiStatus {
            default_ipv4: default_ipv4(),
            ..Default::default()
        };
        if let Some(wlan) = &self.wlan {
            if let Some(interface) = wlan.interface() {
                status.available = true;
                if let Some((ssid, profile, signal)) = wlan.current(&interface) {
                    status.connected = true;
                    status.ssid = Some(ssid);
                    status.profile = Some(profile).filter(|p| !p.is_empty());
                    status.signal = Some(signal);
                }
            }
        }
        let net = (status.default_ipv4, status.connected);
        if self.last_net.replace(net) != Some(net) && net.0.is_some() {
            // Connectivity appeared/changed (e.g. Wi-Fi joined after sign-in): let the weather
            // thread retry now instead of waiting out its 5 min failure back-off.
            let _ = shared.weather.try_send(WeatherMsg::NetworkChanged);
        }
        status
    }

    fn brightness_get(&mut self) -> BrightnessSnapshot {
        if self.wmi.is_none() {
            self.wmi = wmi_connect();
        }
        if let Some(percent) = self.wmi.as_ref().and_then(wmi_brightness) {
            return BrightnessSnapshot {
                percent: Some(percent),
                source: Some(BrightnessSource::Wmi),
            };
        }
        match ddc_brightness() {
            Some(percent) => BrightnessSnapshot {
                percent: Some(percent),
                source: Some(BrightnessSource::Ddc),
            },
            None => BrightnessSnapshot::default(),
        }
    }

    fn brightness_set(&mut self, percent: u8) -> Result<(), String> {
        if self.wmi.is_none() {
            self.wmi = wmi_connect();
        }
        if let Some(services) = &self.wmi {
            if wmi_brightness(services).is_some() {
                return wmi_set_brightness(services, percent);
            }
        }
        ddc_set_brightness(percent)
    }
}

fn win32_error(context: &str, code: u32) -> String {
    format!("{context}: {}", HRESULT::from_win32(code).message())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

fn event_handler<S: RuntimeType + 'static, A: RuntimeType + 'static>(
    shared: &Arc<Shared>,
    bit: u32,
) -> TypedEventHandler<S, A> {
    let shared = Arc::clone(shared);
    TypedEventHandler::new(move |_, _| {
        shared.mark(bit);
        Ok(())
    })
}

// ---------------------------------------------------------------------------
// Radios (Windows.Devices.Radios)
// ---------------------------------------------------------------------------

struct Radios {
    list: Vec<(Radio, Option<i64>)>,
    access_denied: bool,
}

impl Radios {
    fn open(shared: &Arc<Shared>) -> Option<Self> {
        let access = wait!(Radio::RequestAccessAsync(), WINRT_TIMEOUT);
        let radios = wait!(Radio::GetRadiosAsync(), WINRT_TIMEOUT)?;
        let count = radios.Size().unwrap_or(0).min(MAX_RADIOS);
        let mut list = Vec::with_capacity(count as usize);
        for index in 0..count {
            let Ok(radio) = radios.GetAt(index) else {
                continue;
            };
            let token = radio
                .StateChanged(&event_handler::<Radio, IInspectable>(shared, DIRTY_RADIO))
                .ok();
            list.push((radio, token));
        }
        Some(Self {
            list,
            access_denied: matches!(access, Some(a) if a != RadioAccessStatus::Allowed),
        })
    }

    fn find(&self, kind: RadioType) -> Option<&Radio> {
        let want = match kind {
            RadioType::WiFi => RadioKind::WiFi,
            RadioType::Bluetooth => RadioKind::Bluetooth,
        };
        self.list
            .iter()
            .map(|(radio, _)| radio)
            .find(|radio| radio.Kind().ok() == Some(want))
    }

    fn power(&self, kind: RadioType) -> Option<RadioPower> {
        self.find(kind).map(|radio| match radio.State() {
            Ok(RadioState::On) => RadioPower::On,
            Ok(RadioState::Off) => RadioPower::Off,
            Ok(RadioState::Disabled) => RadioPower::Disabled,
            _ => RadioPower::Unknown,
        })
    }

    fn snapshot(&self) -> RadioSnapshot {
        RadioSnapshot {
            available: true,
            access_denied: self.access_denied,
            wifi: self.power(RadioType::WiFi),
            bluetooth: self.power(RadioType::Bluetooth),
        }
    }

    fn set(&self, kind: RadioType, on: bool) -> Result<(), String> {
        let radio = self.find(kind).ok_or("radio not present")?;
        let state = if on { RadioState::On } else { RadioState::Off };
        match wait!(radio.SetStateAsync(state), WINRT_TIMEOUT) {
            Some(RadioAccessStatus::Allowed) => Ok(()),
            Some(RadioAccessStatus::DeniedByUser) => Err("radio access denied by user".into()),
            Some(RadioAccessStatus::DeniedBySystem) => Err("radio access denied by system".into()),
            Some(_) => Err("radio access unspecified".into()),
            None => Err("radio did not respond".into()),
        }
    }
}

impl Drop for Radios {
    fn drop(&mut self) {
        for (radio, token) in &self.list {
            if let Some(token) = token {
                let _ = radio.RemoveStateChanged(*token);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Media (GlobalSystemMediaTransportControlsSessionManager)
// ---------------------------------------------------------------------------

struct Media {
    manager: SessionManager,
    events: bool,
    manager_tokens: [Option<i64>; 2],
    sessions: Vec<(Session, [Option<i64>; 3])>,
}

impl Media {
    fn open(shared: &Arc<Shared>, events: bool) -> Option<Self> {
        let manager = wait!(SessionManager::RequestAsync(), WINRT_TIMEOUT)?;
        let mut media = Self {
            manager,
            events,
            manager_tokens: [None, None],
            sessions: Vec::new(),
        };
        if events {
            media.manager_tokens = [
                media
                    .manager
                    .SessionsChanged(&event_handler::<SessionManager, SessionsChangedEventArgs>(
                        shared,
                        DIRTY_MEDIA_SESSIONS,
                    ))
                    .ok(),
                media
                    .manager
                    .CurrentSessionChanged(&event_handler::<
                        SessionManager,
                        windows::Media::Control::CurrentSessionChangedEventArgs,
                    >(shared, DIRTY_MEDIA))
                    .ok(),
            ];
        }
        media.refresh_sessions(shared);
        Some(media)
    }

    fn unsubscribe_sessions(&mut self) {
        for (session, [properties, playback, timeline]) in self.sessions.drain(..) {
            if let Some(token) = properties {
                let _ = session.RemoveMediaPropertiesChanged(token);
            }
            if let Some(token) = playback {
                let _ = session.RemovePlaybackInfoChanged(token);
            }
            if let Some(token) = timeline {
                let _ = session.RemoveTimelinePropertiesChanged(token);
            }
        }
    }

    fn refresh_sessions(&mut self, shared: &Arc<Shared>) {
        self.unsubscribe_sessions();
        let Ok(list) = self.manager.GetSessions() else {
            return;
        };
        for index in 0..list.Size().unwrap_or(0).min(MAX_SESSIONS) {
            let Ok(session) = list.GetAt(index) else {
                continue;
            };
            let tokens = if self.events {
                [
                    session
                        .MediaPropertiesChanged(&event_handler::<
                            Session,
                            MediaPropertiesChangedEventArgs,
                        >(shared, DIRTY_MEDIA))
                        .ok(),
                    session
                        .PlaybackInfoChanged(
                            &event_handler::<Session, PlaybackInfoChangedEventArgs>(
                                shared,
                                DIRTY_MEDIA,
                            ),
                        )
                        .ok(),
                    session
                        .TimelinePropertiesChanged(&event_handler::<
                            Session,
                            TimelinePropertiesChangedEventArgs,
                        >(
                            shared, DIRTY_MEDIA_TIMELINE
                        ))
                        .ok(),
                ]
            } else {
                [None, None, None]
            };
            self.sessions.push((session, tokens));
        }
    }

    fn snapshot(&self) -> MediaSnapshot {
        let current = self
            .manager
            .GetCurrentSession()
            .ok()
            .and_then(|s| s.SourceAppUserModelId().ok())
            .map(|id| id.to_string());
        MediaSnapshot {
            available: true,
            sessions: self
                .sessions
                .iter()
                .map(|(session, _)| describe_session(session, current.as_deref()))
                .collect(),
        }
    }

    fn command(&self, action: MediaAction, app_id: Option<&str>) -> Result<(), String> {
        let target = match app_id {
            Some(id) => self
                .sessions
                .iter()
                .map(|(session, _)| session)
                .find(|s| s.SourceAppUserModelId().is_ok_and(|v| v == id))
                .cloned(),
            None => self.manager.GetCurrentSession().ok(),
        }
        .ok_or("no media session")?;
        let operation = match action {
            MediaAction::PlayPause => target.TryTogglePlayPauseAsync(),
            MediaAction::Next => target.TrySkipNextAsync(),
            MediaAction::Previous => target.TrySkipPreviousAsync(),
        };
        match wait!(operation, MEDIA_TIMEOUT) {
            Some(true) => Ok(()),
            Some(false) => Err("media session rejected the command".into()),
            None => Err("media session did not respond".into()),
        }
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        self.unsubscribe_sessions();
        if let Some(token) = self.manager_tokens[0] {
            let _ = self.manager.RemoveSessionsChanged(token);
        }
        if let Some(token) = self.manager_tokens[1] {
            let _ = self.manager.RemoveCurrentSessionChanged(token);
        }
    }
}

/// 100 ns ticks between 1601-01-01 and 1970-01-01.
const FILETIME_UNIX_OFFSET: i64 = 116_444_736_000_000_000;

fn describe_session(session: &Session, current: Option<&str>) -> MediaSession {
    let app_id = session
        .SourceAppUserModelId()
        .map(|id| id.to_string())
        .unwrap_or_default();
    let mut out = MediaSession {
        is_current: current == Some(app_id.as_str()),
        app_id,
        ..Default::default()
    };
    if let Some(properties) = wait!(session.TryGetMediaPropertiesAsync(), MEDIA_TIMEOUT) {
        out.title = properties
            .Title()
            .map(|v| v.to_string())
            .unwrap_or_default();
        out.artist = properties
            .Artist()
            .map(|v| v.to_string())
            .unwrap_or_default();
        out.album = properties
            .AlbumTitle()
            .map(|v| v.to_string())
            .unwrap_or_default();
    }
    if let Ok(info) = session.GetPlaybackInfo() {
        out.status = match info.PlaybackStatus() {
            Ok(RawStatus::Closed) => PlaybackStatus::Closed,
            Ok(RawStatus::Opened) => PlaybackStatus::Opened,
            Ok(RawStatus::Changing) => PlaybackStatus::Changing,
            Ok(RawStatus::Stopped) => PlaybackStatus::Stopped,
            Ok(RawStatus::Playing) => PlaybackStatus::Playing,
            Ok(RawStatus::Paused) => PlaybackStatus::Paused,
            _ => PlaybackStatus::Unknown,
        };
        if let Ok(controls) = info.Controls() {
            out.can_play_pause = controls.IsPlayPauseToggleEnabled().unwrap_or(false)
                || controls.IsPlayEnabled().unwrap_or(false)
                || controls.IsPauseEnabled().unwrap_or(false);
            out.can_next = controls.IsNextEnabled().unwrap_or(false);
            out.can_previous = controls.IsPreviousEnabled().unwrap_or(false);
        }
    }
    if let Ok(timeline) = session.GetTimelineProperties() {
        if let (Ok(start), Ok(end), Ok(position)) = (
            timeline.StartTime(),
            timeline.EndTime(),
            timeline.Position(),
        ) {
            if end.Duration > start.Duration {
                out.end_ms = Some(((end.Duration - start.Duration) / 10_000) as u64);
                out.position_ms =
                    Some(((position.Duration - start.Duration).max(0) / 10_000) as u64);
            }
        }
        if let Ok(updated) = timeline.LastUpdatedTime() {
            if updated.UniversalTime > FILETIME_UNIX_OFFSET {
                out.timeline_updated_unix_ms =
                    Some((updated.UniversalTime - FILETIME_UNIX_OFFSET) / 10_000);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Brightness: WMI (internal panel) and DDC/CI (external monitors)
// ---------------------------------------------------------------------------

fn wmi_connect() -> Option<IWbemServices> {
    unsafe {
        let locator: IWbemLocator =
            CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).ok()?;
        let services = locator
            .ConnectServer(
                &BSTR::from("ROOT\\WMI"),
                &BSTR::new(),
                &BSTR::new(),
                &BSTR::new(),
                0,
                &BSTR::new(),
                None::<&IWbemContext>,
            )
            .ok()?;
        CoSetProxyBlanket(
            &services,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
        .ok()?;
        Some(services)
    }
}

/// Owned VARIANT that is cleared on drop.
struct Variant(VARIANT);

impl Drop for Variant {
    fn drop(&mut self) {
        unsafe {
            let _ = VariantClear(&mut self.0);
        }
    }
}

impl Variant {
    fn i4(value: i32) -> Self {
        let mut v = VARIANT::default();
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = VT_I4;
            inner.Anonymous.lVal = value;
        }
        Variant(v)
    }

    fn ui1(value: u8) -> Self {
        let mut v = VARIANT::default();
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = VT_UI1;
            inner.Anonymous.bVal = value;
        }
        Variant(v)
    }

    fn as_u32(&self) -> Option<u32> {
        unsafe {
            let inner = &*self.0.Anonymous.Anonymous;
            let value = &inner.Anonymous;
            match inner.vt {
                VT_UI1 => Some(value.bVal as u32),
                VT_I2 => u32::try_from(value.iVal).ok(),
                VT_UI2 => Some(value.uiVal as u32),
                VT_I4 => u32::try_from(value.lVal).ok(),
                VT_UI4 => Some(value.ulVal),
                _ => None,
            }
        }
    }

    fn as_string(&self) -> Option<String> {
        unsafe {
            let inner = &*self.0.Anonymous.Anonymous;
            (inner.vt == VT_BSTR).then(|| inner.Anonymous.bstrVal.to_string())
        }
    }
}

fn wmi_first(services: &IWbemServices, query: &str) -> Option<IWbemClassObject> {
    unsafe {
        let objects = services
            .ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from(query),
                WBEM_GENERIC_FLAG_TYPE(WBEM_FLAG_FORWARD_ONLY.0 | WBEM_FLAG_RETURN_IMMEDIATELY.0),
                None::<&IWbemContext>,
            )
            .ok()?;
        let mut row = [None];
        let mut returned = 0u32;
        if objects
            .Next(WMI_TIMEOUT_MS, &mut row, &mut returned)
            .is_err()
            || returned == 0
        {
            return None;
        }
        row[0].take()
    }
}

fn wmi_get(object: &IWbemClassObject, name: PCWSTR) -> Option<Variant> {
    let mut value = Variant(VARIANT::default());
    unsafe { object.Get(name, 0, &mut value.0, None, None).ok()? };
    Some(value)
}

fn wmi_brightness(services: &IWbemServices) -> Option<u8> {
    let object = wmi_first(
        services,
        "SELECT CurrentBrightness FROM WmiMonitorBrightness WHERE Active=TRUE",
    )?;
    let value = wmi_get(&object, w!("CurrentBrightness"))?.as_u32()?;
    Some(value.min(100) as u8)
}

fn wmi_set_brightness(services: &IWbemServices, percent: u8) -> Result<(), String> {
    let error = |e: windows::core::Error| format!("WMI: {}", e.message());
    let instance = wmi_first(
        services,
        "SELECT * FROM WmiMonitorBrightnessMethods WHERE Active=TRUE",
    )
    .ok_or("WmiMonitorBrightnessMethods unavailable")?;
    let path = wmi_get(&instance, w!("__PATH"))
        .and_then(|v| v.as_string())
        .ok_or("WMI instance path unavailable")?;
    unsafe {
        let mut class = None;
        services
            .GetObject(
                &BSTR::from("WmiMonitorBrightnessMethods"),
                WBEM_GENERIC_FLAG_TYPE(0),
                None::<&IWbemContext>,
                Some(&mut class),
                None,
            )
            .map_err(error)?;
        let class: IWbemClassObject = class.ok_or("WMI class unavailable")?;
        let mut signature = None;
        class
            .GetMethod(
                w!("WmiSetBrightness"),
                0,
                &mut signature,
                std::ptr::null_mut(),
            )
            .map_err(error)?;
        let signature: IWbemClassObject = signature.ok_or("WMI method unavailable")?;
        let input = signature.SpawnInstance(0).map_err(error)?;
        input
            .Put(w!("Timeout"), 0, &Variant::i4(0).0, 0)
            .map_err(error)?;
        input
            .Put(w!("Brightness"), 0, &Variant::ui1(percent).0, 0)
            .map_err(error)?;
        services
            .ExecMethod(
                &BSTR::from(path),
                &BSTR::from("WmiSetBrightness"),
                WBEM_GENERIC_FLAG_TYPE(0),
                None::<&IWbemContext>,
                &input,
                None,
                None,
            )
            .map_err(error)
    }
}

struct PhysicalMonitors(Vec<PHYSICAL_MONITOR>);

impl Drop for PhysicalMonitors {
    fn drop(&mut self) {
        if !self.0.is_empty() {
            unsafe {
                let _ = DestroyPhysicalMonitors(&self.0);
            }
        }
    }
}

unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    if list.len() < MAX_MONITORS {
        list.push(monitor);
    }
    true.into()
}

fn physical_monitors() -> PhysicalMonitors {
    let mut handles: Vec<HMONITOR> = Vec::new();
    let mut monitors = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut handles as *mut Vec<HMONITOR> as isize),
        );
        for handle in handles {
            let mut count = 0u32;
            if GetNumberOfPhysicalMonitorsFromHMONITOR(handle, &mut count).is_err() || count == 0 {
                continue;
            }
            let empty = PHYSICAL_MONITOR {
                hPhysicalMonitor: HANDLE::default(),
                szPhysicalMonitorDescription: [0; 128],
            };
            let mut array = vec![empty; count.min(MAX_MONITORS as u32) as usize];
            if GetPhysicalMonitorsFromHMONITOR(handle, &mut array).is_ok() {
                monitors.extend(array);
            }
        }
    }
    PhysicalMonitors(monitors)
}

fn ddc_range(monitor: &PHYSICAL_MONITOR) -> Option<(u32, u32, u32)> {
    let handle = monitor.hPhysicalMonitor;
    let (mut min, mut current, mut max) = (0u32, 0u32, 0u32);
    let ok = unsafe { GetMonitorBrightness(handle, &mut min, &mut current, &mut max) } != 0;
    (ok && max > min).then_some((min, current.clamp(min, max), max))
}

fn ddc_brightness() -> Option<u8> {
    let monitors = physical_monitors();
    monitors.0.iter().find_map(|monitor| {
        let (min, current, max) = ddc_range(monitor)?;
        let span = max - min;
        Some((((current - min) * 100 + span / 2) / span) as u8)
    })
}

fn ddc_set_brightness(percent: u8) -> Result<(), String> {
    let monitors = physical_monitors();
    let mut changed = 0;
    for monitor in &monitors.0 {
        if let Some((min, _, max)) = ddc_range(monitor) {
            let value = min + (max - min) * percent as u32 / 100;
            let handle = monitor.hPhysicalMonitor;
            if unsafe { SetMonitorBrightness(handle, value) } != 0 {
                changed += 1;
            }
        }
    }
    if changed > 0 {
        Ok(())
    } else {
        Err("no monitor accepts brightness control".into())
    }
}

// ---------------------------------------------------------------------------
// Wi-Fi (native WLAN API) and default-route IPv4
// ---------------------------------------------------------------------------

struct Wlan {
    handle: HANDLE,
    _context: Arc<Shared>,
}

unsafe extern "system" fn wlan_notify(data: *mut L2_NOTIFICATION_DATA, context: *mut c_void) {
    if data.is_null() || context.is_null() {
        return;
    }
    let shared = unsafe { &*(context as *const Shared) };
    let data = unsafe { &*data };
    if data.NotificationSource != WLAN_NOTIFICATION_SOURCE_ACM {
        return;
    }
    let code = data.NotificationCode as i32;
    if code == wlan_notification_acm_scan_complete.0 || code == wlan_notification_acm_scan_fail.0 {
        shared.mark(DIRTY_SCAN);
    } else if code == wlan_notification_acm_connection_complete.0
        || code == wlan_notification_acm_disconnected.0
        || code == wlan_notification_acm_interface_arrival.0
        || code == wlan_notification_acm_interface_removal.0
    {
        shared.mark(DIRTY_WIFI);
    }
}

fn ssid_text(ssid: &DOT11_SSID) -> String {
    let length = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
    String::from_utf8_lossy(&ssid.ucSSID[..length]).into_owned()
}

impl Wlan {
    fn open(shared: &Arc<Shared>) -> Option<Self> {
        let mut version = 0u32;
        let mut handle = HANDLE::default();
        if unsafe { WlanOpenHandle(2, None, &mut version, &mut handle) } != 0 {
            return None;
        }
        let context = Arc::clone(shared);
        unsafe {
            WlanRegisterNotification(
                handle,
                WLAN_NOTIFICATION_SOURCE_ACM,
                true,
                Some(wlan_notify),
                Some(Arc::as_ptr(&context) as *const c_void),
                None,
                None,
            );
        }
        Some(Self {
            handle,
            _context: context,
        })
    }

    /// Prefers the connected interface; otherwise the first one.
    fn interface(&self) -> Option<GUID> {
        unsafe {
            let mut list = std::ptr::null_mut();
            if WlanEnumInterfaces(self.handle, None, &mut list) != 0 || list.is_null() {
                return None;
            }
            let items = std::slice::from_raw_parts(
                std::ptr::addr_of!((*list).InterfaceInfo) as *const WLAN_INTERFACE_INFO,
                (*list).dwNumberOfItems as usize,
            );
            let pick = items
                .iter()
                .find(|i| i.isState == wlan_interface_state_connected)
                .or(items.first())
                .map(|i| i.InterfaceGuid);
            WlanFreeMemory(list as *const c_void);
            pick
        }
    }

    /// (SSID, profile name, signal %) of the current association.
    fn current(&self, interface: &GUID) -> Option<(String, String, u8)> {
        unsafe {
            let mut size = 0u32;
            let mut data = std::ptr::null_mut();
            if WlanQueryInterface(
                self.handle,
                interface,
                wlan_intf_opcode_current_connection,
                None,
                &mut size,
                &mut data,
                None,
            ) != 0
                || data.is_null()
            {
                return None;
            }
            let result = if size as usize >= std::mem::size_of::<WLAN_CONNECTION_ATTRIBUTES>() {
                let attributes = &*(data as *const WLAN_CONNECTION_ATTRIBUTES);
                (attributes.isState == wlan_interface_state_connected).then(|| {
                    let association = &attributes.wlanAssociationAttributes;
                    (
                        ssid_text(&association.dot11Ssid),
                        from_wide(&attributes.strProfileName),
                        association.wlanSignalQuality.min(100) as u8,
                    )
                })
            } else {
                None
            };
            WlanFreeMemory(data as *const c_void);
            result
        }
    }

    fn networks(&self) -> Vec<WifiNetwork> {
        let Some(interface) = self.interface() else {
            return Vec::new();
        };
        let mut raw = Vec::new();
        unsafe {
            let mut list = std::ptr::null_mut();
            if WlanGetAvailableNetworkList(self.handle, &interface, 0, None, &mut list) != 0
                || list.is_null()
            {
                return Vec::new();
            }
            let items = std::slice::from_raw_parts(
                std::ptr::addr_of!((*list).Network) as *const WLAN_AVAILABLE_NETWORK,
                (*list).dwNumberOfItems as usize,
            );
            for item in items.iter().take(MAX_NETWORKS * 4) {
                let saved = item.dwFlags & WLAN_AVAILABLE_NETWORK_HAS_PROFILE != 0;
                raw.push(WifiNetwork {
                    ssid: ssid_text(&item.dot11Ssid),
                    signal: item.wlanSignalQuality.min(100) as u8,
                    secured: item.bSecurityEnabled.as_bool(),
                    saved,
                    connected: item.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                    profile: saved
                        .then(|| from_wide(&item.strProfileName))
                        .filter(|p| !p.is_empty()),
                });
            }
            WlanFreeMemory(list as *const c_void);
        }
        merge_networks(raw)
    }

    fn scan(&self) -> Result<(), String> {
        let interface = self.interface().ok_or("no WLAN interface")?;
        match unsafe { WlanScan(self.handle, &interface, None, None, None) } {
            0 => Ok(()),
            code => Err(win32_error("WlanScan", code)),
        }
    }

    fn connect(&self, profile: &str) -> Result<(), String> {
        let interface = self.interface().ok_or("no WLAN interface")?;
        let name = wide(profile);
        let parameters = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(name.as_ptr()),
            pDot11Ssid: std::ptr::null_mut(),
            pDesiredBssidList: std::ptr::null_mut(),
            dot11BssType: dot11_BSS_type_any,
            dwFlags: 0,
        };
        match unsafe { WlanConnect(self.handle, &interface, &parameters, None) } {
            0 => Ok(()),
            code => Err(win32_error("WlanConnect", code)),
        }
    }

    fn disconnect(&self) -> Result<(), String> {
        let interface = self.interface().ok_or("no WLAN interface")?;
        match unsafe { WlanDisconnect(self.handle, &interface, None) } {
            0 => Ok(()),
            code => Err(win32_error("WlanDisconnect", code)),
        }
    }

    fn forget(&self, profile: &str) -> Result<(), String> {
        let interface = self.interface().ok_or("no WLAN interface")?;
        let name = wide(profile);
        match unsafe { WlanDeleteProfile(self.handle, &interface, PCWSTR(name.as_ptr()), None) } {
            0 => Ok(()),
            code => Err(win32_error("WlanDeleteProfile", code)),
        }
    }
}

impl Drop for Wlan {
    fn drop(&mut self) {
        // WlanCloseHandle waits for in-flight callbacks, so `_context` outlives them.
        unsafe {
            WlanRegisterNotification(
                self.handle,
                WLAN_NOTIFICATION_SOURCE_NONE,
                true,
                None,
                None,
                None,
                None,
            );
            WlanCloseHandle(self.handle, None);
        }
    }
}

struct IpNotify {
    handle: HANDLE,
    _context: Arc<Shared>,
}

unsafe extern "system" fn ip_changed(
    context: *const c_void,
    _: *const MIB_UNICASTIPADDRESS_ROW,
    _: MIB_NOTIFICATION_TYPE,
) {
    if !context.is_null() {
        unsafe { &*(context as *const Shared) }.mark(DIRTY_WIFI);
    }
}

impl IpNotify {
    fn open(shared: &Arc<Shared>) -> Option<Self> {
        let context = Arc::clone(shared);
        let mut handle = HANDLE::default();
        let result = unsafe {
            NotifyUnicastIpAddressChange(
                AF_INET,
                Some(ip_changed),
                Some(Arc::as_ptr(&context) as *const c_void),
                false,
                &mut handle,
            )
        };
        (result == NO_ERROR).then_some(Self {
            handle,
            _context: context,
        })
    }
}

impl Drop for IpNotify {
    fn drop(&mut self) {
        // Blocks until running callbacks return.
        unsafe {
            let _ = CancelMibChangeNotify2(self.handle);
        }
    }
}

fn default_ipv4() -> Option<Ipv4Addr> {
    unsafe {
        let mut index = 0u32;
        if GetBestInterface(u32::from_ne_bytes([1, 1, 1, 1]), &mut index) != 0 {
            return None;
        }
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut size = 16 * 1024u32;
        let mut buffer: Vec<u64> = Vec::new();
        for _ in 0..3 {
            buffer = vec![0u64; (size as usize).div_ceil(8)];
            match GetAdaptersAddresses(
                AF_INET.0 as u32,
                flags,
                None,
                Some(buffer.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                &mut size,
            ) {
                0 => break,
                111 if size <= 1024 * 1024 => continue, // ERROR_BUFFER_OVERFLOW
                _ => return None,
            }
        }
        let mut adapter = buffer.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !adapter.is_null() {
            if (*adapter).Anonymous1.Anonymous.IfIndex == index {
                let mut unicast = (*adapter).FirstUnicastAddress;
                while !unicast.is_null() {
                    let address = (*unicast).Address.lpSockaddr as *const SOCKADDR_IN;
                    if !address.is_null() && (*address).sin_family == AF_INET {
                        return Some(Ipv4Addr::from(
                            (*address).sin_addr.S_un.S_addr.to_ne_bytes(),
                        ));
                    }
                    unicast = (*unicast).Next;
                }
                return None;
            }
            adapter = (*adapter).Next;
        }
        None
    }
}

/// Dedupes by SSID (max signal, OR of flags, saved profile kept) and sorts
/// connected → saved → signal → name. Hidden (empty) SSIDs are dropped.
pub fn merge_networks(raw: impl IntoIterator<Item = WifiNetwork>) -> Vec<WifiNetwork> {
    let mut out: Vec<WifiNetwork> = Vec::new();
    for network in raw {
        if network.ssid.is_empty() {
            continue;
        }
        match out.iter_mut().find(|e| e.ssid == network.ssid) {
            Some(existing) => {
                existing.signal = existing.signal.max(network.signal);
                existing.secured |= network.secured;
                if network.profile.is_some() && (existing.profile.is_none() || network.connected) {
                    existing.profile = network.profile;
                }
                existing.saved |= network.saved;
                existing.connected |= network.connected;
            }
            None => out.push(network),
        }
    }
    out.sort_by(|a, b| {
        b.connected
            .cmp(&a.connected)
            .then(b.saved.cmp(&a.saved))
            .then(b.signal.cmp(&a.signal))
            .then_with(|| a.ssid.cmp(&b.ssid))
    });
    out.truncate(MAX_NETWORKS);
    out
}

// ---------------------------------------------------------------------------
// Power
// ---------------------------------------------------------------------------

fn enable_shutdown_privilege() -> Result<(), String> {
    let error = |e: windows::core::Error| format!("privilege: {}", e.message());
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .map_err(error)?;
        let mut luid = LUID::default();
        let result =
            LookupPrivilegeValueW(PCWSTR::null(), SE_SHUTDOWN_NAME, &mut luid).and_then(|_| {
                let privileges = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES {
                        Luid: luid,
                        Attributes: SE_PRIVILEGE_ENABLED,
                    }],
                };
                AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None)
            });
        let not_all = GetLastError() == ERROR_NOT_ALL_ASSIGNED;
        let _ = CloseHandle(token);
        result.map_err(error)?;
        if not_all {
            return Err("shutdown privilege not held by this account".into());
        }
        Ok(())
    }
}

fn power(action: PowerAction) -> Result<(), String> {
    let error = |e: windows::core::Error| e.message();
    let reason = SHUTDOWN_REASON(SHTDN_REASON_MAJOR_OTHER.0 | SHTDN_REASON_FLAG_PLANNED.0);
    unsafe {
        match action {
            PowerAction::Lock => LockWorkStation().map_err(error),
            PowerAction::Sleep | PowerAction::Hibernate => {
                enable_shutdown_privilege()?;
                // Returns after resume. Modern-standby systems may refuse S3 sleep.
                if SetSuspendState(action == PowerAction::Hibernate, false, false) {
                    Ok(())
                } else {
                    Err(error(windows::core::Error::from_thread()))
                }
            }
            PowerAction::SignOut => ExitWindowsEx(EWX_LOGOFF, reason).map_err(error),
            PowerAction::Restart => {
                enable_shutdown_privilege()?;
                ExitWindowsEx(EWX_REBOOT, reason).map_err(error)
            }
            PowerAction::Shutdown => {
                enable_shutdown_privilege()?;
                ExitWindowsEx(EWX_POWEROFF, reason).map_err(error)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Processes and disks
// ---------------------------------------------------------------------------

fn filetime(value: FILETIME) -> u64 {
    ((value.dwHighDateTime as u64) << 32) | value.dwLowDateTime as u64
}

struct ProcRow {
    pid: u32,
    creation: u64,
    cpu: u64,
    name: String,
    working_set: u64,
}

struct ProcFrame {
    system: Option<u64>,
    /// pid → (creation time, kernel+user time)
    processes: HashMap<u32, (u64, u64)>,
}

impl ProcFrame {
    fn take() -> Self {
        let system = system_cpu_time();
        let processes = scan_processes(false)
            .into_iter()
            .map(|row| (row.pid, (row.creation, row.cpu)))
            .collect();
        Self { system, processes }
    }
}

/// Kernel (includes idle) + user time summed over all logical processors.
fn system_cpu_time() -> Option<u64> {
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe { GetSystemTimes(None, Some(&mut kernel), Some(&mut user)).ok()? };
    Some(filetime(kernel) + filetime(user))
}

fn scan_processes(memory: bool) -> Vec<ProcRow> {
    let mut rows = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return rows;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more && rows.len() < MAX_PROCESSES {
            let pid = entry.th32ProcessID;
            if pid != 0 {
                if let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                    let mut times = [FILETIME::default(); 4];
                    let [creation, exit, kernel, user] = &mut times;
                    if GetProcessTimes(process, creation, exit, kernel, user).is_ok() {
                        let mut working_set = 0;
                        if memory {
                            let mut counters = PROCESS_MEMORY_COUNTERS {
                                cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
                                ..Default::default()
                            };
                            if K32GetProcessMemoryInfo(process, &mut counters, counters.cb)
                                .as_bool()
                            {
                                working_set = counters.WorkingSetSize as u64;
                            }
                        }
                        rows.push(ProcRow {
                            pid,
                            creation: filetime(*creation),
                            cpu: filetime(*kernel) + filetime(*user),
                            name: from_wide(&entry.szExeFile),
                            working_set,
                        });
                    }
                    let _ = CloseHandle(process);
                }
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    rows
}

fn finish_processes(first: ProcFrame) -> Vec<ProcessUsage> {
    let rows = scan_processes(true);
    let system = system_cpu_time();
    let total = match (first.system, system) {
        (Some(before), Some(after)) if after > before => (after - before) as f64,
        _ => 0.0,
    };
    group_processes(
        rows.into_iter().map(|row| {
            let busy = match first.processes.get(&row.pid) {
                Some(&(creation, cpu)) if creation == row.creation => row.cpu.saturating_sub(cpu),
                _ => 0, // started inside the window or PID reused: no baseline
            };
            let percent = if total > 0.0 {
                busy as f64 * 100.0 / total
            } else {
                0.0
            };
            (row.name, percent, row.working_set)
        }),
        TOP_PROCESSES,
    )
}

/// Groups `(name, cpu %, working set)` samples by case-insensitive name and
/// returns the `top` groups by CPU, then memory, then name.
pub fn group_processes(
    samples: impl IntoIterator<Item = (String, f64, u64)>,
    top: usize,
) -> Vec<ProcessUsage> {
    let mut groups: HashMap<String, (String, f64, u64, u32)> = HashMap::new();
    for (name, cpu, working_set) in samples {
        let entry = groups
            .entry(name.to_lowercase())
            .or_insert_with(|| (name, 0.0, 0, 0));
        entry.1 += cpu.max(0.0);
        entry.2 = entry.2.saturating_add(working_set);
        entry.3 += 1;
    }
    let mut out: Vec<ProcessUsage> = groups
        .into_values()
        .map(|(name, cpu, working_set_bytes, instances)| ProcessUsage {
            name,
            cpu_percent: cpu.min(100.0) as f32,
            working_set_bytes,
            instances,
        })
        .collect();
    out.sort_by(|a, b| {
        b.cpu_percent
            .total_cmp(&a.cpu_percent)
            .then(b.working_set_bytes.cmp(&a.working_set_bytes))
            .then_with(|| a.name.cmp(&b.name))
    });
    out.truncate(top);
    out
}

const DRIVE_FIXED: u32 = 3;

fn disks() -> Vec<DiskInfo> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .filter_map(|bit| {
            let root = format!("{}:\\", (b'A' + bit) as char);
            let path = wide(&root);
            let path = PCWSTR(path.as_ptr());
            unsafe {
                if GetDriveTypeW(path) != DRIVE_FIXED {
                    return None;
                }
                let (mut available, mut total) = (0u64, 0u64);
                GetDiskFreeSpaceExW(path, Some(&mut available), Some(&mut total), None).ok()?;
                let mut label = [0u16; 261];
                let mut filesystem = [0u16; 261];
                let _ = GetVolumeInformationW(
                    path,
                    Some(&mut label),
                    None,
                    None,
                    None,
                    Some(&mut filesystem),
                );
                Some(DiskInfo {
                    root,
                    label: from_wide(&label),
                    filesystem: from_wide(&filesystem),
                    total_bytes: total,
                    free_bytes: available,
                })
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Weather (WinHTTP + Open-Meteo)
// ---------------------------------------------------------------------------

struct Internet(*mut c_void);

impl Drop for Internet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn https_get(host: &str, path: &str) -> Result<String, String> {
    let error = |what: &str, e: windows::core::Error| format!("{what}: {}", e.message());
    unsafe {
        let session = Internet(WinHttpOpen(
            USER_AGENT,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            return Err(error("WinHttpOpen", windows::core::Error::from_thread()));
        }
        let _ = WinHttpSetTimeouts(session.0, 5000, 5000, 5000, 10000);
        let host = wide(host);
        let connection = Internet(WinHttpConnect(
            session.0,
            PCWSTR(host.as_ptr()),
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ));
        if connection.0.is_null() {
            return Err(error("WinHttpConnect", windows::core::Error::from_thread()));
        }
        let path = wide(path);
        let request = Internet(WinHttpOpenRequest(
            connection.0,
            w!("GET"),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if request.0.is_null() {
            return Err(error(
                "WinHttpOpenRequest",
                windows::core::Error::from_thread(),
            ));
        }
        WinHttpSendRequest(request.0, None, None, 0, 0, 0).map_err(|e| error("send", e))?;
        WinHttpReceiveResponse(request.0, std::ptr::null_mut()).map_err(|e| error("receive", e))?;
        let mut status = 0u32;
        let mut length = std::mem::size_of::<u32>() as u32;
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some(&mut status as *mut u32 as *mut c_void),
            &mut length,
            std::ptr::null_mut(),
        )
        .map_err(|e| error("status", e))?;
        if status != 200 {
            return Err(format!("HTTP {status}"));
        }
        let mut body = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            let mut read = 0u32;
            WinHttpReadData(
                request.0,
                chunk.as_mut_ptr() as *mut c_void,
                chunk.len() as u32,
                &mut read,
            )
            .map_err(|e| error("read", e))?;
            if read == 0 {
                break;
            }
            if body.len() + read as usize > HTTP_MAX_BODY {
                return Err("response too large".into());
            }
            body.extend_from_slice(&chunk[..read as usize]);
        }
        String::from_utf8(body).map_err(|_| "response is not UTF-8".into())
    }
}

/// Percent-encodes a query component (RFC 3986 unreserved characters kept).
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 3);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Maps a WMO weather code (Open-Meteo `weather_code`) to the UI status key.
pub fn weather_status(code: u32, is_day: bool) -> Option<&'static str> {
    let (day, night) = match code {
        0 | 1 => ("clear_day", "clear_night"),
        2 | 3 | 45 | 48 => ("cloudy_day", "cloudy_night"),
        51 | 53 | 55 | 56 | 57 | 61 | 80 => ("light_rain_day", "light_rain_night"),
        63 | 65 | 66 | 67 | 81 | 82 => ("heavy_rain_day", "heavy_rain_night"),
        71 | 73 | 75 | 77 | 85 | 86 => ("snow_day", "snow_night"),
        95 | 96 | 99 => ("thunder_day", "thunder_night"),
        _ => return None,
    };
    Some(if is_day { day } else { night })
}

fn json_f64(value: &serde_json::Value) -> Option<f64> {
    match value {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|v: &f64| v.is_finite())
}

fn checked_location(latitude: f64, longitude: f64, label: String) -> Result<Location, String> {
    if (-90.0..=90.0).contains(&latitude) && (-180.0..=180.0).contains(&longitude) {
        Ok(Location {
            latitude,
            longitude,
            label,
        })
    } else {
        Err("coordinates out of range".into())
    }
}

/// Parses an Open-Meteo `/v1/forecast?current=temperature_2m,is_day,weather_code,wind_speed_10m` body.
pub fn parse_open_meteo(json: &str) -> Result<WeatherReading, String> {
    let root: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if root.get("error").and_then(|v| v.as_bool()) == Some(true) {
        let reason = root
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        return Err(format!("Open-Meteo: {reason}"));
    }
    let current = root.get("current").ok_or("missing current block")?;
    let celsius = current
        .get("temperature_2m")
        .and_then(json_f64)
        .ok_or("missing temperature_2m")?;
    let code = current
        .get("weather_code")
        .and_then(|v| v.as_u64())
        .and_then(|v| u32::try_from(v).ok())
        .ok_or("missing weather_code")?;
    let is_day = current
        .get("is_day")
        .and_then(|v| v.as_u64())
        .ok_or("missing is_day")?
        != 0;
    let wind_kmh = current
        .get("wind_speed_10m")
        .and_then(json_f64)
        .ok_or("missing wind_speed_10m")?;
    Ok(WeatherReading {
        status: weather_status(code, is_day),
        weather_code: code,
        celsius,
        fahrenheit: celsius * 9.0 / 5.0 + 32.0,
        wind_kmh,
        is_day,
    })
}

/// Parses ipapi.co (`latitude`/`longitude` numbers) or get.geojs.io (strings) responses.
pub fn parse_ip_location(json: &str) -> Result<Location, String> {
    let root: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if root.get("error").and_then(|v| v.as_bool()) == Some(true) {
        let reason = root
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("error");
        return Err(format!("IP geolocation: {reason}"));
    }
    let latitude = root
        .get("latitude")
        .and_then(json_f64)
        .ok_or("missing latitude")?;
    let longitude = root
        .get("longitude")
        .and_then(json_f64)
        .ok_or("missing longitude")?;
    let text = |key: &str| {
        root.get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let label = match (text("city"), text("country_name").or(text("country"))) {
        (Some(city), Some(country)) => format!("{city}, {country}"),
        (Some(city), None) => city.to_string(),
        (None, Some(country)) => country.to_string(),
        (None, None) => String::new(),
    };
    checked_location(latitude, longitude, label)
}

/// Parses an Open-Meteo geocoding (`/v1/search`) response, taking the first result.
pub fn parse_geocoding(json: &str) -> Result<Location, String> {
    let root: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let first = root
        .get("results")
        .and_then(|v| v.as_array())
        .and_then(|v| v.first())
        .ok_or("city not found")?;
    let latitude = first
        .get("latitude")
        .and_then(json_f64)
        .ok_or("missing latitude")?;
    let longitude = first
        .get("longitude")
        .and_then(json_f64)
        .ok_or("missing longitude")?;
    let name = first
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let label = match first.get("country").and_then(|v| v.as_str()) {
        Some(country) if !name.is_empty() => format!("{name}, {country}"),
        _ => name.to_string(),
    };
    checked_location(latitude, longitude, label)
}

fn resolve_location(config: &LocationConfig) -> Result<Location, String> {
    match config {
        LocationConfig::Coordinates(location) => checked_location(
            location.latitude,
            location.longitude,
            location.label.clone(),
        ),
        LocationConfig::City(name) => {
            let name = name.trim();
            if name.is_empty() {
                return Err("empty city name".into());
            }
            let path = format!(
                "/v1/search?name={}&count=1&language=en&format=json",
                percent_encode(name)
            );
            parse_geocoding(&https_get("geocoding-api.open-meteo.com", &path)?)
        }
        LocationConfig::Auto => https_get("ipapi.co", "/json/")
            .and_then(|body| parse_ip_location(&body))
            .or_else(|first| {
                https_get("get.geojs.io", "/v1/ip/geo.json")
                    .and_then(|body| parse_ip_location(&body))
                    .map_err(|second| format!("{first}; {second}"))
            }),
    }
}

fn fetch_weather(location: &Location) -> Result<WeatherReading, String> {
    let path = format!(
        "/v1/forecast?latitude={:.4}&longitude={:.4}&current=temperature_2m,is_day,weather_code,wind_speed_10m&wind_speed_unit=kmh",
        location.latitude, location.longitude
    );
    parse_open_meteo(&https_get("api.open-meteo.com", &path)?)
}

fn unix_secs(time: SystemTime) -> Option<u64> {
    time.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn weather_state(
    last: &Option<(WeatherReading, SystemTime)>,
    location: &Option<Location>,
    error: Option<String>,
) -> WeatherState {
    WeatherState {
        available: error.is_none() && last.is_some(),
        reading: last.as_ref().map(|(reading, _)| reading.clone()),
        location: location.clone(),
        fetched_unix: last.as_ref().and_then(|(_, at)| unix_secs(*at)),
        age_secs: last
            .as_ref()
            .and_then(|(_, at)| SystemTime::now().duration_since(*at).ok())
            .map(|d| d.as_secs()),
        error,
    }
}

fn weather_worker(shared: Arc<Shared>, commands: Receiver<WeatherMsg>) {
    let mut subscribed = false;
    let mut config = LocationConfig::Auto;
    let mut location: Option<Location> = None;
    let mut last: Option<(WeatherReading, SystemTime)> = None;
    let mut next: Option<Instant> = None;
    // Whether the most recent fetch attempt failed, and when it was made.
    let mut failed = false;
    let mut attempted: Option<Instant> = None;
    loop {
        let message = match next.filter(|_| subscribed) {
            Some(due) => commands.recv_timeout(due.saturating_duration_since(Instant::now())),
            None => commands.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        if shared.quit.load(Ordering::Acquire) {
            break;
        }
        let fetch = match message {
            Ok(WeatherMsg::Subscribe(on)) => {
                subscribed = on;
                let age = last
                    .as_ref()
                    .and_then(|(_, at)| SystemTime::now().duration_since(*at).ok());
                match age {
                    Some(age) if on && age < WEATHER_REFRESH => {
                        shared.publish(Update::Weather(weather_state(&last, &location, None)));
                        next = Some(Instant::now() + (WEATHER_REFRESH - age));
                        false
                    }
                    _ => on,
                }
            }
            Ok(WeatherMsg::Refresh) => true,
            Ok(WeatherMsg::Location(new_config)) => {
                if new_config != config {
                    config = new_config;
                    location = None;
                }
                subscribed
            }
            Ok(WeatherMsg::NetworkChanged) if subscribed && failed => {
                match attempted.map(|at| at + WEATHER_NET_RETRY_GAP) {
                    // Too soon after the last attempt: pull the retry forward instead.
                    Some(earliest) if earliest > Instant::now() => {
                        next = Some(next.map_or(earliest, |n| n.min(earliest)));
                        false
                    }
                    _ => true,
                }
            }
            Ok(WeatherMsg::NetworkChanged) => false,
            Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if !fetch {
            continue;
        }
        attempted = Some(Instant::now());
        let resolved = match &location {
            Some(cached) => Ok(cached.clone()),
            None => resolve_location(&config),
        };
        let result = resolved.and_then(|resolved| {
            location = Some(resolved.clone());
            fetch_weather(&resolved)
        });
        let error = match result {
            Ok(reading) => {
                last = Some((reading, SystemTime::now()));
                next = Some(Instant::now() + WEATHER_REFRESH);
                failed = false;
                None
            }
            Err(error) => {
                next = Some(Instant::now() + WEATHER_RETRY);
                failed = true;
                Some(error)
            }
        };
        if shared.quit.load(Ordering::Acquire) {
            break;
        }
        shared.publish(Update::Weather(weather_state(&last, &location, error)));
    }
}

// ---------------------------------------------------------------------------
// Tests (pure functions only; nothing here touches system state)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn net(ssid: &str, signal: u8, saved: bool, connected: bool) -> WifiNetwork {
        WifiNetwork {
            ssid: ssid.into(),
            signal,
            secured: true,
            saved,
            connected,
            profile: saved.then(|| ssid.to_string()),
        }
    }

    #[test]
    fn weather_codes_map_to_ui_statuses() {
        assert_eq!(weather_status(0, true), Some("clear_day"));
        assert_eq!(weather_status(1, false), Some("clear_night"));
        assert_eq!(weather_status(3, true), Some("cloudy_day"));
        assert_eq!(weather_status(45, false), Some("cloudy_night"));
        assert_eq!(weather_status(51, true), Some("light_rain_day"));
        assert_eq!(weather_status(80, false), Some("light_rain_night"));
        assert_eq!(weather_status(65, true), Some("heavy_rain_day"));
        assert_eq!(weather_status(82, false), Some("heavy_rain_night"));
        assert_eq!(weather_status(75, true), Some("snow_day"));
        assert_eq!(weather_status(86, false), Some("snow_night"));
        assert_eq!(weather_status(95, true), Some("thunder_day"));
        assert_eq!(weather_status(99, false), Some("thunder_night"));
        assert_eq!(weather_status(42, true), None);
    }

    #[test]
    fn parses_open_meteo_fixture() {
        let fixture = r#"{"latitude":52.52,"longitude":13.419998,"generationtime_ms":0.05,
            "utc_offset_seconds":0,"timezone":"GMT","timezone_abbreviation":"GMT","elevation":38.0,
            "current_units":{"time":"iso8601","interval":"seconds","temperature_2m":"°C","is_day":"",
            "weather_code":"wmo code","wind_speed_10m":"km/h"},
            "current":{"time":"2026-10-16T12:00","interval":900,"temperature_2m":12.5,"is_day":1,
            "weather_code":61,"wind_speed_10m":14.8}}"#;
        let reading = parse_open_meteo(fixture).expect("fixture parses");
        assert_eq!(reading.status, Some("light_rain_day"));
        assert_eq!(reading.weather_code, 61);
        assert!(reading.is_day);
        assert!((reading.celsius - 12.5).abs() < 1e-9);
        assert!((reading.fahrenheit - 54.5).abs() < 1e-9);
        assert!((reading.wind_kmh - 14.8).abs() < 1e-9);
    }

    #[test]
    fn rejects_incomplete_open_meteo() {
        assert!(parse_open_meteo(r#"{"current":{"temperature_2m":1.0}}"#).is_err());
        assert!(
            parse_open_meteo(r#"{"error":true,"reason":"bad latitude"}"#)
                .is_err_and(|e| e.contains("bad latitude"))
        );
        assert!(parse_open_meteo("not json").is_err());
    }

    #[test]
    fn parses_locations() {
        let ipapi = r#"{"ip":"x","city":"Riyadh","country_name":"Saudi Arabia","latitude":24.6877,"longitude":46.7219}"#;
        let location = parse_ip_location(ipapi).expect("ipapi parses");
        assert_eq!(location.label, "Riyadh, Saudi Arabia");
        assert!((location.latitude - 24.6877).abs() < 1e-9);
        let geojs =
            r#"{"city":"Berlin","country":"Germany","latitude":"52.5200","longitude":"13.4050"}"#;
        assert!((parse_ip_location(geojs).expect("geojs").longitude - 13.405).abs() < 1e-9);
        assert!(parse_ip_location(r#"{"error":true,"reason":"RateLimited"}"#).is_err());
        let geo = r#"{"results":[{"name":"Paris","latitude":48.85341,"longitude":2.3488,"country":"France"}]}"#;
        assert_eq!(
            parse_geocoding(geo).expect("geocoding").label,
            "Paris, France"
        );
        assert!(parse_geocoding(r#"{"generationtime_ms":0.1}"#).is_err());
        assert!(parse_ip_location(r#"{"latitude":120,"longitude":0}"#).is_err());
    }

    #[test]
    fn percent_encodes_city_names() {
        assert_eq!(percent_encode("New York"), "New%20York");
        assert_eq!(percent_encode("São Paulo&x"), "S%C3%A3o%20Paulo%26x");
    }

    #[test]
    fn wifi_list_dedupes_and_sorts() {
        let merged = merge_networks(vec![
            net("Cafe", 40, false, false),
            net("Home", 70, false, false),
            net("Home", 65, true, true),
            net("Office", 90, true, false),
            net("", 99, false, false),
            net("Cafe", 55, false, false),
            net("Zeta", 90, false, false),
        ]);
        let names: Vec<&str> = merged.iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(names, ["Home", "Office", "Zeta", "Cafe"]);
        assert_eq!(merged[0].signal, 70);
        assert!(merged[0].saved && merged[0].connected);
        assert_eq!(merged[0].profile.as_deref(), Some("Home"));
        assert_eq!(merged[3].signal, 55);
    }

    #[test]
    fn processes_group_by_name_and_rank() {
        let groups = group_processes(
            vec![
                ("chrome.exe".to_string(), 3.0, 100),
                ("Chrome.exe".to_string(), 2.0, 200),
                ("code.exe".to_string(), 4.0, 50),
                ("idle.exe".to_string(), 0.0, 900),
                ("tiny.exe".to_string(), 0.0, 10),
            ],
            3,
        );
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].name, "chrome.exe");
        assert_eq!(groups[0].instances, 2);
        assert_eq!(groups[0].working_set_bytes, 300);
        assert!((groups[0].cpu_percent - 5.0).abs() < 1e-6);
        assert_eq!(groups[1].name, "code.exe");
        assert_eq!(groups[2].name, "idle.exe");
    }

    #[test]
    fn mailbox_coalesces_snapshots_and_bounds_actions() {
        let (wake, _rx) = mpsc::sync_channel(1);
        let (weather, _weather_rx) = mpsc::sync_channel(1);
        let shared = Shared {
            mailbox: Mutex::new(Mailbox::default()),
            hwnd: 0,
            msg: 0,
            quit: AtomicBool::new(false),
            dirty: AtomicU32::new(0),
            wake,
            weather,
        };
        shared.publish(Update::Disks(Vec::new()));
        shared.publish(Update::Disks(Vec::new()));
        for _ in 0..40 {
            shared.action("test", Ok(()));
        }
        let mailbox = shared.mailbox.lock().expect("lock");
        assert_eq!(mailbox.latest.iter().flatten().count(), 1);
        assert_eq!(mailbox.actions.len(), MAX_ACTION_RESULTS);
    }
}
