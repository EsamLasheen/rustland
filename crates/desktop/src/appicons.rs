//! Window / application icons as RGBA (top bar active window, workspace apps). Cached per exe, bounded.
use crate::topbar::Image;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{mpsc, Mutex, OnceLock},
};
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
    hicon_pixels(icon, owned).map(|(px, w, h)| Image {
        rgba: Rc::new(px),
        w,
        h,
    })
}
/// HICON → (straight RGBA pixels, width, height); plain data, so it can cross threads.
unsafe fn hicon_pixels(icon: HICON, owned: bool) -> Option<(Vec<u8>, u32, u32)> {
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
    Some((px, w as u32, h as u32))
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

/// Explorer's large icon for a file, shortcut, or shell namespace item (`shell:AppsFolder\<AUMID>`,
/// i.e. Store apps), as plain pixels. Shell items cost 100–300 ms; files are usually much cheaper.
unsafe fn path_pixels(path: &str) -> Option<(Vec<u8>, u32, u32)> {
    use windows::Win32::{Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES, UI::Shell::*};
    let w = crate::platform::wide(path);
    let mut info = SHFILEINFOW::default();
    // Shell namespace items are not files: resolve them to an item ID list and ask for that
    // item's icon instead.
    let mut pidl: *mut Common::ITEMIDLIST = std::ptr::null_mut();
    let (name, flags) = if path.starts_with("shell:") {
        let _ = SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None);
        (PCWSTR(pidl as *const u16), SHGFI_PIDL)
    } else {
        (PCWSTR(w.as_ptr()), SHGFI_FLAGS(0))
    };
    let ok = if name.is_null() {
        0 // unresolvable shell item: no icon (callers draw their glyph)
    } else {
        SHGetFileInfoW(
            name,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            Some(&mut info),
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_LARGEICON | flags,
        )
    };
    if !pidl.is_null() {
        windows::Win32::System::Com::CoTaskMemFree(Some(pidl as *const _));
    }
    if ok != 0 && !info.hIcon.is_invalid() {
        hicon_pixels(info.hIcon, true)
    } else {
        None
    }
}
fn remember(key: String, img: Option<Image>) {
    PATHS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 256 {
            c.clear();
        }
        c.insert(key, img);
    });
}

/// Icon of a file or shortcut as Explorer shows it (resolves .lnk targets and overlays), cached.
pub fn for_path(path: &str) -> Option<Image> {
    let key = format!("path:{}", path.to_lowercase());
    if let Some(hit) = PATHS.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let img = unsafe { path_pixels(path) }.map(|(px, w, h)| Image {
        rgba: Rc::new(px),
        w,
        h,
    });
    remember(key, img.clone());
    img
}

type Pixels = Option<(Vec<u8>, u32, u32)>;
/// Shell-item icon requests handed to the worker, keyed like `PATHS`.
#[derive(Default)]
struct Deferred {
    /// Requested and not finished yet (so repaints do not queue them again).
    pending: HashSet<String>,
    /// Extracted by the worker, waiting to be picked up by the UI thread.
    done: HashMap<String, Pixels>,
}
static DEFERRED: Mutex<Option<Deferred>> = Mutex::new(None);
/// A request to the icon worker: (path, cache key, window to notify, message to post).
type IconRequest = (String, String, isize, u32);
static DEFERRED_TX: OnceLock<Mutex<mpsc::Sender<IconRequest>>> = OnceLock::new();

/// Like `for_path`, but shell namespace items (Store apps, slow to resolve) are extracted on a
/// background thread so the caller never stalls: `None` until ready, then `notify` receives `msg`
/// and the next call returns the icon. Files and shortcuts are resolved inline as in `for_path`.
pub fn for_path_deferred(path: &str, notify: HWND, msg: u32) -> Option<Image> {
    if !path.starts_with("shell:") {
        return for_path(path);
    }
    let key = format!("path:{}", path.to_lowercase());
    if let Some(hit) = PATHS.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let mut d = DEFERRED.lock().ok()?;
    let d = d.get_or_insert_with(Deferred::default);
    match d.done.remove(&key) {
        Some(Some((px, w, h))) => {
            let img = Some(Image {
                rgba: Rc::new(px),
                w,
                h,
            });
            remember(key, img.clone());
            img
        }
        Some(None) => {
            remember(key, None);
            None
        }
        None if d.pending.contains(&key) => None,
        None => {
            if d.pending.len() > 256 {
                d.pending.clear(); // bounded; a cleared request is at worst queued twice
            }
            d.pending.insert(key.clone());
            let tx = DEFERRED_TX.get_or_init(|| {
                let (tx, rx) = mpsc::channel::<IconRequest>();
                let _ = std::thread::Builder::new()
                    .name("launcher-icons".into())
                    .spawn(move || unsafe {
                        let _ = windows::Win32::System::Com::CoInitializeEx(
                            None,
                            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
                        );
                        for (path, key, h, msg) in rx {
                            let px = path_pixels(&path);
                            if let Ok(mut g) = DEFERRED.lock() {
                                let d = g.get_or_insert_with(Deferred::default);
                                d.pending.remove(&key);
                                if d.done.len() > 64 {
                                    d.done.clear(); // never picked up (launcher closed)
                                }
                                d.done.insert(key, px);
                            }
                            let _ =
                                PostMessageW(Some(HWND(h as *mut _)), msg, WPARAM(0), LPARAM(0));
                        }
                    });
                Mutex::new(tx)
            });
            if let Ok(tx) = tx.lock() {
                let _ = tx.send((path.to_string(), key, notify.0 as isize, msg));
            }
            None
        }
    }
}
