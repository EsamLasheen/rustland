//! Crash recovery without services or scheduled tasks: `--shell` is a tiny supervisor that runs the
//! real shell (`--shell-child`) and restarts it if it crashes. A clean exit (user quit, update,
//! uninstall) ends both. It waits on the child handle, so it costs no CPU and ~1 MB.
use std::time::{Duration, Instant};
use windows::Win32::{
    Foundation::*,
    System::Threading::*,
    UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN},
};

const MAX_CRASHES: usize = 5;
const WINDOW: Duration = Duration::from_secs(120);
/// Exit code of a child that could not start at all (bad install, missing data folder): not retried.
pub const FATAL_EXIT: i32 = 3;
/// Exit code of a child that left the desktop to plain Windows on request: wait for the user.
pub const PAUSE_EXIT: i32 = 4;

fn stop_event() -> Option<HANDLE> {
    crate::platform::shell_stop_event()
}

fn stop_requested(e: Option<HANDLE>) -> bool {
    e.is_some_and(|e| unsafe { WaitForSingleObject(e, 0) } == WAIT_OBJECT_0)
}

fn log(m: &str) {
    if let Ok(p) = crate::platform::paths() {
        crate::platform::log(&p, m);
    }
}

pub fn run() -> std::io::Result<()> {
    let Some(_instance) = crate::platform::Instance::named(r"Local\MonarchNext.Shell")? else {
        return Ok(()); // already running
    };
    let exe = std::env::current_exe()?;
    let stop = stop_event();
    if let Some(e) = stop {
        unsafe {
            let _ = ResetEvent(e);
        }
    }
    let mut crashes: Vec<Instant> = vec![];
    // slower crash loops (one every 30 s, say) are caught by an hourly budget
    let mut hourly: Vec<Instant> = vec![];
    loop {
        let status = std::process::Command::new(&exe)
            .arg("--shell-child")
            .status()?;
        if status.code() == Some(PAUSE_EXIT) && !stop_requested(stop) {
            log("paused: plain Windows");
            if !paused::wait(stop) {
                return Ok(());
            }
            log("resumed");
            crashes.clear();
            continue;
        }
        // installer/uninstaller/deploy request, or Windows signing out / shutting down
        if stop_requested(stop) || unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) } != 0 {
            return Ok(());
        }
        // A clean exit nobody asked for: another program closed the shell. Restart Manager does
        // this while an app updates (seen 2026-11-24 during a Claude update: RustLand was closed and
        // never came back), and so does "End task". The user leaves through pause, sign-out or
        // shutdown, all handled above, so the shell comes back.
        if status.success() {
            let now = Instant::now();
            hourly.retain(|t| now.duration_since(*t) < Duration::from_secs(3600));
            hourly.push(now);
            if hourly.len() >= 10 {
                return Ok(());
            }
            log("shell was closed by another program (not by the user); starting it again");
            std::thread::sleep(Duration::from_secs(2));
            if stop_requested(stop) || unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) } != 0 {
                return Ok(());
            }
            continue;
        }
        if status.code() == Some(FATAL_EXIT) {
            let _ = crate::wm::recover();
            crate::winglass::give_back_after_crash(
                crate::config::Store::open()
                    .map(|s| s.config.window_opacity)
                    .unwrap_or(crate::winglass::DEFAULT_OPACITY),
            );
            crate::taskbar::restore_after_crash();
            return Err(std::io::Error::other(
                "RustLand could not start. Details are in session.log.",
            ));
        }
        let now = Instant::now();
        crashes.retain(|t| now.duration_since(*t) < WINDOW);
        crashes.push(now);
        hourly.retain(|t| now.duration_since(*t) < Duration::from_secs(3600));
        hourly.push(now);
        log(&format!(
            "shell exited unexpectedly ({status}); crashes in the last 2 min: {}",
            crashes.len()
        ));
        if crashes.len() >= MAX_CRASHES || hourly.len() >= 10 {
            let _ = crate::wm::recover();
            // give the desktop back rather than crash-looping
            crate::winglass::give_back_after_crash(
                crate::config::Store::open()
                    .map(|s| s.config.window_opacity)
                    .unwrap_or(crate::winglass::DEFAULT_OPACITY),
            );
            crate::taskbar::restore_after_crash();
            return Err(std::io::Error::other(
                "RustLand stopped after repeated crashes. Details are in session.log.",
            ));
        }
        std::thread::sleep(Duration::from_millis(500 * crashes.len() as u64));
    }
}

/// Plain-Windows mode: the shell is gone, the supervisor keeps a tray icon and the Win+Alt+Home
/// hotkey. Costs one hidden window and a message loop.
mod paused {
    use windows::core::w;
    use windows::Win32::Foundation::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::INFINITE;
    use windows::Win32::UI::Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_NOREPEAT, MOD_WIN};
    use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
    use windows::Win32::UI::WindowsAndMessaging::*;

    const WM_TRAY: u32 = WM_APP + 1;
    const HOTKEY: i32 = 1;
    const MENU_RESUME: usize = 1;
    const MENU_QUIT: usize = 2;
    /// What the user chose (set from the window procedure).
    static CHOICE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
    const RESUME: u8 = 1;
    const QUIT: u8 = 2;

    unsafe fn add_icon(h: HWND) {
        let mut d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: h,
            uID: 1,
            uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            ..Default::default()
        };
        let tip: Vec<u16> = "RustLand متوقف · اضغط هنا أو Win+Alt+Home للتشغيل".encode_utf16().collect();
        let n = tip.len().min(d.szTip.len() - 1);
        d.szTip[..n].copy_from_slice(&tip[..n]);
        let _ = Shell_NotifyIconW(NIM_ADD, &d);
    }

    unsafe fn remove_icon(h: HWND) {
        let d = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: h,
            uID: 1,
            ..Default::default()
        };
        let _ = Shell_NotifyIconW(NIM_DELETE, &d);
    }

    unsafe fn menu(h: HWND) {
        let Ok(m) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(m, MF_STRING, MENU_RESUME, w!("تشغيل RustLand (Win+Alt+Home)"));
        let _ = AppendMenuW(m, MF_STRING, MENU_QUIT, w!("إنهاء RustLand لحد إعادة التشغيل"));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // required for the menu to close when clicking elsewhere
        let _ = SetForegroundWindow(h);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), h, None);
        let _ = DestroyMenu(m);
        match cmd.0 as usize {
            MENU_RESUME => CHOICE.store(RESUME, std::sync::atomic::Ordering::SeqCst),
            MENU_QUIT => CHOICE.store(QUIT, std::sync::atomic::Ordering::SeqCst),
            _ => {}
        }
    }

    unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        static TASKBAR_CREATED: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let created = *TASKBAR_CREATED.get_or_init(|| RegisterWindowMessageW(w!("TaskbarCreated")));
        match msg {
            WM_HOTKEY if wp.0 as i32 == HOTKEY => {
                CHOICE.store(RESUME, std::sync::atomic::Ordering::SeqCst);
                LRESULT(0)
            }
            WM_TRAY => {
                match lp.0 as u32 {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => CHOICE.store(RESUME, std::sync::atomic::Ordering::SeqCst),
                    WM_RBUTTONUP | WM_CONTEXTMENU => menu(h),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_QUERYENDSESSION => LRESULT(1),
            WM_ENDSESSION if wp.0 != 0 => {
                CHOICE.store(QUIT, std::sync::atomic::Ordering::SeqCst);
                LRESULT(0)
            }
            m if m == created && created != 0 => {
                add_icon(h); // Explorer restarted: the tray is new
                LRESULT(0)
            }
            _ => DefWindowProcW(h, msg, wp, lp),
        }
    }

    /// Wait for the user. True: start the shell again; false: end (quit, sign-out, stop request).
    pub fn wait(stop: Option<HANDLE>) -> bool {
        unsafe {
            CHOICE.store(0, std::sync::atomic::Ordering::SeqCst);
            let Ok(module) = GetModuleHandleW(None) else {
                return true;
            };
            let class = w!("RustLand.Paused");
            let _ = RegisterClassExW(&WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(proc),
                hInstance: module.into(),
                lpszClassName: class,
                ..Default::default()
            });
            let Ok(h) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("RustLand paused"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(module.into()),
                None,
            ) else {
                return true; // no way to come back later: do not strand the user
            };
            add_icon(h);
            let hotkey = RegisterHotKey(Some(h), HOTKEY, MOD_WIN | MOD_ALT | MOD_NOREPEAT, 0x24).is_ok();
            if !hotkey {
                super::log("paused: Win+Alt+Home is taken by another program; use the tray icon");
            }
            let handles: Vec<HANDLE> = stop.into_iter().collect();
            let result = loop {
                match CHOICE.load(std::sync::atomic::Ordering::SeqCst) {
                    RESUME => break true,
                    QUIT => break false,
                    _ => {}
                }
                let r = MsgWaitForMultipleObjects(Some(&handles), false, INFINITE, QS_ALLINPUT);
                if !handles.is_empty() && r.0 == 0 {
                    break false; // installer / uninstaller / deploy asked the shell to stop
                }
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    if msg.message == WM_QUIT {
                        CHOICE.store(QUIT, std::sync::atomic::Ordering::SeqCst);
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            };
            if hotkey {
                let _ = UnregisterHotKey(Some(h), HOTKEY);
            }
            remove_icon(h);
            let _ = DestroyWindow(h);
            let _ = UnregisterClassW(class, Some(module.into()));
            result
        }
    }
}
