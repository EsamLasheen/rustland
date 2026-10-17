//! Win32 integration of the tiling window manager. All policy lives in
//! `monarch_core::wm_layout`; this module only observes windows and executes effects.
//!
//! Threading: [`Wm::start`] spawns one dedicated thread that owns every piece of manager
//! state. That thread is per-monitor-v2 DPI aware, creates a hidden top-level window (for
//! private messages, timers and the display/work-area broadcasts), installs
//! out-of-context WinEvent hooks (delivered on that thread through its own message loop)
//! and pumps messages until [`Wm::stop`]. Every other public function only stores values in
//! atomics and posts a message to that thread, so it never blocks and is safe to call from
//! the integrator's low-level keyboard/mouse hook callbacks.
//!
//! Results: whenever bar-visible state changes, [`WM_STATE_CHANGED`] is posted to the
//! `notify` window given to [`Wm::start`]; its handler calls [`bar_state`].
//!
//! Recovery: windows on invisible workspaces are minimized (legacy `Minimize` hiding).
//! Each one is journaled in memory, in a lock-free slot table (for the panic hook) and on
//! disk (`wm-hidden.json` in the app data folder) before it is hidden. They are restored on
//! [`Wm::stop`], from the panic hook and by [`recover`] at the next start.
use monarch_core::wm_layout::{
    classify, exe_is, BarWorkspace, Config, Dir, Disposition, Effect, Journal, Model, NewWindow,
    WinId, WindowFacts, WmAction, MAX_JOURNAL, MAX_WINDOWS,
};
use monarch_core::Rect;
use std::{
    cell::RefCell,
    collections::{HashMap, VecDeque},
    ffi::c_void,
    fs, io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicIsize, AtomicU64, Ordering},
        mpsc, Mutex, Once, OnceLock,
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use windows::{
    core::{w, BOOL, PWSTR},
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY},
        System::{LibraryLoader::GetModuleHandleW, Threading::*},
        UI::{
            Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK},
            HiDpi::{SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2},
            Input::KeyboardAndMouse::*,
            WindowsAndMessaging::*,
        },
    },
};

/// Posted to the `notify` window when workspace/bar state changed. WPARAM/LPARAM are 0.
pub const WM_STATE_CHANGED: u32 = WM_APP + 60;

const MSG_ACTION: u32 = WM_APP + 1;
const MSG_POINTER: u32 = WM_APP + 2;
const MSG_DRAG_MOVE: u32 = WM_APP + 3;
const MSG_DRAG_RESIZE: u32 = WM_APP + 4;
const MSG_DRAG_UPDATE: u32 = WM_APP + 5;
const MSG_DRAG_END: u32 = WM_APP + 6;
const TIMER_HOVER: usize = 1;
const HOVER_DELAY_MS: u32 = 60;
const SUPPRESS_TTL: Duration = Duration::from_millis(400);
const SUPPRESS_CAP: usize = 256;
const BACKLOG_CAP: usize = 256;
const MIN_FLOAT_SIZE: i32 = 100;
/// A window that refuses its tile is re-laid out at most this often (2x/s).
const REFIT_INTERVAL: Duration = Duration::from_millis(500);
/// Bounded rounds of effect execution per event (effects may produce effects).
const FLUSH_ROUNDS: usize = 4;
const JOURNAL_FILE: &str = "wm-hidden.json";
const SHELL_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
];

static WINDOW: AtomicIsize = AtomicIsize::new(0);
static NOTIFY: AtomicIsize = AtomicIsize::new(0);
static POINTER: AtomicU64 = AtomicU64::new(0);
/// The hover timer is running; pointer moves need no message until it settles.
static HOVER_ARMED: AtomicBool = AtomicBool::new(false);
static DRAG_POINT: AtomicU64 = AtomicU64::new(0);
static DRAG_PENDING: AtomicBool = AtomicBool::new(false);
/// Lock-free mirror of the journal so the panic hook can restore without locking.
static HIDDEN: [AtomicIsize; MAX_JOURNAL] = [const { AtomicIsize::new(0) }; MAX_JOURNAL];
static BAR: Mutex<Vec<Vec<BarWorkspace>>> = Mutex::new(Vec::new());
static JOURNAL_PATH: OnceLock<PathBuf> = OnceLock::new();
static PANIC_HOOK: Once = Once::new();

/// Win+left-drag moves, Win+right-drag resizes (AltSnap semantics).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragKind {
    Move,
    Resize,
}

fn pack(x: i32, y: i32) -> u64 {
    ((x as u32 as u64) << 32) | y as u32 as u64
}
fn unpack(v: u64) -> (i32, i32) {
    ((v >> 32) as u32 as i32, v as u32 as i32)
}
fn hwnd(key: isize) -> HWND {
    HWND(key as *mut c_void)
}
fn id(h: HWND) -> WinId {
    h.0 as isize as WinId
}
fn post(msg: u32, w: usize, l: isize) -> bool {
    let target = WINDOW.load(Ordering::Acquire);
    target != 0 && unsafe { PostMessageW(Some(hwnd(target)), msg, WPARAM(w), LPARAM(l)).is_ok() }
}

// ---------------------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------------------

/// Owner of the manager thread. Dropping it stops the manager and restores hidden windows.
pub struct Wm {
    thread: Option<JoinHandle<()>>,
}

impl Wm {
    /// Recover windows left hidden by a previous crash, then start managing windows.
    /// `notify` receives [`WM_STATE_CHANGED`].
    pub fn start(notify: HWND) -> io::Result<Self> {
        if WINDOW.load(Ordering::Acquire) != 0 {
            return Err(io::Error::other("Window manager already running"));
        }
        install_panic_hook();
        NOTIFY.store(notify.0 as isize, Ordering::Release);
        let (tx, rx) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("monarch-wm".into())
            .spawn(move || run(tx))?;
        match rx.recv() {
            Ok(Ok(())) => Ok(Self {
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(io::Error::other(
                    "Window manager thread exited during start",
                ))
            }
        }
    }
    /// Stop managing, restore every window the manager hid and join the thread.
    pub fn stop(mut self) {
        self.shutdown();
    }
    fn shutdown(&mut self) {
        let _ = post(WM_CLOSE, 0, 0);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        NOTIFY.store(0, Ordering::Release);
    }
}
impl Drop for Wm {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn is_running() -> bool {
    WINDOW.load(Ordering::Acquire) != 0
}

/// Queue an action for the manager thread. Returns false when the manager is not running.
pub fn perform(action: WmAction) -> bool {
    post(MSG_ACTION, action.encode(), 0)
}

/// Focus-follows-mouse input: call for every pointer move (screen coordinates, physical
/// pixels as delivered to WH_MOUSE_LL). Coalesced; never blocks. A tiled window under a
/// pointer that rested for ~60 ms is focused. Only the first move after the pointer
/// settled posts a message; it arms one timer that re-checks the position when it fires.
pub fn pointer_moved(x: i32, y: i32) {
    POINTER.store(pack(x, y), Ordering::SeqCst);
    if !HOVER_ARMED.swap(true, Ordering::SeqCst) && !post(MSG_POINTER, 0, 0) {
        HOVER_ARMED.store(false, Ordering::SeqCst);
    }
}

/// Start a Win+drag at the pointer. Returns true when a draggable top-level window is
/// under the pointer; the integrator should then swallow the button press and feed
/// [`drag_update`]/[`drag_end`].
pub fn drag_begin(x: i32, y: i32, kind: DragKind) -> bool {
    if !is_running() {
        return false;
    }
    let Some(h) = (unsafe { draggable_at(x, y) }) else {
        return false;
    };
    DRAG_POINT.store(pack(x, y), Ordering::Relaxed);
    let msg = match kind {
        DragKind::Move => MSG_DRAG_MOVE,
        DragKind::Resize => MSG_DRAG_RESIZE,
    };
    post(msg, pack(x, y) as usize, h.0 as isize)
}

/// Pointer moved during a drag. Coalesced; never blocks.
pub fn drag_update(x: i32, y: i32) {
    DRAG_POINT.store(pack(x, y), Ordering::Relaxed);
    if !DRAG_PENDING.swap(true, Ordering::AcqRel) && !post(MSG_DRAG_UPDATE, 0, 0) {
        DRAG_PENDING.store(false, Ordering::Release);
    }
}

/// Drag finished (button released). Tiled windows swap with the tile under the pointer.
pub fn drag_end(x: i32, y: i32) {
    let _ = post(MSG_DRAG_END, pack(x, y) as usize, 0);
}

/// Workspaces of `monitor` for the bar: name, focused flag and (hwnd, exe) per window.
/// Window ids are HWND values: `HWND(id as isize as *mut c_void)`.
pub fn bar_state(monitor: usize) -> Vec<BarWorkspace> {
    BAR.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(monitor)
        .cloned()
        .unwrap_or_default()
}

/// Restore windows listed in the on-disk journal of a previous run that did not exit
/// cleanly. Only still-minimized windows of the same process are touched. Returns the
/// number restored. A corrupt journal is kept as `wm-hidden.corrupt.json`.
/// Independent of the manager: safe to call when the manager is disabled or not started.
pub fn recover() -> io::Result<usize> {
    let path = journal_path()?;
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    match Journal::parse(&bytes) {
        Ok(entries) => {
            let mut restored = 0;
            for e in entries {
                let h = hwnd(e.hwnd as isize);
                unsafe {
                    if IsWindow(Some(h)).as_bool() && pid_of(h) == e.pid && IsIconic(h).as_bool() {
                        let _ = ShowWindowAsync(h, SW_SHOWNOACTIVATE);
                        restored += 1;
                    }
                }
            }
            fs::remove_file(&path)?;
            Ok(restored)
        }
        Err(e) => {
            let _ = fs::rename(&path, path.with_extension("corrupt.json"));
            Err(e)
        }
    }
}

/// Restore hidden windows if the main thread or the manager thread panics (a panic on
/// any other worker thread does not end the shell or the manager). Idempotent;
/// [`Wm::start`] calls it.
pub fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if matches!(std::thread::current().name(), Some("main" | "monarch-wm")) {
                restore_slots();
            }
            previous(info);
        }));
    });
}

fn restore_slots() {
    let mut any = false;
    for slot in &HIDDEN {
        let v = slot.swap(0, Ordering::AcqRel);
        if v != 0 {
            any = true;
            unsafe {
                let _ = ShowWindowAsync(hwnd(v), SW_SHOWNOACTIVATE);
            }
        }
    }
    if any {
        if let Some(p) = JOURNAL_PATH.get() {
            let _ = fs::remove_file(p);
        }
    }
}

fn journal_path() -> io::Result<PathBuf> {
    if let Some(p) = JOURNAL_PATH.get() {
        return Ok(p.clone());
    }
    let p = monarch_platform::paths()?.data.join(JOURNAL_FILE);
    Ok(JOURNAL_PATH.get_or_init(|| p).clone())
}

// ---------------------------------------------------------------------------------------
// Manager thread
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Event {
    Startup,
    Win(u32, isize),
    Action(WmAction),
    Pointer,
    Hover,
    DragBegin(isize, (i32, i32), DragKind),
    DragUpdate((i32, i32)),
    DragEnd((i32, i32)),
    Display,
    Shutdown,
}

thread_local! {
    static ENGINE: RefCell<Option<Engine>> = const { RefCell::new(None) };
    static BACKLOG: RefCell<VecDeque<Event>> = const { RefCell::new(VecDeque::new()) };
}

/// Run an event on the engine. Events arriving re-entrantly (a WinEvent delivered while the
/// engine is executing effects) are queued and handled right after, in order.
fn dispatch(ev: Event) {
    ENGINE.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            BACKLOG.with(|b| {
                if let Ok(mut b) = b.try_borrow_mut() {
                    if b.len() < BACKLOG_CAP {
                        b.push_back(ev);
                    }
                }
            });
            return;
        };
        let Some(engine) = guard.as_mut() else {
            return;
        };
        engine.handle(ev);
        while let Some(next) =
            BACKLOG.with(|b| b.try_borrow_mut().ok().and_then(|mut b| b.pop_front()))
        {
            engine.handle(next);
        }
    });
}

/// Clears the running flag even if the thread unwinds.
struct RunningGuard;
impl Drop for RunningGuard {
    fn drop(&mut self) {
        WINDOW.store(0, Ordering::Release);
    }
}

fn run(ready: mpsc::SyncSender<io::Result<()>>) {
    let _guard = RunningGuard;
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let window = match create_window() {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(io::Error::other(e)));
                return;
            }
        };
        if let Err(e) = recover() {
            if let Ok(p) = monarch_platform::paths() {
                monarch_platform::log(&p, &format!("WM recovery: {e}"));
            }
        }
        let mut engine = Engine::new(window);
        engine.hooks = install_hooks();
        if engine.hooks.is_empty() {
            let _ = DestroyWindow(window);
            let _ = ready.send(Err(io::Error::other(
                "Could not install window event hooks",
            )));
            return;
        }
        ENGINE.with(|c| *c.borrow_mut() = Some(engine));
        WINDOW.store(window.0 as isize, Ordering::Release);
        dispatch(Event::Startup);
        let _ = ready.send(Ok(()));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            DispatchMessageW(&msg);
        }
        let leftover = ENGINE.with(|c| c.try_borrow_mut().ok().and_then(|mut e| e.take()));
        drop(leftover);
    }
}

unsafe fn create_window() -> windows::core::Result<HWND> {
    let module = GetModuleHandleW(None)?;
    let class = w!("MonarchNext.WindowManager");
    let wc = WNDCLASSW {
        hInstance: module.into(),
        lpszClassName: class,
        lpfnWndProc: Some(wndproc),
        ..Default::default()
    };
    // Registration fails harmlessly when the class survives a previous start.
    RegisterClassW(&wc);
    CreateWindowExW(
        WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        class,
        w!("Monarch Next window manager"),
        WS_POPUP,
        0,
        0,
        0,
        0,
        None,
        None,
        Some(module.into()),
        None,
    )
}

unsafe fn install_hooks() -> Vec<HWINEVENTHOOK> {
    [
        (EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND),
        (EVENT_SYSTEM_MOVESIZESTART, EVENT_SYSTEM_MOVESIZEEND),
        (EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND),
        (EVENT_OBJECT_DESTROY, EVENT_OBJECT_HIDE),
        (EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_LOCATIONCHANGE),
        (EVENT_OBJECT_CLOAKED, EVENT_OBJECT_UNCLOAKED),
    ]
    .into_iter()
    .map(|(min, max)| {
        SetWinEventHook(
            min,
            max,
            None,
            Some(on_win_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    })
    .filter(|h| !h.is_invalid())
    .collect()
}

unsafe extern "system" fn on_win_event(
    _hook: HWINEVENTHOOK,
    event: u32,
    h: HWND,
    object: i32,
    child: i32,
    _thread: u32,
    _time: u32,
) {
    if object != OBJID_WINDOW.0 || child != CHILDID_SELF as i32 || h.is_invalid() {
        return;
    }
    dispatch(Event::Win(event, h.0 as isize));
}

unsafe extern "system" fn wndproc(h: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        MSG_ACTION => {
            if let Some(a) = WmAction::decode(w.0) {
                dispatch(Event::Action(a));
            }
        }
        MSG_POINTER => dispatch(Event::Pointer),
        MSG_DRAG_MOVE | MSG_DRAG_RESIZE => {
            let kind = if msg == MSG_DRAG_MOVE {
                DragKind::Move
            } else {
                DragKind::Resize
            };
            dispatch(Event::DragBegin(l.0, unpack(w.0 as u64), kind));
        }
        MSG_DRAG_UPDATE => {
            DRAG_PENDING.store(false, Ordering::Release);
            dispatch(Event::DragUpdate(unpack(
                DRAG_POINT.load(Ordering::Relaxed),
            )));
        }
        MSG_DRAG_END => dispatch(Event::DragEnd(unpack(w.0 as u64))),
        WM_TIMER if w.0 == TIMER_HOVER => dispatch(Event::Hover),
        WM_DISPLAYCHANGE => dispatch(Event::Display),
        WM_SETTINGCHANGE if w.0 == SPI_SETWORKAREA.0 as usize => dispatch(Event::Display),
        WM_CLOSE => {
            dispatch(Event::Shutdown);
            let _ = DestroyWindow(h);
        }
        WM_DESTROY => {
            let engine = ENGINE.with(|c| c.try_borrow_mut().ok().and_then(|mut e| e.take()));
            drop(engine);
            PostQuitMessage(0);
        }
        _ => return DefWindowProcW(h, msg, w, l),
    }
    LRESULT(0)
}

// ---------------------------------------------------------------------------------------
// Win32 helpers
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Insets {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

fn to_rect(r: RECT) -> Rect {
    Rect {
        x: r.left,
        y: r.top,
        width: r.right - r.left,
        height: r.bottom - r.top,
    }
}

unsafe fn pid_of(h: HWND) -> u32 {
    let mut pid = 0;
    GetWindowThreadProcessId(h, Some(&mut pid));
    pid
}

/// (window rect, visual rect). The visual rect excludes the invisible resize borders.
unsafe fn frame(h: HWND) -> Option<(RECT, RECT)> {
    let mut wr = RECT::default();
    GetWindowRect(h, &mut wr).ok()?;
    let mut vr = RECT::default();
    if DwmGetWindowAttribute(
        h,
        DWMWA_EXTENDED_FRAME_BOUNDS,
        &mut vr as *mut RECT as *mut c_void,
        std::mem::size_of::<RECT>() as u32,
    )
    .is_err()
    {
        vr = wr;
    }
    Some((wr, vr))
}

unsafe fn is_cloaked(h: HWND) -> bool {
    let mut cloaked = 0u32;
    DwmGetWindowAttribute(
        h,
        DWMWA_CLOAKED,
        &mut cloaked as *mut u32 as *mut c_void,
        std::mem::size_of::<u32>() as u32,
    )
    .is_ok()
        && cloaked != 0
}

unsafe fn class_of(h: HWND, buf: &mut [u16; 256]) -> usize {
    GetClassNameW(h, buf).max(0) as usize
}

unsafe fn class_is(h: HWND, names: &[&str]) -> bool {
    let mut buf = [0u16; 256];
    let n = class_of(h, &mut buf);
    let class = String::from_utf16_lossy(&buf[..n]);
    names.contains(&class.as_str())
}

unsafe fn process_exe(pid: u32) -> String {
    let Ok(p) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
        return String::new();
    };
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let ok = QueryFullProcessImageNameW(p, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
        .is_ok();
    let _ = CloseHandle(p);
    if !ok {
        return String::new();
    }
    let path = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
    path.rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

unsafe extern "system" fn find_foreign_child(h: HWND, l: LPARAM) -> BOOL {
    let state = &mut *(l.0 as *mut (u32, u32));
    let pid = pid_of(h);
    if pid != 0 && pid != state.0 {
        state.1 = pid;
        return BOOL(0);
    }
    BOOL(1)
}

/// Executable of a window; UWP frames report the hosted app instead of ApplicationFrameHost.
unsafe fn window_exe(h: HWND, pid: u32) -> String {
    let exe = process_exe(pid);
    if exe_is(&exe, "ApplicationFrameHost") {
        let mut state = (pid, 0u32);
        let _ = EnumChildWindows(
            Some(h),
            Some(find_foreign_child),
            LPARAM(&mut state as *mut (u32, u32) as isize),
        );
        if state.1 != 0 {
            let inner = process_exe(state.1);
            if !inner.is_empty() {
                return inner;
            }
        }
    }
    exe
}

unsafe fn root_at(x: i32, y: i32) -> Option<HWND> {
    let h = WindowFromPoint(POINT { x, y });
    if h.is_invalid() {
        return None;
    }
    let root = GetAncestor(h, GA_ROOT);
    (!root.is_invalid()).then_some(root)
}

/// Quick, hook-safe check for Win+drag targets.
unsafe fn draggable_at(x: i32, y: i32) -> Option<HWND> {
    let h = root_at(x, y)?;
    if h == GetShellWindow() || h == GetDesktopWindow() || IsZoomed(h).as_bool() {
        return None;
    }
    if pid_of(h) == GetCurrentProcessId() || !IsWindowVisible(h).as_bool() {
        return None;
    }
    if class_is(h, SHELL_CLASSES) {
        return None;
    }
    Some(h)
}

/// Whether the process runs elevated. A process whose token we may not query (access
/// denied) is treated as elevated: a non-elevated manager cannot move its windows.
unsafe fn process_elevated(pid: u32) -> bool {
    let denied = |e: &windows::core::Error| e.code() == ERROR_ACCESS_DENIED.to_hresult();
    let process = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
        Ok(p) => p,
        Err(e) => return denied(&e),
    };
    let mut token = HANDLE::default();
    let elevated = match OpenProcessToken(process, TOKEN_QUERY, &mut token) {
        Err(e) => denied(&e),
        Ok(()) => {
            let mut info = TOKEN_ELEVATION::default();
            let mut len = 0u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut info as *mut TOKEN_ELEVATION as *mut c_void),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut len,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && info.TokenIsElevated != 0
        }
    };
    let _ = CloseHandle(process);
    elevated
}

/// The window shows a modal popup, a menu, or is being moved/sized.
unsafe fn in_modal_ui(h: HWND) -> bool {
    if GetWindow(h, GW_ENABLEDPOPUP).is_ok_and(|p| p != h) {
        return true;
    }
    let thread = GetWindowThreadProcessId(h, None);
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let busy = GUI_INMENUMODE.0 | GUI_INMOVESIZE.0 | GUI_POPUPMENUMODE.0 | GUI_SYSTEMMENUMODE.0;
    GetGUIThreadInfo(thread, &mut info).is_ok() && info.flags.0 & busy != 0
}

unsafe fn toggle_maximize(h: HWND) {
    let cmd = if IsZoomed(h).as_bool() {
        SW_RESTORE
    } else {
        SW_MAXIMIZE
    };
    let _ = ShowWindowAsync(h, cmd);
}

/// Windows only lets the process that received the last input take the foreground. An
/// unassigned virtual key (0xE8, also used by shortcuts.rs) makes this process that one.
unsafe fn focus(h: HWND) {
    if !IsWindow(Some(h)).as_bool() {
        return;
    }
    if SetForegroundWindow(h).as_bool() && GetForegroundWindow() == h {
        return;
    }
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0xE8),
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    SendInput(
        &[key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)],
        std::mem::size_of::<INPUT>() as i32,
    );
    let _ = SetForegroundWindow(h);
}

unsafe extern "system" fn collect_monitor(m: HMONITOR, _dc: HDC, _r: *mut RECT, l: LPARAM) -> BOOL {
    let list = &mut *(l.0 as *mut Vec<(isize, Rect, bool)>);
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if list.len() < 16 && GetMonitorInfoW(m, &mut info).as_bool() {
        list.push((
            m.0 as isize,
            to_rect(info.rcWork),
            info.dwFlags & MONITORINFOF_PRIMARY != 0,
        ));
    }
    BOOL(1)
}

/// Monitors (handle, work area) with the primary first, then left to right.
unsafe fn read_monitors() -> Vec<(isize, Rect)> {
    let mut list: Vec<(isize, Rect, bool)> = Vec::new();
    let _ = EnumDisplayMonitors(
        None,
        None,
        Some(collect_monitor),
        LPARAM(&mut list as *mut Vec<(isize, Rect, bool)> as isize),
    );
    list.sort_by_key(|(_, r, primary)| (!*primary, r.x, r.y));
    list.into_iter().map(|(h, r, _)| (h, r)).collect()
}

unsafe extern "system" fn collect_window(h: HWND, l: LPARAM) -> BOOL {
    let list = &mut *(l.0 as *mut Vec<isize>);
    if list.len() < 4096 && IsWindowVisible(h).as_bool() {
        list.push(h.0 as isize);
    }
    BOOL(1)
}

// ---------------------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct Drag {
    hwnd: isize,
    kind: DragKind,
    start: (i32, i32),
    last: (i32, i32),
    rect: RECT,
    /// -1 = left/top edge, 1 = right/bottom edge, 0 = untouched.
    edges: (i32, i32),
    tiled: bool,
}

/// A native (title bar / border) move or size loop in progress.
#[derive(Clone, Copy, Debug)]
struct MoveSize {
    hwnd: isize,
    start: Rect,
    zoomed: bool,
}

struct Engine {
    model: Model,
    window: HWND,
    started: Instant,
    own_pid: u32,
    hooks: Vec<HWINEVENTHOOK>,
    monitors: Vec<isize>,
    suppressed: Vec<(isize, Instant)>,
    insets: HashMap<isize, Insets>,
    /// Placements for windows that were still minimized; applied when they restore.
    pending: HashMap<isize, Rect>,
    /// Last visual rect requested per window (to detect windows refusing their tile).
    placed: HashMap<isize, Rect>,
    /// Last min-size re-layout per window (rate limit).
    refit: HashMap<isize, Instant>,
    out: Vec<Effect>,
    drag: Option<Drag>,
    movesize: Option<MoveSize>,
    /// This process is elevated (then elevated windows can be managed).
    elevated: bool,
    /// Pointer position at the previous hover tick.
    hover_pos: u64,
    /// Root window under the pointer at the last settled hover.
    last_hover: isize,
    bar_dirty: bool,
}

impl Engine {
    unsafe fn new(window: HWND) -> Self {
        let mons = read_monitors();
        let works: Vec<Rect> = mons.iter().map(|(_, r)| *r).collect();
        Self {
            model: Model::new(Config::default(), &works),
            window,
            started: Instant::now(),
            own_pid: GetCurrentProcessId(),
            hooks: Vec::new(),
            monitors: mons.into_iter().map(|(h, _)| h).collect(),
            suppressed: Vec::with_capacity(SUPPRESS_CAP),
            insets: HashMap::new(),
            pending: HashMap::new(),
            placed: HashMap::new(),
            refit: HashMap::new(),
            out: Vec::with_capacity(64),
            drag: None,
            movesize: None,
            elevated: process_elevated(GetCurrentProcessId()),
            hover_pos: 0,
            last_hover: 0,
            bar_dirty: false,
        }
    }

    fn handle(&mut self, ev: Event) {
        unsafe {
            match ev {
                Event::Startup => self.startup(),
                Event::Win(event, key) => self.on_win(event, key),
                Event::Action(a) => self.action(a),
                Event::Pointer => self.arm_hover(),
                Event::Hover => self.hover_tick(),
                Event::DragBegin(key, pt, kind) => self.drag_begin(key, pt, kind),
                Event::DragUpdate(pt) => self.drag_update(pt),
                Event::DragEnd(pt) => self.drag_end(pt),
                Event::Display => {
                    self.display();
                }
                Event::Shutdown => {
                    self.shutdown();
                    return;
                }
            }
            self.flush();
        }
    }

    fn suppress(&mut self, key: isize) {
        let now = Instant::now();
        self.suppressed
            .retain(|(_, t)| now.duration_since(*t) < SUPPRESS_TTL);
        if self.suppressed.len() >= SUPPRESS_CAP {
            self.suppressed.remove(0);
        }
        self.suppressed.push((key, now));
    }
    fn is_suppressed(&self, key: isize) -> bool {
        let now = Instant::now();
        self.suppressed
            .iter()
            .any(|(k, t)| *k == key && now.duration_since(*t) < SUPPRESS_TTL)
    }
    fn forget(&mut self, key: isize) {
        self.insets.remove(&key);
        self.pending.remove(&key);
        self.placed.remove(&key);
        self.refit.remove(&key);
    }

    /// Shell, desktop and our own surfaces are never targets of window actions.
    unsafe fn is_shell(&self, h: HWND) -> bool {
        h == GetShellWindow()
            || h == GetDesktopWindow()
            || pid_of(h) == self.own_pid
            || class_is(h, SHELL_CLASSES)
    }

    /// Window actions act on the real foreground window, not a possibly stale model focus.
    unsafe fn action(&mut self, a: WmAction) {
        if a == WmAction::Retile {
            // Monitors or work areas may have changed without a broadcast.
            if !self.display() {
                self.model.perform(a, &mut self.out);
            }
            return;
        }
        let targets_window = matches!(
            a,
            WmAction::Close
                | WmAction::ToggleMaximize
                | WmAction::Minimize
                | WmAction::ToggleFloat
                | WmAction::SendToWorkspace(_)
                | WmAction::Move(_)
                | WmAction::Flip
                | WmAction::ResizeAxis { .. }
        );
        if targets_window {
            let fg = GetForegroundWindow();
            if fg.is_invalid() || self.is_shell(fg) {
                return;
            }
            let wid = id(fg);
            if self.model.window(wid).is_some() {
                self.model.on_foreground(wid, &mut self.out);
            }
            if self.model.focused_window() != Some(wid) {
                // Unmanaged (or minimized/hidden) foreground window: act on it directly.
                match a {
                    WmAction::Close => {
                        let _ = PostMessageW(Some(fg), WM_CLOSE, WPARAM(0), LPARAM(0));
                    }
                    WmAction::Minimize => {
                        let _ = ShowWindowAsync(fg, SW_MINIMIZE);
                    }
                    WmAction::ToggleMaximize => toggle_maximize(fg),
                    _ => {}
                }
                return;
            }
        }
        self.model.perform(a, &mut self.out);
    }
    fn monitor_index(&self, m: HMONITOR) -> usize {
        self.monitors
            .iter()
            .position(|x| *x == m.0 as isize)
            .unwrap_or(0)
    }

    unsafe fn startup(&mut self) {
        let mut list: Vec<isize> = Vec::new();
        let _ = EnumWindows(
            Some(collect_window),
            LPARAM(&mut list as *mut Vec<isize> as isize),
        );
        // Bottom of the z-order first so the foreground window ends up focused.
        for &key in list.iter().rev() {
            self.try_manage(hwnd(key));
            // One layout pass at the end instead of one per window.
            self.out.retain(|e| !matches!(e, Effect::Place(..)));
        }
        self.model.retile_all(&mut self.out);
        let fg = GetForegroundWindow();
        if !fg.is_invalid() {
            self.model.on_foreground(id(fg), &mut self.out);
        }
        self.bar_dirty = true;
    }

    /// Re-read monitors and re-lay out everything. False when no monitor could be read.
    unsafe fn display(&mut self) -> bool {
        let mons = read_monitors();
        if mons.is_empty() {
            return false;
        }
        let works: Vec<Rect> = mons.iter().map(|(_, r)| *r).collect();
        self.monitors = mons.into_iter().map(|(h, _)| h).collect();
        self.model.set_monitors(&works, &mut self.out);
        true
    }

    unsafe fn try_manage(&mut self, h: HWND) {
        if self.model.len() >= MAX_WINDOWS || GetAncestor(h, GA_ROOT) != h {
            return;
        }
        if !IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
            return;
        }
        let style = GetWindowLongW(h, GWL_STYLE) as u32;
        let ex = GetWindowLongW(h, GWL_EXSTYLE) as u32;
        if style & WS_CHILD.0 != 0 || ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
            return;
        }
        let pid = pid_of(h);
        if pid == 0 || pid == self.own_pid {
            return;
        }
        let mut cbuf = [0u16; 256];
        let n = class_of(h, &mut cbuf);
        let class = String::from_utf16_lossy(&cbuf[..n]);
        let mut tbuf = [0u16; 512];
        let n = GetWindowTextW(h, &mut tbuf).max(0) as usize;
        let title = String::from_utf16_lossy(&tbuf[..n]);
        let Some((_, vr)) = frame(h) else {
            return;
        };
        let owner = GetWindow(h, GW_OWNER).ok().filter(|o| !o.is_invalid());
        let exe = window_exe(h, pid);
        let facts = WindowFacts {
            exe: &exe,
            title: &title,
            class: &class,
            own_process: false,
            visible: true,
            cloaked: is_cloaked(h),
            child: false,
            tool_window: false,
            no_activate: false,
            app_window: ex & WS_EX_APPWINDOW.0 != 0,
            has_caption: style & WS_CAPTION.0 == WS_CAPTION.0,
            owned: owner.is_some_and(|o| IsWindowVisible(o).as_bool()),
            dialog_frame: ex & WS_EX_DLGMODALFRAME.0 != 0,
            resizable: style & WS_THICKFRAME.0 != 0,
            width: vr.right - vr.left,
            height: vr.bottom - vr.top,
        };
        let disposition = classify(&facts);
        if disposition == Disposition::Ignore {
            return;
        }
        // UIPI: a non-elevated manager cannot move elevated windows (e.g. Task Manager).
        if !self.elevated && process_elevated(pid) {
            return;
        }
        // Owned windows (dialogs) join their owner's workspace and are never hidden.
        let root_owner = GetAncestor(h, GA_ROOTOWNER);
        let owner = (!root_owner.is_invalid() && root_owner != h)
            .then(|| self.model.window(id(root_owner)))
            .flatten()
            .map(|w| (w.monitor, w.workspace));
        let monitor = match owner {
            Some((m, _)) => m,
            None => self.monitor_index(MonitorFromWindow(h, MONITOR_DEFAULTTONEAREST)),
        };
        let managed = self.model.manage(
            NewWindow {
                id: id(h),
                pid,
                exe: &exe,
                title: &title,
                monitor,
                floating: disposition == Disposition::Float,
                workspace: owner.map(|(_, ws)| ws),
            },
            self.started.elapsed(),
            &mut self.out,
        );
        if managed && IsZoomed(h).as_bool() {
            self.model.set_maximized(id(h), true, &mut self.out);
            self.out
                .retain(|e| !matches!(e, Effect::Place(w, _) if *w == id(h)));
        }
    }

    unsafe fn on_win(&mut self, event: u32, key: isize) {
        let h = hwnd(key);
        let wid = id(h);
        let managed = self.model.window(wid).is_some();
        match event {
            EVENT_OBJECT_LOCATIONCHANGE => {
                if !managed {
                    return;
                }
                // Minimized windows (including workspace-hidden ones) never report zoom.
                if IsIconic(h).as_bool() {
                    return;
                }
                if let Some(r) = self.pending.remove(&key) {
                    self.place(h, r);
                }
                let zoomed = IsZoomed(h).as_bool();
                if self
                    .model
                    .window(wid)
                    .is_some_and(|w| w.maximized != zoomed)
                {
                    self.model.set_maximized(wid, zoomed, &mut self.out);
                }
                if !zoomed {
                    self.check_min_size(h);
                }
            }
            EVENT_SYSTEM_FOREGROUND => {
                if !managed {
                    self.try_manage(h);
                } else if !IsIconic(h).as_bool() {
                    self.model.on_foreground(wid, &mut self.out);
                }
            }
            EVENT_OBJECT_SHOW | EVENT_OBJECT_UNCLOAKED => {
                if !managed {
                    self.try_manage(h);
                } else if event == EVENT_OBJECT_UNCLOAKED
                    && self.model.is_tiled(wid)
                    && self.model.is_visible(wid)
                {
                    // Back from another virtual desktop: re-apply the layout.
                    if let Some(mon) = self.model.window(wid).map(|w| w.monitor) {
                        self.model.retile_monitor(mon, &mut self.out);
                    }
                }
            }
            EVENT_SYSTEM_MINIMIZEEND => {
                if !managed {
                    self.try_manage(h);
                } else if !self.is_suppressed(key) {
                    self.model.on_restored(wid, &mut self.out);
                }
            }
            EVENT_SYSTEM_MINIMIZESTART => {
                if managed && !self.is_suppressed(key) {
                    self.model.on_minimized(wid, &mut self.out);
                }
            }
            EVENT_OBJECT_DESTROY => {
                if managed {
                    self.model.closed(wid, &mut self.out);
                    self.forget(key);
                }
            }
            EVENT_OBJECT_HIDE => {
                if managed && !self.is_suppressed(key) && !IsWindowVisible(h).as_bool() {
                    self.model.closed(wid, &mut self.out);
                    self.forget(key);
                }
            }
            // EVENT_OBJECT_CLOAKED (virtual desktop switch, suspended app) never
            // unmanages: only DESTROY/HIDE do.
            EVENT_SYSTEM_MOVESIZESTART => {
                self.movesize = frame(h).map(|(_, vr)| MoveSize {
                    hwnd: key,
                    start: to_rect(vr),
                    zoomed: IsZoomed(h).as_bool(),
                });
            }
            EVENT_SYSTEM_MOVESIZEEND => {
                let start = self.movesize.take().filter(|m| m.hwnd == key);
                if managed && self.model.is_tiled(wid) {
                    self.native_move_end(h, start);
                }
            }
            _ => {}
        }
    }

    /// First pointer move after the pointer settled: start the (single) hover timer.
    unsafe fn arm_hover(&mut self) {
        self.hover_pos = POINTER.load(Ordering::SeqCst);
        SetTimer(Some(self.window), TIMER_HOVER, HOVER_DELAY_MS, None);
    }

    /// Hover timer tick: keep ticking while the pointer moves; once it rested for a full
    /// period, stop the timer and run focus-follows-mouse.
    unsafe fn hover_tick(&mut self) {
        let pos = POINTER.load(Ordering::SeqCst);
        if pos != self.hover_pos {
            self.hover_pos = pos;
            return;
        }
        let _ = KillTimer(Some(self.window), TIMER_HOVER);
        HOVER_ARMED.store(false, Ordering::SeqCst);
        // A move that raced with disarming must not be lost.
        if POINTER.load(Ordering::SeqCst) != pos {
            if !HOVER_ARMED.swap(true, Ordering::SeqCst) {
                self.arm_hover();
            }
            return;
        }
        self.hover(pos);
    }

    /// Focus-follows-mouse after the pointer rested for HOVER_DELAY_MS.
    unsafe fn hover(&mut self, pos: u64) {
        if self.drag.is_some() {
            return;
        }
        for vk in [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON] {
            if GetAsyncKeyState(vk.0 as i32) < 0 {
                return;
            }
        }
        let (x, y) = unpack(pos);
        let Some(h) = root_at(x, y) else {
            return;
        };
        // Only a pointer that entered another window focuses it; small bumps inside the
        // same window never take focus back after the user switched with the keyboard.
        if std::mem::replace(&mut self.last_hover, h.0 as isize) == h.0 as isize {
            return;
        }
        let wid = id(h);
        if !self.model.is_tiled(wid) || !self.model.is_visible(wid) {
            return;
        }
        let fg = GetForegroundWindow();
        if fg == h {
            return;
        }
        // Never steal focus from floating windows, dialogs, menus, the Start menu, the
        // shell's own panels or a window being moved/sized.
        let from_ok = fg.is_invalid()
            || ((self.model.is_tiled(id(fg))
                || fg == GetShellWindow()
                || class_is(fg, &["Progman", "WorkerW"]))
                && !in_modal_ui(fg));
        if from_ok {
            self.model.on_foreground(wid, &mut self.out);
            focus(h);
        }
    }

    /// Native move/size loop ended. Only a change of size between start and end is a
    /// resize; a title-bar move keeps the tile size (even when the window is larger than
    /// its tile). Maximized windows are skipped.
    unsafe fn native_move_end(&mut self, h: HWND, start: Option<MoveSize>) {
        let wid = id(h);
        let Some(start) = start.filter(|s| !s.zoomed) else {
            return;
        };
        if IsZoomed(h).as_bool() {
            return;
        }
        let (Some(layout), Some((_, vr))) = (self.model.tiled_rect(wid), frame(h)) else {
            return;
        };
        let (s, e) = (start.start, to_rect(vr));
        let (dl, dt) = (e.x - s.x, e.y - s.y);
        let resized = e.width != s.width || e.height != s.height;
        // Apply the user's edge movements to the tile rect.
        let visual = if resized {
            let dr = e.x + e.width - (s.x + s.width);
            let db = e.y + e.height - (s.y + s.height);
            Rect {
                x: layout.x + dl,
                y: layout.y + dt,
                width: layout.width + dr - dl,
                height: layout.height + db - dt,
            }
        } else {
            Rect {
                x: layout.x + dl,
                y: layout.y + dt,
                ..layout
            }
        };
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        self.model
            .user_moved(wid, visual, (cursor.x, cursor.y), &mut self.out);
    }

    /// A placed tiled window that came out larger than its tile (minimum track size)
    /// gets that size recorded and the layout redone around it; at most 2x/s per window.
    unsafe fn check_min_size(&mut self, h: HWND) {
        let (key, wid) = (h.0 as isize, id(h));
        if self.drag.is_some_and(|d| d.hwnd == key)
            || self.movesize.is_some_and(|m| m.hwnd == key)
            || !self.model.is_tiled(wid)
            || !self.model.is_visible(wid)
        {
            return;
        }
        let (Some(placed), Some((_, vr))) = (self.placed.get(&key).copied(), frame(h)) else {
            return;
        };
        let v = to_rect(vr);
        // A window refusing to shrink keeps the requested origin and grows right/down;
        // anything else is a stale event from before the placement.
        if (v.x - placed.x).abs() > 2 || (v.y - placed.y).abs() > 2 {
            return;
        }
        let width = if v.width > placed.width + 2 {
            v.width
        } else {
            0
        };
        let height = if v.height > placed.height + 2 {
            v.height
        } else {
            0
        };
        if width == 0 && height == 0 {
            return;
        }
        let now = Instant::now();
        if self
            .refit
            .get(&key)
            .is_some_and(|t| now.duration_since(*t) < REFIT_INTERVAL)
        {
            return;
        }
        if self.refit.len() >= MAX_WINDOWS {
            self.refit
                .retain(|_, t| now.duration_since(*t) < REFIT_INTERVAL);
        }
        self.refit.insert(key, now);
        self.model.set_min_size(wid, width, height, &mut self.out);
    }

    unsafe fn drag_begin(&mut self, key: isize, start: (i32, i32), kind: DragKind) {
        let h = hwnd(key);
        let Some((wr, _)) = frame(h) else {
            return;
        };
        let wid = id(h);
        let third = |p: i32, lo: i32, len: i32| {
            if p < lo + len / 3 {
                -1
            } else if p >= lo + len - len / 3 {
                1
            } else {
                0
            }
        };
        let mut edges = (
            third(start.0, wr.left, wr.right - wr.left),
            third(start.1, wr.top, wr.bottom - wr.top),
        );
        if edges == (0, 0) {
            edges = (1, 1);
        }
        self.drag = Some(Drag {
            hwnd: key,
            kind,
            start,
            last: start,
            rect: wr,
            edges,
            tiled: self.model.is_tiled(wid) && self.model.is_visible(wid),
        });
        if kind == DragKind::Move {
            self.suppress(key);
            let _ = SetWindowPos(
                h,
                Some(HWND_TOP),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS,
            );
        }
    }

    unsafe fn drag_update(&mut self, pt: (i32, i32)) {
        let Some(mut d) = self.drag else {
            return;
        };
        let h = hwnd(d.hwnd);
        let (dx, dy) = (pt.0 - d.start.0, pt.1 - d.start.1);
        let r = d.rect;
        let flags = SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS;
        match d.kind {
            DragKind::Move => {
                self.suppress(d.hwnd);
                let _ = SetWindowPos(h, None, r.left + dx, r.top + dy, 0, 0, flags | SWP_NOSIZE);
            }
            DragKind::Resize if d.tiled => {
                let (sx, sy) = (pt.0 - d.last.0, pt.1 - d.last.1);
                let wid = id(h);
                if d.edges.0 != 0 && sx != 0 {
                    let dir = if d.edges.0 > 0 { Dir::Right } else { Dir::Left };
                    self.model
                        .resize_px(wid, dir, sx * d.edges.0, &mut self.out);
                }
                if d.edges.1 != 0 && sy != 0 {
                    let dir = if d.edges.1 > 0 { Dir::Down } else { Dir::Up };
                    self.model
                        .resize_px(wid, dir, sy * d.edges.1, &mut self.out);
                }
            }
            DragKind::Resize => {
                let (mut left, mut top, mut right, mut bottom) = (r.left, r.top, r.right, r.bottom);
                match d.edges.0 {
                    -1 => left = (left + dx).min(right - MIN_FLOAT_SIZE),
                    1 => right = (right + dx).max(left + MIN_FLOAT_SIZE),
                    _ => {}
                }
                match d.edges.1 {
                    -1 => top = (top + dy).min(bottom - MIN_FLOAT_SIZE),
                    1 => bottom = (bottom + dy).max(top + MIN_FLOAT_SIZE),
                    _ => {}
                }
                self.suppress(d.hwnd);
                let _ = SetWindowPos(h, None, left, top, right - left, bottom - top, flags);
            }
        }
        d.last = pt;
        self.drag = Some(d);
    }

    unsafe fn drag_end(&mut self, pt: (i32, i32)) {
        self.drag_update(pt);
        let Some(d) = self.drag.take() else {
            return;
        };
        let h = hwnd(d.hwnd);
        let wid = id(h);
        if !d.tiled {
            return;
        }
        match d.kind {
            DragKind::Move => {
                // Size is unchanged, so the model treats this as a move: swap with the tile
                // under the pointer (or move to that monitor) and snap back into the grid.
                if let Some(layout) = self.model.tiled_rect(wid) {
                    let moved = Rect {
                        x: layout.x + pt.0 - d.start.0,
                        y: layout.y + pt.1 - d.start.1,
                        ..layout
                    };
                    self.model.user_moved(wid, moved, pt, &mut self.out);
                }
            }
            DragKind::Resize => {
                if let Some(mon) = self.model.window(wid).map(|w| w.monitor) {
                    self.model.retile_monitor(mon, &mut self.out);
                }
            }
        }
    }

    /// Persist the journal (write-ahead, before effects run), then execute effects.
    /// Executing an effect may produce more (a vanished window is unmanaged and its
    /// workspace re-laid out), so run a bounded number of rounds.
    unsafe fn flush(&mut self) {
        for _ in 0..FLUSH_ROUNDS {
            if self.model.journal_mut().take_dirty() {
                self.sync_slots();
                self.persist();
            }
            if self.out.is_empty() {
                break;
            }
            let mut effects = std::mem::take(&mut self.out);
            for e in effects.drain(..) {
                self.execute(e);
            }
            if self.out.is_empty() {
                self.out = effects;
            }
        }
        if std::mem::take(&mut self.bar_dirty) {
            self.publish();
        }
    }

    fn sync_slots(&self) {
        let journal = self.model.journal();
        for slot in &HIDDEN {
            let v = slot.load(Ordering::Acquire);
            if v != 0 && !journal.contains(v as WinId) {
                slot.store(0, Ordering::Release);
            }
        }
        for e in journal.entries() {
            let v = e.hwnd as isize;
            if HIDDEN.iter().any(|s| s.load(Ordering::Acquire) == v) {
                continue;
            }
            if let Some(free) = HIDDEN.iter().find(|s| s.load(Ordering::Acquire) == 0) {
                free.store(v, Ordering::Release);
            }
        }
    }

    fn persist(&self) {
        let Ok(path) = journal_path() else {
            return;
        };
        let journal = self.model.journal();
        if journal.is_empty() {
            let _ = fs::remove_file(&path);
            return;
        }
        let Ok(bytes) = journal.to_bytes() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let temp = path.with_extension("pending.json");
        if fs::write(&temp, &bytes).is_ok() {
            let _ = fs::rename(&temp, &path);
        }
    }

    fn publish(&self) {
        let snapshot: Vec<Vec<BarWorkspace>> = (0..self.model.monitors().len())
            .map(|m| self.model.bar(m))
            .collect();
        *BAR.lock().unwrap_or_else(|e| e.into_inner()) = snapshot;
        let notify = NOTIFY.load(Ordering::Acquire);
        if notify != 0 {
            unsafe {
                let _ = PostMessageW(Some(hwnd(notify)), WM_STATE_CHANGED, WPARAM(0), LPARAM(0));
            }
        }
    }

    unsafe fn execute(&mut self, e: Effect) {
        match e {
            Effect::Place(w, r) => self.place(hwnd(w as isize), r),
            Effect::Hide(w) => {
                self.suppress(w as isize);
                let h = hwnd(w as isize);
                // A hidden window must not keep keyboard focus (e.g. when switching to an
                // empty workspace): SW_MINIMIZE also activates the next window.
                let cmd = if GetForegroundWindow() == h {
                    SW_MINIMIZE
                } else {
                    SW_SHOWMINNOACTIVE
                };
                let _ = ShowWindowAsync(h, cmd);
            }
            Effect::Show(w) => {
                self.suppress(w as isize);
                let _ = ShowWindowAsync(hwnd(w as isize), SW_SHOWNOACTIVATE);
            }
            Effect::Focus(w) => focus(hwnd(w as isize)),
            Effect::Close(w) => {
                let _ = PostMessageW(Some(hwnd(w as isize)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            Effect::ToggleMaximize(w) => toggle_maximize(hwnd(w as isize)),
            Effect::Minimize(w) => {
                let _ = ShowWindowAsync(hwnd(w as isize), SW_MINIMIZE);
            }
            Effect::Restore(w) => {
                let h = hwnd(w as isize);
                let _ = ShowWindowAsync(h, SW_RESTORE);
                focus(h);
            }
            Effect::BarChanged => self.bar_dirty = true,
        }
    }

    /// Position a window so its VISUAL bounds (DWM extended frame) equal `target`.
    unsafe fn place(&mut self, h: HWND, target: Rect) {
        let key = h.0 as isize;
        if !IsWindow(Some(h)).as_bool() {
            // Destroyed without a DESTROY event reaching us.
            self.model.unmanage(id(h), &mut self.out);
            self.forget(key);
            return;
        }
        if self.placed.len() >= MAX_WINDOWS && !self.placed.contains_key(&key) {
            let model = &self.model;
            self.placed
                .retain(|k, _| model.window(id(hwnd(*k))).is_some());
        }
        self.placed.insert(key, target);
        if IsIconic(h).as_bool() {
            // Being restored asynchronously: place now with known insets (the request is
            // queued behind the restore on the window's thread) and again once restored.
            if let Some(i) = self.insets.get(&key).copied() {
                self.set_pos(h, target, i);
            }
            if self.pending.len() < MAX_WINDOWS {
                self.pending.insert(key, target);
            }
            return;
        }
        let Some((wr, vr)) = frame(h) else {
            return;
        };
        let measured = Insets {
            l: vr.left - wr.left,
            t: vr.top - wr.top,
            r: wr.right - vr.right,
            b: wr.bottom - vr.bottom,
        };
        let insets = if [measured.l, measured.t, measured.r, measured.b]
            .iter()
            .all(|v| (0..=64).contains(v))
        {
            measured
        } else {
            Insets::default()
        };
        self.insets.insert(key, insets);
        if to_rect(vr) == target {
            return;
        }
        self.set_pos(h, target, insets);
    }

    unsafe fn set_pos(&mut self, h: HWND, r: Rect, i: Insets) {
        self.suppress(h.0 as isize);
        let _ = SetWindowPos(
            h,
            None,
            r.x - i.l,
            r.y - i.t,
            r.width + i.l + i.r,
            r.height + i.t + i.b,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_ASYNCWINDOWPOS,
        );
    }

    /// Stop observing and restore every window the manager hid.
    unsafe fn shutdown(&mut self) {
        for hook in self.hooks.drain(..) {
            let _ = UnhookWinEvent(hook);
        }
        let _ = KillTimer(Some(self.window), TIMER_HOVER);
        HOVER_ARMED.store(false, Ordering::SeqCst);
        for e in self.model.journal_mut().drain() {
            let h = hwnd(e.hwnd as isize);
            if IsWindow(Some(h)).as_bool() {
                let _ = ShowWindowAsync(h, SW_SHOWNOACTIVATE);
            }
        }
        self.model.journal_mut().take_dirty();
        self.sync_slots();
        self.persist();
        BAR.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if !self.hooks.is_empty() || !self.model.journal().is_empty() {
            unsafe { self.shutdown() };
        }
    }
}
