//! Display-synced animation frames. `SetTimer` cannot beat ~15.6 ms and drifts against the screen,
//! which shows as judder on any display and wastes most of a high-refresh one. While at least one
//! animation runs, a small thread waits for each DWM composition and posts `WM_PACED` to the
//! windows that asked; with none running it sleeps on a condvar and costs nothing.
//!
//! A slot gets one message at a time: when painting takes longer than a refresh, frames are skipped
//! instead of queueing up, so an animation (which reads the clock) stays on time.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

/// `wparam`: the id given to [`start`].
pub const WM_PACED: u32 = WM_APP + 0x7A0;

struct Slot {
    hwnd: isize,
    id: usize,
    pending: Arc<AtomicBool>,
}

static SLOTS: Mutex<Vec<Slot>> = Mutex::new(Vec::new());
static WAKE: Condvar = Condvar::new();
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Send `h` a `WM_PACED(id)` every display refresh until [`stop`].
pub fn start(h: HWND, id: usize) {
    let key = h.0 as isize;
    let mut slots = SLOTS.lock().unwrap_or_else(|e| e.into_inner());
    if !slots.iter().any(|s| s.hwnd == key && s.id == id) {
        slots.push(Slot {
            hwnd: key,
            id,
            pending: Arc::new(AtomicBool::new(false)),
        });
    }
    drop(slots);
    if !RUNNING.swap(true, Ordering::SeqCst) {
        let spawned = std::thread::Builder::new()
            .name("frame-pacer".into())
            .spawn(run);
        if spawned.is_err() {
            RUNNING.store(false, Ordering::SeqCst);
        }
    }
    WAKE.notify_all();
}

pub fn stop(h: HWND, id: usize) {
    let key = h.0 as isize;
    SLOTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|s| !(s.hwnd == key && s.id == id));
}

/// The window took a `WM_PACED`: the next one may be sent. False when the slot was stopped after
/// the message was posted (the frame must be ignored).
pub fn ack(h: HWND, id: usize) -> bool {
    let key = h.0 as isize;
    let slots = SLOTS.lock().unwrap_or_else(|e| e.into_inner());
    match slots.iter().find(|s| s.hwnd == key && s.id == id) {
        Some(s) => {
            s.pending.store(false, Ordering::SeqCst);
            true
        }
        None => false,
    }
}

fn run() {
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
    loop {
        {
            let mut slots = SLOTS.lock().unwrap_or_else(|e| e.into_inner());
            while slots.is_empty() {
                slots = WAKE.wait(slots).unwrap_or_else(|e| e.into_inner());
            }
            // a window that is gone cannot take messages: drop its slot
            slots.retain(|s| {
                if s.pending.swap(true, Ordering::SeqCst) {
                    return true;
                }
                unsafe {
                    PostMessageW(
                        Some(HWND(s.hwnd as *mut _)),
                        WM_PACED,
                        WPARAM(s.id),
                        LPARAM(0),
                    )
                }
                .is_ok()
            });
        }
        let t0 = Instant::now();
        let flushed = unsafe { DwmFlush() }.is_ok();
        // no composition to wait for (locked, remote session): never spin
        if !flushed || t0.elapsed() < Duration::from_millis(1) {
            std::thread::sleep(Duration::from_millis(16));
        }
    }
}
