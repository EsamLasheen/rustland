//! Watches the Obsidian focus-timer file and posts its state to the UI thread. Uses a directory change
//! notification; cloud-synced folders (Google Drive) may not raise one, so the wait also times out every
//! 5 s and compares the modification time (one metadata call — the legacy polled every 2 s).
use monarch_core::focus::FocusState;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::*, Storage::FileSystem::*, System::Threading::WaitForSingleObject,
        UI::WindowsAndMessaging::PostMessageW,
    },
};

pub const WM_FOCUS_STATE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 49;

/// The configured focus-timer file, when one is set and its folder exists.
pub fn resolve(configured: &str) -> Option<PathBuf> {
    if configured.trim().is_empty() {
        return None;
    }
    let p = PathBuf::from(configured.trim());
    p.parent().is_some_and(|d| d.exists()).then_some(p)
}

pub fn start(notify: HWND, file: PathBuf) {
    let raw = notify.0 as isize;
    let _ = std::thread::Builder::new()
        .name("focus-watch".into())
        .spawn(move || {
            let Some(dir) = file.parent().map(|d| d.to_path_buf()) else {
                return;
            };
            let wdir = crate::platform::wide(&dir);
            let open = || {
                unsafe {
                    FindFirstChangeNotificationW(
                        PCWSTR(wdir.as_ptr()),
                        false,
                        FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME,
                    )
                }
                .ok()
                .filter(|h| !h.is_invalid())
                .map(ChangeHandle)
            };
            // Dropped (closed) on every exit path, including the early return below.
            let mut change = open();
            let mut next_reopen = Instant::now() + REOPEN_EVERY;
            let mut last: Option<SystemTime> = None;
            let mut first = true;
            loop {
                let m = std::fs::metadata(&file).and_then(|m| m.modified()).ok();
                if first || m != last {
                    first = false;
                    last = m;
                    let state = std::fs::read(&file)
                        .ok()
                        .and_then(|b| FocusState::parse(&b));
                    let b = Box::into_raw(Box::new(state)) as isize;
                    unsafe {
                        if PostMessageW(
                            Some(HWND(raw as *mut _)),
                            WM_FOCUS_STATE,
                            WPARAM(0),
                            LPARAM(b),
                        )
                        .is_err()
                        {
                            drop(Box::from_raw(b as *mut Option<FocusState>));
                            return; // shell gone
                        }
                    }
                }
                let healthy = match &change {
                    Some(h) => unsafe {
                        let w = WaitForSingleObject(h.0, 5000);
                        if w == WAIT_OBJECT_0 {
                            // Coalesce bursts of writes; also bounds the loop rate if the handle
                            // were to stay signalled.
                            std::thread::sleep(MIN_INTERVAL);
                        }
                        (w == WAIT_OBJECT_0 || w == WAIT_TIMEOUT)
                            && FindNextChangeNotification(h.0).is_ok()
                    },
                    None => {
                        std::thread::sleep(POLL);
                        if Instant::now() >= next_reopen {
                            next_reopen = Instant::now() + REOPEN_EVERY;
                            change = open();
                        }
                        true
                    }
                };
                if !healthy {
                    // Folder vanished (e.g. Google Drive unmounted): close the handle, fall back
                    // to the 5 s metadata poll and retry the notification periodically.
                    change = None;
                    next_reopen = Instant::now() + REOPEN_EVERY;
                    std::thread::sleep(POLL);
                }
            }
        });
}

const POLL: Duration = Duration::from_secs(5);
const REOPEN_EVERY: Duration = Duration::from_secs(30);
const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Owns a change-notification handle; closes it on drop.
struct ChangeHandle(HANDLE);
impl Drop for ChangeHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = FindCloseChangeNotification(self.0);
        }
    }
}

pub fn take(l: LPARAM) -> Option<FocusState> {
    unsafe { *Box::from_raw(l.0 as *mut Option<FocusState>) }
}
