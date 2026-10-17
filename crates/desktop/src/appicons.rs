//! Window / application icons as RGBA (top bar active window, workspace apps). Cached per exe, bounded.
use crate::topbar::Image;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::Threading::*,
        UI::{Shell::SHDefExtractIconW, WindowsAndMessaging::*},
    },
};

thread_local! {
    /// Window icons by executable (bounded).
    static CACHE: RefCell<HashMap<String, Option<Image>>> = RefCell::new(HashMap::new());
    /// Launcher shortcut icons, bounded separately so the two caches never evict each other.
    static PATHS: RefCell<HashMap<String, Option<Image>>> = RefCell::new(HashMap::new());
}

/// Full image path of the process owning a window.
pub fn exe_of(h: HWND) -> Option<String> {
    unsafe {
        let mut pid = 0;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let p = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            p,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut n,
        )
        .is_ok();
        let _ = CloseHandle(p);
        ok.then(|| String::from_utf16_lossy(&buf[..n as usize]))
    }
}

/// HICON → straight RGBA. `owned`: we extracted it and must destroy it.
unsafe fn hicon_rgba(icon: HICON, owned: bool) -> Option<Image> {
    let mut ii = ICONINFO::default();
    if GetIconInfo(icon, &mut ii).is_err() {
        if owned {
            let _ = DestroyIcon(icon);
        }
        return None;
    }
    let mut bm = BITMAP::default();
    GetObjectW(
        ii.hbmColor.into(),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut _),
    );
    let (w, h) = (bm.bmWidth.max(1), bm.bmHeight.max(1));
    let dc = CreateCompatibleDC(None);
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u8; (w * h * 4) as usize];
    GetDIBits(
        dc,
        ii.hbmColor,
        0,
        h as u32,
        Some(px.as_mut_ptr() as *mut _),
        &mut info,
        DIB_RGB_COLORS,
    );
    if px.as_chunks::<4>().0.iter().all(|p| p[3] == 0) {
        // old-style icon without alpha: take it from the AND mask
        let mut mask = vec![0u8; (w * h * 4) as usize];
        GetDIBits(
            dc,
            ii.hbmMask,
            0,
            h as u32,
            Some(mask.as_mut_ptr() as *mut _),
            &mut info,
            DIB_RGB_COLORS,
        );
        for (p, m) in px
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(mask.as_chunks::<4>().0)
        {
            p[3] = if m[0] == 0 { 255 } else { 0 };
        }
    }
    let _ = DeleteDC(dc);
    let _ = DeleteObject(ii.hbmColor.into());
    let _ = DeleteObject(ii.hbmMask.into());
    if owned {
        let _ = DestroyIcon(icon);
    }
    for p in px.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    } // BGRA -> RGBA
    Some(Image {
        rgba: Rc::new(px),
        w: w as u32,
        h: h as u32,
    })
}

unsafe fn from_window(h: HWND) -> Option<Image> {
    let mut r = 0usize;
    for kind in [
        1usize, /* ICON_BIG */
        2,      /* ICON_SMALL2 */
        0,      /* ICON_SMALL */
    ] {
        if r == 0 {
            let _ = SendMessageTimeoutW(
                h,
                WM_GETICON,
                WPARAM(kind),
                LPARAM(0),
                SMTO_ABORTIFHUNG | SMTO_BLOCK,
                100,
                Some(&mut r),
            );
        }
    }
    if r == 0 {
        r = GetClassLongPtrW(h, GCLP_HICON);
    }
    if r == 0 {
        r = GetClassLongPtrW(h, GCLP_HICONSM);
    }
    (r != 0)
        .then(|| hicon_rgba(HICON(r as *mut _), false))
        .flatten()
}

unsafe fn from_file(path: &str) -> Option<Image> {
    let w = crate::platform::wide(path);
    let mut icon = HICON::default();
    SHDefExtractIconW(PCWSTR(w.as_ptr()), 0, 0, Some(&mut icon), None, 32)
        .ok()
        .ok()?;
    if icon.is_invalid() {
        return None;
    }
    hicon_rgba(icon, true)
}

/// Icon for a window: the exe's own icon (crisp, cached), else the window's icon
/// (Store apps whose files are not readable, `.bin` programs with only a generic file icon).
pub fn for_window(h: HWND) -> Option<Image> {
    let exe = exe_of(h).unwrap_or_default();
    let key = exe.to_lowercase();
    // Store apps are all hosted by ApplicationFrameHost and protected processes hide their path:
    // their icon belongs to the window, so it is never shared through the exe-keyed cache
    if key.is_empty() || key.ends_with("applicationframehost.exe") {
        return unsafe { from_window(h) };
    }
    if let Some(hit) = CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let img = unsafe {
        if key.ends_with(".exe") {
            from_file(&exe).or_else(|| from_window(h))
        } else {
            from_window(h).or_else(|| from_file(&exe))
        }
    };
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 128 {
            c.clear();
        } // bounded
        c.insert(key, img.clone());
    });
    img
}

/// Icon of a file or shortcut as Explorer shows it (resolves .lnk targets and overlays), cached.
pub fn for_path(path: &str) -> Option<Image> {
    use windows::Win32::{Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES, UI::Shell::*};
    let key = format!("path:{}", path.to_lowercase());
    if let Some(hit) = PATHS.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let img = unsafe {
        let w = crate::platform::wide(path);
        let mut info = SHFILEINFOW::default();
        let ok = SHGetFileInfoW(
            PCWSTR(w.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON,
        );
        if ok != 0 && !info.hIcon.is_invalid() {
            hicon_rgba(info.hIcon, true)
        } else {
            None
        }
    };
    PATHS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 256 {
            c.clear();
        }
        c.insert(key, img.clone());
    });
    img
}
