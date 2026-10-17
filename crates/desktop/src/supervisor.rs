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
    loop {
        let status = std::process::Command::new(&exe)
            .arg("--shell-child")
            .status()?;
        // user quit, installer/uninstaller request, or Windows signing out / shutting down
        if status.success()
            || stop_requested(stop)
            || unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) } != 0
        {
            return Ok(());
        }
        if status.code() == Some(FATAL_EXIT) {
            crate::taskbar::restore_after_crash();
            return Err(std::io::Error::other(
                "Monarch Next could not start. Details are in session.log.",
            ));
        }
        let now = Instant::now();
        crashes.retain(|t| now.duration_since(*t) < WINDOW);
        crashes.push(now);
        log(&format!(
            "shell exited unexpectedly ({status}); crashes in the last 2 min: {}",
            crashes.len()
        ));
        if crashes.len() >= MAX_CRASHES {
            // give the desktop back rather than crash-looping
            crate::taskbar::restore_after_crash();
            return Err(std::io::Error::other(
                "Monarch Next stopped after repeated crashes. Details are in session.log.",
            ));
        }
        std::thread::sleep(Duration::from_millis(500 * crashes.len() as u64));
    }
}
