//! First-run / update questions shown by the installer: name, wallpaper folder, weather city,
//! optional Obsidian focus-timer file, rank, and which parts of the shell to enable. Answers are
//! written to the normal settings file before the shell starts, so it comes up configured.
use crate::config::{ShellConfig, Store, RANKS};
use std::cell::RefCell;
use windows::{
    core::{w, PCWSTR, PWSTR},
    Win32::{
        Foundation::*,
        Graphics::{Dwm::*, Gdi::*},
        System::{Com::*, LibraryLoader::GetModuleHandleW},
        UI::{
            Controls::*, HiDpi::*, Shell::Common::COMDLG_FILTERSPEC, Shell::*,
            WindowsAndMessaging::*,
        },
    },
};

/// What the user chose; `None` from [`ask`] means "cancel".
pub struct Choices {
    pub config: ShellConfig,
    pub autostart: bool,
}

const ID_NAME: i32 = 101;
const ID_WALL: i32 = 102;
const ID_WALL_BROWSE: i32 = 103;
const ID_CITY: i32 = 104;
const ID_FOCUS: i32 = 105;
const ID_FOCUS_BROWSE: i32 = 106;
const ID_RANK: i32 = 107;
const ID_TILE: i32 = 110;
const ID_TASKBAR: i32 = 111;
const ID_WALLPAPER: i32 = 112;
const ID_ACCENT: i32 = 113;
const ID_AUTOSTART: i32 = 114;

struct State {
    config: ShellConfig,
    result: Option<Choices>,
    font: HFONT,
    bold: HFONT,
}
thread_local! { static ST: RefCell<Option<State>> = const { RefCell::new(None) }; }

fn text_of(h: HWND, id: i32) -> String {
    unsafe {
        let Ok(c) = GetDlgItem(Some(h), id) else {
            return String::new();
        };
        let mut buf = vec![0u16; 1024];
        let n = GetWindowTextW(c, &mut buf) as usize;
        String::from_utf16_lossy(&buf[..n]).trim().to_string()
    }
}
fn set_text(h: HWND, id: i32, s: &str) {
    let w = crate::platform::wide(s);
    unsafe {
        if let Ok(c) = GetDlgItem(Some(h), id) {
            let _ = SetWindowTextW(c, PCWSTR(w.as_ptr()));
        }
    }
}
fn checked(h: HWND, id: i32) -> bool {
    unsafe { IsDlgButtonChecked(h, id) == BST_CHECKED.0 }
}

/// Folder or file picker (Windows' own common dialog).
fn pick(owner: HWND, folder: bool) -> Option<String> {
    unsafe {
        let dlg: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let mut opts = dlg.GetOptions().ok()? | FOS_FORCEFILESYSTEM;
        if folder {
            opts |= FOS_PICKFOLDERS;
        } else {
            let spec = [
                COMDLG_FILTERSPEC {
                    pszName: w!("Focus timer state (*.json)"),
                    pszSpec: w!("*.json"),
                },
                COMDLG_FILTERSPEC {
                    pszName: w!("All files"),
                    pszSpec: w!("*.*"),
                },
            ];
            let _ = dlg.SetFileTypes(&spec);
        }
        let _ = dlg.SetOptions(opts);
        dlg.Show(Some(owner)).ok()?;
        let item = dlg.GetResult().ok()?;
        let p: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn child(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: WINDOW_STYLE,
    ex: WINDOW_EX_STYLE,
    id: i32,
    r: (i32, i32, i32, i32),
    font: HFONT,
) -> HWND {
    let t = crate::platform::wide(text);
    let h = CreateWindowExW(
        ex,
        class,
        PCWSTR(t.as_ptr()),
        WS_CHILD | WS_VISIBLE | style,
        r.0,
        r.1,
        r.2,
        r.3,
        Some(parent),
        Some(HMENU(id as isize as *mut _)),
        Some(GetModuleHandleW(None).unwrap_or_default().into()),
        None,
    )
    .unwrap_or_default();
    SendMessageW(
        h,
        WM_SETFONT,
        Some(WPARAM(font.0 as usize)),
        Some(LPARAM(1)),
    );
    h
}

unsafe fn build(h: HWND) {
    let k = GetDpiForWindow(h).max(96) as f32 / 96.;
    let s = |v: i32| (v as f32 * k).round() as i32;
    let (font, bold, cfg) = ST.with(|st| {
        let st = st.borrow();
        let st = st.as_ref().unwrap();
        (st.font, st.bold, st.config.clone())
    });
    let (x, w) = (s(24), s(472));
    let mut y = s(18);
    let label = |y: i32, t: &str, f: HFONT| {
        child(
            h,
            w!("STATIC"),
            t,
            WINDOW_STYLE(0),
            WINDOW_EX_STYLE(0),
            -1,
            (x, y, w, s(20)),
            f,
        )
    };
    let edit = |y: i32, id: i32, t: &str, width: i32| {
        child(
            h,
            w!("EDIT"),
            t,
            WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            WS_EX_CLIENTEDGE,
            id,
            (x, y, width, s(26)),
            font,
        )
    };
    let button = |y: i32, id: i32, t: &str, bx: i32, bw: i32, style: u32| {
        child(
            h,
            w!("BUTTON"),
            t,
            WS_TABSTOP | WINDOW_STYLE(style),
            WINDOW_EX_STYLE(0),
            id,
            (bx, y, bw, s(26)),
            font,
        )
    };

    label(y, "Set up Monarch Next  ·  إعداد Monarch Next", bold);
    y += s(26);
    label(
        y,
        "You can change all of this later from the Settings panel.",
        font,
    );
    y += s(34);

    label(y, "Your name (dashboard greeting)  ·  اسمك", font);
    y += s(22);
    edit(y, ID_NAME, &cfg.user_name, w);
    y += s(38);

    label(
        y,
        "Wallpapers folder — sub-folders E, D, C, B, A, S are created inside  ·  فولدر الخلفيات",
        font,
    );
    y += s(22);
    edit(y, ID_WALL, &cfg.rank_folders, w - s(96));
    button(
        y,
        ID_WALL_BROWSE,
        "Browse…",
        x + w - s(88),
        s(88),
        BS_PUSHBUTTON as u32,
    );
    y += s(38);

    label(
        y,
        "City for weather (empty = detect automatically)  ·  المدينة للطقس",
        font,
    );
    y += s(22);
    edit(y, ID_CITY, &cfg.weather_city, w);
    y += s(38);

    label(
        y,
        "Obsidian focus-timer file (optional)  ·  ملف مؤقت التركيز",
        font,
    );
    y += s(22);
    edit(y, ID_FOCUS, &cfg.focus_file, w - s(96));
    button(
        y,
        ID_FOCUS_BROWSE,
        "Browse…",
        x + w - s(88),
        s(88),
        BS_PUSHBUTTON as u32,
    );
    y += s(38);

    label(y, "Hunter rank (accent colour)  ·  الرتبة", font);
    let combo = child(
        h,
        w!("COMBOBOX"),
        "",
        WS_TABSTOP | WS_VSCROLL | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
        WINDOW_EX_STYLE(0),
        ID_RANK,
        (x + w - s(120), y - s(4), s(120), s(200)),
        font,
    );
    for (i, (r, _)) in RANKS.iter().enumerate() {
        let t = crate::platform::wide(format!("Rank {r}"));
        SendMessageW(combo, CB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
        if *r == cfg.rank {
            SendMessageW(combo, CB_SETCURSEL, Some(WPARAM(i)), None);
        }
    }
    y += s(40);

    let check = |y: i32, id: i32, t: &str, on: bool| {
        let c = button(y, id, t, x, w, BS_AUTOCHECKBOX as u32);
        SendMessageW(
            c,
            BM_SETCHECK,
            Some(WPARAM(if on { BST_CHECKED.0 as usize } else { 0 })),
            None,
        );
    };
    check(
        y,
        ID_TILE,
        "Tile windows and use workspaces 1–10  ·  ترتيب النوافذ",
        cfg.window_manager,
    );
    y += s(28);
    check(
        y,
        ID_TASKBAR,
        "Hide the Windows taskbar  ·  إخفاء شريط ويندوز",
        cfg.hide_taskbar,
    );
    y += s(28);
    check(
        y,
        ID_WALLPAPER,
        "Wallpaper engine, clock and window glow  ·  الخلفية والإضاءة",
        cfg.wallpaper_engine,
    );
    y += s(28);
    check(
        y,
        ID_ACCENT,
        "Take the accent colour from the wallpaper  ·  لون من الخلفية",
        cfg.dynamic_accent,
    );
    y += s(28);
    check(
        y,
        ID_AUTOSTART,
        "Start when I sign in  ·  التشغيل مع ويندوز",
        true,
    );
    y += s(44);

    button(
        y,
        IDCANCEL.0,
        "Cancel",
        x + w - s(200),
        s(96),
        BS_PUSHBUTTON as u32,
    );
    button(
        y,
        IDOK.0,
        "Install",
        x + w - s(96),
        s(96),
        BS_DEFPUSHBUTTON as u32,
    );
    y += s(26 + 24);

    let mut rc = RECT {
        left: 0,
        top: 0,
        right: x * 2 + w,
        bottom: y,
    };
    let _ = AdjustWindowRectExForDpi(
        &mut rc,
        WS_CAPTION | WS_SYSMENU,
        false,
        WINDOW_EX_STYLE(0),
        GetDpiForWindow(h),
    );
    let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);
    let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
    let _ = SetWindowPos(h, None, (sw - ww) / 2, (sh - wh) / 2, ww, wh, SWP_NOZORDER);
    if let Ok(name) = GetDlgItem(Some(h), ID_NAME) {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(Some(name));
    }
}

unsafe fn collect(h: HWND) -> Choices {
    let mut c = ST.with(|st| st.borrow().as_ref().unwrap().config.clone());
    c.user_name = text_of(h, ID_NAME);
    c.rank_folders = text_of(h, ID_WALL);
    c.weather_city = text_of(h, ID_CITY);
    c.focus_file = text_of(h, ID_FOCUS);
    if let Ok(combo) = GetDlgItem(Some(h), ID_RANK) {
        let i = SendMessageW(combo, CB_GETCURSEL, None, None).0;
        if let Some((r, color)) = RANKS.get(i.max(0) as usize) {
            if c.rank != *r {
                c.rank = *r;
                c.accent = (*color).into();
            }
        }
    }
    c.window_manager = checked(h, ID_TILE);
    c.hide_taskbar = checked(h, ID_TASKBAR);
    c.wallpaper_engine = checked(h, ID_WALLPAPER);
    c.dynamic_accent = checked(h, ID_ACCENT);
    Choices {
        config: c,
        autostart: checked(h, ID_AUTOSTART),
    }
}

unsafe extern "system" fn proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => {
            // dark title bar to match the shell
            let dark: i32 = 1;
            let _ =
                DwmSetWindowAttribute(h, DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, 4);
            build(h);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wp.0 & 0xffff) as i32;
            match id {
                ID_WALL_BROWSE => {
                    if let Some(p) = pick(h, true) {
                        set_text(h, ID_WALL, &p);
                    }
                }
                ID_FOCUS_BROWSE => {
                    if let Some(p) = pick(h, false) {
                        set_text(h, ID_FOCUS, &p);
                    }
                }
                x if x == IDOK.0 => {
                    let c = collect(h);
                    ST.with(|st| st.borrow_mut().as_mut().unwrap().result = Some(c));
                    let _ = DestroyWindow(h);
                }
                x if x == IDCANCEL.0 => {
                    let _ = DestroyWindow(h);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            SetBkMode(HDC(wp.0 as *mut _), TRANSPARENT);
            LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize)
        }
        WM_CLOSE => {
            let _ = DestroyWindow(h);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

/// Show the questions (pre-filled from current settings) and wait for Install or Cancel.
pub fn ask(update: bool) -> Option<Choices> {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let config = Store::open().map(|s| s.config).unwrap_or_default();
        let dpi = GetDpiForSystem().max(96) as i32;
        let mk = |size: i32, weight: i32| {
            CreateFontW(
                -(size * dpi / 72),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                w!("Segoe UI"),
            )
        };
        ST.with(|st| {
            *st.borrow_mut() = Some(State {
                config,
                result: None,
                font: mk(9, 400),
                bold: mk(13, 600),
            })
        });
        let module = GetModuleHandleW(None).ok()?;
        let class = w!("MonarchNext.Setup");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: module.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            hIcon: LoadIconW(Some(module.into()), PCWSTR(1 as _)).unwrap_or_default(),
            ..Default::default()
        });
        let title = if update {
            w!("Update Monarch Next")
        } else {
            w!("Install Monarch Next")
        };
        let h = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            title,
            WS_CAPTION | WS_SYSMENU | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            600,
            600,
            None,
            None,
            Some(module.into()),
            None,
        )
        .ok()?;
        if update {
            if let Ok(b) = GetDlgItem(Some(h), IDOK.0) {
                let _ = SetWindowTextW(b, w!("Update"));
            }
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(h, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        let (font, bold, result) = ST.with(|st| {
            let mut st = st.borrow_mut();
            let s = st.take().unwrap();
            (s.font, s.bold, s.result)
        });
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(bold.into());
        result
    }
}

/// Save the answers and prepare the wallpaper folders (one per rank).
pub fn apply(c: &Choices) -> std::io::Result<()> {
    let mut store = Store::open()?;
    store.config = c.config.clone();
    store.save()?;
    let root = if c.config.rank_folders.trim().is_empty() {
        crate::wallpaper::default_rank_root()
    } else {
        Some(std::path::PathBuf::from(c.config.rank_folders.trim()))
    };
    if let Some(root) = root {
        for (r, _) in RANKS {
            let _ = std::fs::create_dir_all(root.join(r.to_string()));
        }
    }
    Ok(())
}
