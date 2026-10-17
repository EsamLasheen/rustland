//! ASUS laptops can lock the Windows key in the keyboard controller (Armoury Crate "Win Key", Fn+Win).
//! A locked key never reaches Windows, so every Win shortcut silently stops. Armoury Crate mirrors the
//! state in `HKCU\Software\ASUS\Armoury Crate Service\SystemFeature\WinKeyStatus` (1 = enabled); this
//! watches that value (registry change notification, no polling) and tells the shell when it changes.
use windows::{
    core::w,
    Win32::{
        Foundation::*,
        System::{Registry::*, Threading::*},
        UI::WindowsAndMessaging::{PostMessageW, WM_APP},
    },
};

pub const WM_WINKEY: u32 = WM_APP + 53;

unsafe fn read(key: HKEY) -> Option<u32> {
    let mut v = 0u32;
    let mut size = 4u32;
    RegQueryValueExW(
        key,
        w!("WinKeyStatus"),
        None,
        None,
        Some(&mut v as *mut u32 as *mut u8),
        Some(&mut size),
    )
    .ok()
    .ok()?;
    Some(v)
}

/// Posts `WM_WINKEY` with wParam = 1 (enabled) / 0 (locked) at start and on every change.
/// Does nothing on machines without Armoury Crate.
pub fn start(notify: HWND) {
    let raw = notify.0 as isize;
    let _ = std::thread::Builder::new()
        .name("winkey-watch".into())
        .spawn(move || unsafe {
            let mut key = HKEY::default();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                w!("Software\\ASUS\\Armoury Crate Service\\SystemFeature"),
                None,
                KEY_READ | KEY_NOTIFY,
                &mut key,
            )
            .is_err()
            {
                return;
            }
            let Ok(event) = CreateEventW(None, false, false, None) else {
                let _ = RegCloseKey(key);
                return;
            };
            let mut last = None;
            loop {
                let now = read(key);
                if now.is_some() && now != last {
                    last = now;
                    if PostMessageW(
                        Some(HWND(raw as *mut _)),
                        WM_WINKEY,
                        WPARAM(now.unwrap_or(1) as usize),
                        LPARAM(0),
                    )
                    .is_err()
                    {
                        break; // shell gone
                    }
                }
                if RegNotifyChangeKeyValue(
                    key,
                    false,
                    REG_NOTIFY_CHANGE_LAST_SET,
                    Some(event),
                    true,
                )
                .is_err()
                {
                    break;
                }
                let _ = WaitForSingleObject(event, INFINITE);
            }
            let _ = CloseHandle(event);
            let _ = RegCloseKey(key);
        });
}
