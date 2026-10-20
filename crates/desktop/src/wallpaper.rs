//! Native depth wallpaper (replaces Lively and the old engine, in-process, no external programs).
//!
//! Frame = base image → clock (kicker · Arabic date · hh:mm, DirectWrite via `render`) → depth layer
//! (`<name>_depth.png`, the subject stands in front of the clock) → vignette + static grain. It is shown
//! in a window behind the desktop icons:
//! * Windows 11 24H2+: WorkerW is a child of Progman; our window is a `WS_EX_LAYERED` child of Progman
//!   (alpha 255) placed directly below `SHELLDLL_DefView`. Needs the Windows 10 `supportedOS` entry in
//!   the manifest (present in `app.manifest`).
//! * Older builds: a child of the WorkerW sibling that follows the icon host.
//!
//! Threads: a UI thread (windows, paint, Explorer-restart / display / resume events) and a render
//! worker (WIC decode, composition, clock). The wallpaper is decoded once at screen size; only the
//! clock rectangle is recomposed, once per minute (aligned to the minute) or when accent / rank /
//! image change. Nothing runs in between. Rank folders + rotation + dominant-colour accent live here.
//!
//! Memory: on the layered (24H2+) path the full frame is pushed once with `UpdateLayeredWindow` and
//! then freed (DWM keeps the surface); a minute update pushes only the clock rectangle
//! (`UpdateLayeredWindowIndirect` + dirty rect). The frost ("panel open") is kept at 1/8 scale and
//! upscaled by GDI (HALFTONE) into a second layered window above the sharp one, so closing a panel
//! is instant. The engine keeps only the clock rectangle's base / depth pixels and the 1/8 frame.
//! The non-layered path (pre-24H2, or a failed push) keeps the full frame for WM_PAINT.
#![allow(dead_code)] // Public API is consumed once the integrator wires the module in.
use crate::render::{self, Align, Color, Face, Font, Rect};
use std::cell::{Cell, RefCell};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    COLORREF, GENERIC_READ, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, S_OK, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleDC, CreateDIBSection, CreateSolidBrush, DeleteDC, DeleteObject,
    EndPaint, FillRect, GetDC, InvalidateRect, RedrawWindow, ReleaseDC, ScreenToClient,
    SelectObject, SetBrushOrgEx, SetDIBitsToDevice, SetStretchBltMode, StretchDIBits, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, HALFTONE, HBITMAP, HDC, PAINTSTRUCT,
    RDW_ALLCHILDREN, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, SRCCOPY,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    IWICPalette, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant,
    WICBitmapInterpolationModeHighQualityCubic, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
    COINIT_APARTMENTTHREADED,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Theme background (--bg), the vignette colour and the fill when no image is set.
pub const DEFAULT_BG: Color = Color::rgb(5, 8, 15);
/// Theme default accent (rank E blue).
pub const DEFAULT_ACCENT: Color = Color::rgb(0x62, 0xba, 0xf8);
/// Rank folder names, lowest to highest.
pub const RANKS: [char; 6] = ['E', 'D', 'C', 'B', 'A', 'S'];
/// Upper bound on the images considered in one rank folder (bounded scans).
pub const MAX_FOLDER_IMAGES: usize = 4096;
const IMAGE_EXTS: [&str; 6] = ["png", "jpg", "jpeg", "webp", "bmp", "gif"];

// ── pure helpers: rank, time, text ──────────────────────────────────────────

/// Upper-case rank letter, `E` for anything that is not a rank.
pub fn normalize_rank(rank: char) -> char {
    let r = rank.to_ascii_uppercase();
    if RANKS.contains(&r) {
        r
    } else {
        'E'
    }
}

/// The kicker line above the date.
pub fn kicker(rank: char) -> String {
    format!("SOLO LEVELING · RANK {}", normalize_rank(rank))
}

/// Local wall-clock time (fields of SYSTEMTIME). `weekday`: 0 = Sunday.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millis: u16,
}

pub fn local_time() -> LocalTime {
    let s = unsafe { GetLocalTime() };
    LocalTime {
        year: s.wYear,
        month: s.wMonth as u8,
        day: s.wDay as u8,
        weekday: s.wDayOfWeek as u8,
        hour: s.wHour as u8,
        minute: s.wMinute as u8,
        second: s.wSecond as u8,
        millis: s.wMilliseconds,
    }
}

/// Milliseconds until the next minute boundary (1..=60000).
pub fn ms_to_next_minute(second: u8, millis: u16) -> u64 {
    let into = second.min(59) as u64 * 1000 + millis.min(999) as u64;
    60_000 - into
}

/// 12-hour `hh:mm` (the legacy clock's `%I:%M`).
pub fn clock_hhmm(t: &LocalTime) -> String {
    let h = match t.hour % 12 {
        0 => 12,
        h => h,
    };
    format!("{h:02}:{:02}", t.minute)
}

fn arabic_digits(n: u32) -> String {
    n.to_string()
        .chars()
        .map(|c| {
            c.to_digit(10)
                .and_then(|d| char::from_u32(0x660 + d))
                .unwrap_or(c)
        })
        .collect()
}

/// "الخميس، ١٦ أكتوبر" (weekday، day month), Arabic-Indic digits.
pub fn arabic_date(t: &LocalTime) -> String {
    const DAYS: [&str; 7] = [
        "الأحد",
        "الاثنين",
        "الثلاثاء",
        "الأربعاء",
        "الخميس",
        "الجمعة",
        "السبت",
    ];
    const MONTHS: [&str; 12] = [
        "يناير",
        "فبراير",
        "مارس",
        "أبريل",
        "مايو",
        "يونيو",
        "يوليو",
        "أغسطس",
        "سبتمبر",
        "أكتوبر",
        "نوفمبر",
        "ديسمبر",
    ];
    let day = DAYS[t.weekday as usize % 7];
    let month = MONTHS[(t.month.clamp(1, 12) - 1) as usize];
    format!("{day}، {} {month}", arabic_digits(t.day as u32))
}

/// Milliseconds since the Unix epoch (0 if the clock is before 1970).
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ── pure helpers: rank folders and rotation ─────────────────────────────────

/// `Pictures\Shadow Monarch` (the legacy default rank root).
pub fn default_rank_root() -> Option<PathBuf> {
    crate::platform::known_folder(&windows::Win32::UI::Shell::FOLDERID_Pictures)
        .ok()
        .map(|p| p.join("Shadow Monarch"))
}

/// `<root>\<RANK>`.
pub fn rank_dir(root: &Path, rank: char) -> PathBuf {
    root.join(normalize_rank(rank).to_string())
}

/// Folder the wallpapers are taken from: the rank's sub-folder when it holds images, otherwise the
/// root itself — so a plain folder of images works without any E…S structure.
pub fn images_dir(root: &Path, rank: char) -> PathBuf {
    let ranked = rank_dir(root, rank);
    if list_images(&ranked).is_empty() && !list_images(root).is_empty() {
        root.to_path_buf()
    } else {
        ranked
    }
}

/// A wallpaper candidate: known image extension and not a `_depth` layer.
pub fn is_image_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    let Some((stem, ext)) = n.rsplit_once('.') else {
        return false;
    };
    IMAGE_EXTS.contains(&ext) && !stem.ends_with("_depth") && !stem.is_empty()
}

/// Images in `dir`, sorted by file name (case-insensitive), at most [`MAX_FOLDER_IMAGES`].
pub fn list_images(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| is_image_name(&n.to_string_lossy()))
                .unwrap_or(false)
        })
        .take(MAX_FOLDER_IMAGES)
        .collect();
    v.sort_by_key(|p| sort_key(p));
    v
}

fn sort_key(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .to_lowercase()
        .eq(&b.to_string_lossy().to_lowercase())
}

/// The foreground layer `<stem>_depth.<png|webp>` next to the image, if present.
pub fn depth_for(image: &Path) -> Option<PathBuf> {
    let stem = image.file_stem()?.to_string_lossy().to_string();
    let dir = image.parent()?;
    ["png", "webp"]
        .iter()
        .map(|ext| dir.join(format!("{stem}_depth.{ext}")))
        .find(|p| p.is_file())
}

/// Index after `current` (wrapping), or the first one; `None` for an empty list.
pub fn next_index(current: Option<usize>, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match current {
        Some(i) if i + 1 < len => i + 1,
        _ => 0,
    })
}

/// The image after `current` in `list` (case-insensitive path match), wrapping; the first image when
/// `current` is not in the list.
pub fn next_in_list<'a>(list: &'a [PathBuf], current: Option<&Path>) -> Option<&'a PathBuf> {
    let pos = current.and_then(|c| list.iter().position(|p| same_path(p, c)));
    next_index(pos, list.len()).and_then(|i| list.get(i))
}

/// Next wallpaper of a folder (see [`next_in_list`]).
pub fn next_in_folder(dir: &Path, current: Option<&Path>) -> Option<PathBuf> {
    next_in_list(&list_images(dir), current).cloned()
}

/// Milliseconds until the next rotation (0 = due now); `None` when rotation is off (`hours <= 0`).
/// A last-rotation time in the future (clock moved back) waits at most one period.
pub fn rotation_wait_ms(last_ms: u64, now_ms: u64, hours: f32) -> Option<u64> {
    if !hours.is_finite() || hours <= 0.0 {
        return None;
    }
    let period = (hours as f64 * 3_600_000.0).max(60_000.0) as u64;
    Some(
        last_ms
            .saturating_add(period)
            .saturating_sub(now_ms)
            .min(period),
    )
}

/// Shortest worker sleep while a rotation is pending (no busy loop even if the wait reads 0).
const MIN_ROTATION_WAIT_MS: u64 = 1_000;

/// Folder rotation schedule.
struct Rotation {
    root: Option<PathBuf>,
    hours: f32,
    /// Unix ms of the last rotation (or of the last scheduled attempt that found nothing).
    last: u64,
}

impl Rotation {
    /// See [`rotation_wait_ms`]; `None` without a rank root.
    fn wait(&self, now_ms: u64) -> Option<u64> {
        self.root.as_ref()?;
        rotation_wait_ms(self.last, now_ms, self.hours)
    }

    /// Next image of the rank folder. A rotation restarts the period; so does a *scheduled* attempt
    /// that finds nothing (missing / empty folder), otherwise the wait stays 0 and the worker would
    /// rescan the folder forever. A manual attempt that finds nothing leaves the schedule alone.
    fn next(
        &mut self,
        rank: char,
        current: Option<&Path>,
        now_ms: u64,
        scheduled: bool,
    ) -> Option<PathBuf> {
        let next = self
            .root
            .as_ref()
            .and_then(|r| next_in_folder(&images_dir(r, rank), current));
        if next.is_some() || scheduled {
            self.last = now_ms;
        }
        next
    }
}

// ── pure helpers: accent extraction ─────────────────────────────────────────

/// HSL (h in degrees, s/l 0..1), .NET Color convention.
fn hsl(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.0;
    let d = mx - mn;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = if l <= 0.5 {
        d / (mx + mn)
    } else {
        d / (2.0 - mx - mn)
    };
    let mut h = if mx == r {
        (g - b) / d
    } else if mx == g {
        2.0 + (b - r) / d
    } else {
        4.0 + (r - g) / d
    } * 60.0;
    if h < 0.0 {
        h += 360.0;
    }
    (h, s, l)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match ((h / 60.0).floor() as i64).rem_euclid(6) {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let to = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to(r), to(g), to(b))
}

/// Dominant *vivid* colour of straight RGBA pixels (Material-You style): the image is reduced to a
/// 64×40 grid, colours are binned by hue (24 bins of 15°) weighted by saturation and mid lightness,
/// and the winning bin's mean is clamped to glow on the navy theme (S 0.65–0.92, L 0.62–0.70).
/// `None` for grey / dark images (keep the rank colour) or a short buffer.
use monarch_core::material;
fn color_of(c: [u8; 3]) -> Color {
    Color::rgb(c[0], c[1], c[2])
}
pub fn accent_of(rgba: &[u8], w: u32, h: u32) -> Option<Color> {
    accent_from(rgba, w as usize, h as usize, false)
}

fn accent_from(px: &[u8], w: usize, h: usize, bgra: bool) -> Option<Color> {
    if w == 0 || h == 0 || px.len() < w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    let (cols, rows) = (w.min(64), h.min(40));
    let mut bins = [[0f64; 4]; 24]; // weight, r, g, b
    for cy in 0..rows {
        let y0 = cy * h / rows;
        let y1 = ((cy + 1) * h / rows).max(y0 + 1);
        let sy = ((y1 - y0) / 4).max(1);
        for cx in 0..cols {
            let x0 = cx * w / cols;
            let x1 = ((cx + 1) * w / cols).max(x0 + 1);
            let sx = ((x1 - x0) / 4).max(1);
            let (mut r, mut g, mut b, mut k) = (0f64, 0f64, 0f64, 0f64);
            for y in (y0..y1).step_by(sy) {
                for x in (x0..x1).step_by(sx) {
                    let i = (y * w + x) * 4;
                    let (pr, pg, pb) = if bgra {
                        (px[i + 2], px[i + 1], px[i])
                    } else {
                        (px[i], px[i + 1], px[i + 2])
                    };
                    r += pr as f64;
                    g += pg as f64;
                    b += pb as f64;
                    k += 1.0;
                }
            }
            if k == 0.0 {
                continue;
            }
            let (r, g, b) = (r / k, g / k, b / k);
            let (hue, s, l) = hsl(r / 255.0, g / 255.0, b / 255.0);
            if s < 0.28 || !(0.18..=0.9).contains(&l) {
                continue;
            }
            let wgt = s * (1.0 - (l - 0.55).abs());
            let bin = &mut bins[(hue / 15.0).floor() as usize % 24];
            bin[0] += wgt;
            bin[1] += r * wgt;
            bin[2] += g * wgt;
            bin[3] += b * wgt;
        }
    }
    let best = (0..24).max_by(|a, b| bins[*a][0].total_cmp(&bins[*b][0]))?;
    let bin = bins[best];
    // legacy threshold: 6 weight units on a 64×40 grid
    if bin[0] <= 0.0 || bin[0] < 6.0 * (cols * rows) as f64 / 2560.0 {
        return None;
    }
    let (r, g, b) = (
        bin[1] / bin[0] / 255.0,
        bin[2] / bin[0] / 255.0,
        bin[3] / bin[0] / 255.0,
    );
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.0;
    let d = mx - mn;
    let s = if d == 0.0 {
        0.0
    } else {
        d / (1.0 - (2.0 * l - 1.0).abs()).max(1e-9)
    };
    let mut hue = if d == 0.0 {
        0.0
    } else if mx == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if mx == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    if hue < 0.0 {
        hue += 360.0;
    }
    let (r, g, b) = hsl_to_rgb(hue, s.clamp(0.65, 0.92), l.clamp(0.62, 0.70));
    Some(Color::rgb(r, g, b))
}

// ── pure helpers: pixels ────────────────────────────────────────────────────

/// 3-pass box blur on a float mask ≈ gaussian with `sigma` (O(n), independent of the radius).
pub fn blur_mask(m: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if w == 0 || h == 0 || m.len() < w * h || sigma <= 0.0 {
        return m.to_vec();
    }
    let r = ((sigma * sigma * 4.0 + 1.0).sqrt() / 2.0).round().max(1.0) as usize;
    let mut a = m[..w * h].to_vec();
    let mut b = vec![0f32; w * h];
    for _ in 0..3 {
        for y in 0..h {
            box_line(&a, &mut b, y * w, 1, w, r);
        }
        for x in 0..w {
            box_line(&b, &mut a, x, w, h, r);
        }
    }
    a
}

/// One box-filter pass of radius `r` over `n` samples starting at `start` with `stride`;
/// samples beyond the ends repeat the edge value (no dark borders).
fn box_line(src: &[f32], dst: &mut [f32], start: usize, stride: usize, n: usize, r: usize) {
    let at = |i: isize| src[start + (i.clamp(0, n as isize - 1) as usize) * stride];
    let ri = r as isize;
    let mut acc: f32 = (-ri..=ri).map(at).sum();
    let div = (2 * r + 1) as f32;
    for i in 0..n as isize {
        dst[start + i as usize * stride] = acc / div;
        acc += at(i + ri + 1) - at(i - ri);
    }
}

/// Colour as BGR floats 0..255 (the frame's byte order).
fn bgr(c: Color) -> [f32; 3] {
    [c.b * 255.0, c.g * 255.0, c.r * 255.0]
}

/// Premultiplied BGRA `src` over opaque `dst` (same size).
fn over_premul(dst: &mut [u8], src: &[u8]) {
    for (d, s) in dst
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(src.as_chunks::<4>().0)
    {
        let ia = 255 - s[3] as u32;
        if ia == 255 {
            continue;
        }
        for c in 0..3 {
            d[c] = (s[c] as u32 + (d[c] as u32 * ia + 127) / 255).min(255) as u8;
        }
        d[3] = 255;
    }
}

/// Static film grain, a function of the pixel position (so partial re-renders match).
fn grain(x: u32, y: u32) -> f32 {
    let mut s = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ 0x2545_F491;
    s ^= s >> 15;
    s = s.wrapping_mul(0x2C1B_3C6D);
    s ^= s >> 12;
    ((s & 0xff) as f32 - 127.5) * 0.05
}

/// Vignette (transparent at 55 % of the half-diagonal → `bg` at 55 % strength in the corners) and
/// grain over rows `y0..` of a frame `w × h`.
fn finish_rows(buf: &mut [u8], w: usize, h: usize, y0: usize, bg: [f32; 3]) {
    finish_rect(buf, w, w, h, 0, y0, bg);
}

/// [`finish_rows`] for a `bw`-pixel-wide rectangle at (`x0`, `y0`) of a `w × h` frame.
fn finish_rect(buf: &mut [u8], bw: usize, w: usize, h: usize, x0: usize, y0: usize, bg: [f32; 3]) {
    if w == 0 || h == 0 || bw == 0 {
        return;
    }
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    for (ry, row) in buf.chunks_exact_mut(bw * 4).enumerate() {
        let y = y0 + ry;
        let dy = (y as f32 + 0.5 - cy) / cy;
        for (rx, p) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let x = x0 + rx;
            let dx = (x as f32 + 0.5 - cx) / cx;
            let d = (dx * dx + dy * dy).sqrt() / std::f32::consts::SQRT_2;
            let v = ((d - 0.55) / 0.45).clamp(0.0, 1.0) * 0.55;
            let g = grain(x as u32, y as u32);
            for c in 0..3 {
                p[c] = (p[c] as f32 * (1.0 - v) + bg[c] * v + g)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
            p[3] = 255;
        }
    }
}

/// A float mask placed in a BGRA buffer.
struct Mask<'a> {
    a: &'a [f32],
    w: usize,
    h: usize,
    x0: i64,
    y0: i64,
}

/// Blend `color` through `mask × alpha` onto an opaque BGRA buffer `bw` pixels wide.
fn blend_mask(buf: &mut [u8], bw: usize, m: &Mask, color: [f32; 3], alpha: f32) {
    let bh = buf.len() / (bw * 4).max(1);
    for my in 0..m.h {
        let y = m.y0 + my as i64;
        if y < 0 || y as usize >= bh {
            continue;
        }
        for mx in 0..m.w {
            let x = m.x0 + mx as i64;
            if x < 0 || x as usize >= bw {
                continue;
            }
            let a = (m.a[my * m.w + mx] * alpha).min(1.0);
            if a <= 0.002 {
                continue;
            }
            let i = (y as usize * bw + x as usize) * 4;
            for c in 0..3 {
                buf[i + c] = (buf[i + c] as f32 * (1.0 - a) + color[c] * a) as u8;
            }
        }
    }
}

/// Downscale factor of the frost ("panel open") frame.
const FROST_SCALE: usize = 8;

/// A small opaque BGRA image (the frame at 1/[`FROST_SCALE`], or its frost).
#[derive(Clone, Debug, Default, PartialEq)]
struct Small {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Small {
    fn for_frame(w: usize, h: usize) -> Self {
        let (sw, sh) = (w.div_ceil(FROST_SCALE), h.div_ceil(FROST_SCALE));
        Self {
            w: sw,
            h: sh,
            px: [0, 0, 0, 255].repeat(sw * sh),
        }
    }
}

/// Box-average a `bw × bh` BGRA rectangle placed at (`x0`, `y0`) of the frame into the cells of
/// `small` it covers. `x0` / `y0` are multiples of [`FROST_SCALE`]; the right / bottom edge is a
/// multiple of it or the frame edge, so every touched cell is covered completely.
fn shrink_into(small: &mut Small, buf: &[u8], bw: usize, bh: usize, x0: usize, y0: usize) {
    const F: usize = FROST_SCALE;
    if bw == 0 || bh == 0 || buf.len() < bw * bh * 4 {
        return;
    }
    let (cx0, cy0) = (x0 / F, y0 / F);
    for cy in 0..bh.div_ceil(F) {
        let sy = cy0 + cy;
        if sy >= small.h {
            break;
        }
        let (ya, yb) = (cy * F, ((cy + 1) * F).min(bh));
        for cx in 0..bw.div_ceil(F) {
            let sx = cx0 + cx;
            if sx >= small.w {
                break;
            }
            let (xa, xb) = (cx * F, ((cx + 1) * F).min(bw));
            let mut acc = [0u32; 3];
            for y in ya..yb {
                for p in buf[(y * bw + xa) * 4..(y * bw + xb) * 4].as_chunks::<4>().0 {
                    for (a, v) in acc.iter_mut().zip(&p[..3]) {
                        *a += *v as u32;
                    }
                }
            }
            let n = ((yb - ya) * (xb - xa)) as u32;
            let o = (sy * small.w + sx) * 4;
            for (dst, a) in small.px[o..o + 3].iter_mut().zip(acc) {
                *dst = ((a + n / 2) / n) as u8;
            }
            small.px[o + 3] = 255;
        }
    }
}

/// Frost of the 1/8 frame: gaussian (σ 2.2 cells) and brightness 0.7, still at 1/8 scale; the
/// present step upscales it (GDI HALFTONE), so no full-size frosted copy is kept.
fn frost_of(small: &Small) -> Small {
    let (sw, sh) = (small.w, small.h);
    if sw == 0 || sh == 0 || small.px.len() < sw * sh * 4 {
        return small.clone();
    }
    let mut out = small.clone();
    for c in 0..3 {
        let plane: Vec<f32> = small
            .px
            .as_chunks::<4>()
            .0
            .iter()
            .map(|p| p[c] as f32)
            .collect();
        let blurred = blur_mask(&plane, sw, sh, 2.2);
        for (o, v) in out.px.as_chunks_mut::<4>().0.iter_mut().zip(blurred) {
            o[c] = (v * 0.7).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Downscale of the frame used as the glass backdrop (1/4: clear enough to read shapes through
/// the glass, ~170 KB at 1080p).
const GLASS_SCALE: usize = 4;

/// Backdrop for the shell's glass surfaces, "crystal" style: the frame box-averaged to 1/4,
/// a light blur (σ 1.6 cells ≈ 6 px) so shapes stay recognisable through the glass, colours a
/// little more saturated (vibrancy) and barely dimmed.
fn glass_of(frame: &[u8], w: usize, h: usize) -> render::Backdrop {
    const S: usize = GLASS_SCALE;
    let (sw, sh) = (w.div_ceil(S), h.div_ceil(S));
    let mut planes = vec![vec![0f32; sw * sh]; 3];
    if w > 0 && h > 0 && frame.len() >= w * h * 4 {
        for sy in 0..sh {
            let (ya, yb) = (sy * S, ((sy + 1) * S).min(h));
            for sx in 0..sw {
                let (xa, xb) = (sx * S, ((sx + 1) * S).min(w));
                let mut acc = [0f32; 3];
                for y in ya..yb {
                    for p in frame[(y * w + xa) * 4..(y * w + xb) * 4].as_chunks::<4>().0 {
                        for (a, v) in acc.iter_mut().zip(&p[..3]) {
                            *a += *v as f32;
                        }
                    }
                }
                let n = ((yb - ya) * (xb - xa)) as f32;
                for (plane, a) in planes.iter_mut().zip(acc) {
                    plane[sy * sw + sx] = a / n;
                }
            }
        }
    }
    let blurred: Vec<Vec<f32>> = planes.iter().map(|p| blur_mask(p, sw, sh, 1.6)).collect();
    let mut px = vec![255u8; sw * sh * 4];
    for (i, o) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (b, g, r) = (blurred[0][i], blurred[1][i], blurred[2][i]);
        let luma = 0.0722 * b + 0.7152 * g + 0.2126 * r;
        for (c, v) in [b, g, r].into_iter().enumerate() {
            o[c] = ((luma + (v - luma) * 1.3) * 0.94).round().clamp(0.0, 255.0) as u8;
        }
    }
    render::Backdrop {
        w: sw,
        h: sh,
        scale: S,
        px,
    }
}

// ── clock layout (DesktopClock.qml) ─────────────────────────────────────────

struct Line {
    text: String,
    font: Font,
    color: Color,
    tracking: f32,
    /// Frame row of the line box top.
    y: f32,
    h: f32,
    glow_sigma: f32,
    glow_alpha: f32,
}

struct ClockLayout {
    lines: Vec<Line>,
    /// Frame rows covered by the clock (text + glow + shadow).
    y0: usize,
    y1: usize,
    /// Glow / shadow reach around the ink (px).
    pad: f32,
}

fn clock_layout(w: usize, h: usize, accent: Color, rank: char, t: &LocalTime) -> ClockLayout {
    let vw = w as f32 / 100.0;
    let k = w as f32 / 1920.0; // glow radii were tuned at 1080p
    let (ks, ds, ts) = (1.35 * vw, 1.9 * vw, 13.5 * vw);
    let (kh, dh, th) = (ks * 1.4, ds * 1.7, ts * 1.15);
    let ky = h as f32 * 0.13;
    let dy = ky + kh + 0.5 * vw;
    let ty = dy + dh * 0.85;
    let pad = (3.0 * (20.0 * k).max(ts * 0.08) + ts * 0.05 + 4.0).ceil();
    let y0 = (ky - pad).max(0.0) as usize;
    let y1 = ((ty + th + pad).ceil() as usize).min(h).max(y0);
    let line = |text: String, font, color, tracking, y: f32, h, glow_sigma, glow_alpha| Line {
        text,
        font,
        color,
        tracking,
        y,
        h,
        glow_sigma,
        glow_alpha,
    };
    ClockLayout {
        lines: vec![
            line(
                kicker(rank),
                Font::new(Face::Label, ks, 600),
                accent.alpha(0.9),
                0.5 * ks,
                ky,
                kh,
                9.0 * k,
                0.6,
            ),
            line(
                arabic_date(t),
                Font::new(Face::Head, ds, 600),
                Color::hex("#e8f1ff").alpha(0.92),
                0.0,
                dy,
                dh,
                0.0,
                0.0,
            ),
            line(
                clock_hhmm(t),
                Font::new(Face::Label, ts, 700),
                accent,
                0.02 * ts,
                ty,
                th,
                20.0 * k,
                0.45,
            ),
        ],
        y0,
        y1,
        pad,
    }
}

/// The clock rectangle of a frame: the pixels the clock (ink + glow + shadow) can touch.
/// `x0` / `y0` are multiples of [`FROST_SCALE`]; `x1` / `y1` too, or the frame edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Crop {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl Crop {
    fn w(&self) -> usize {
        self.x1 - self.x0
    }
    fn h(&self) -> usize {
        self.y1 - self.y0
    }
    /// Bytes of a BGRA buffer of this size.
    fn len(&self) -> usize {
        self.w() * self.h() * 4
    }
}

fn align_down(v: usize) -> usize {
    v / FROST_SCALE * FROST_SCALE
}

fn align_up(v: usize, max: usize) -> usize {
    v.div_ceil(FROST_SCALE).saturating_mul(FROST_SCALE).min(max)
}

/// Index in `0..n` maximising `f`.
fn arg_max(n: u8, f: impl Fn(u8) -> f32) -> u8 {
    (0..n)
        .map(|i| (i, f(i)))
        .fold((0, f32::MIN), |a, b| if b.1 > a.1 { b } else { a })
        .0
}

/// Widest possible clock line (px): every rank's kicker, the widest weekday / month / day number,
/// the widest `dd:dd`. `None` when DirectWrite is unavailable.
fn widest_clock_line(w: usize, h: usize) -> Option<f32> {
    let l = clock_layout(w, h, DEFAULT_ACCENT, 'E', &LocalTime::default());
    render::measure_with(|c| {
        let m = |s: &str, i: usize| c.measure(s, &l.lines[i].font, l.lines[i].tracking);
        let kick = RANKS.iter().map(|r| m(&kicker(*r), 0)).fold(0f32, f32::max);
        let date = |t: LocalTime| m(&arabic_date(&t), 1);
        let mut t = LocalTime {
            day: 28,
            month: 1,
            ..Default::default()
        };
        t.weekday = arg_max(7, |d| date(LocalTime { weekday: d, ..t }));
        t.month = 1 + arg_max(12, |mo| date(LocalTime { month: mo + 1, ..t }));
        t.day = 1 + arg_max(31, |d| date(LocalTime { day: d + 1, ..t }));
        let time = (0..10)
            .map(|d| m(&format!("{d}{d}:{d}{d}"), 2))
            .fold(0f32, f32::max);
        kick.max(date(t)).max(time)
    })
}

/// Clock rectangle of a `w × h` frame: the layout's rows, and (unless `wide`) the widest possible
/// line + glow reach around the centre column instead of the full width.
fn clock_crop(w: usize, h: usize, wide: bool) -> Crop {
    let l = clock_layout(w, h, DEFAULT_ACCENT, 'E', &LocalTime::default());
    let (y0, y1) = (align_down(l.y0), align_up(l.y1, h));
    let full = Crop {
        x0: 0,
        y0,
        x1: w,
        y1: y1.max(y0),
    };
    if wide {
        return full;
    }
    let Some(line) = widest_clock_line(w, h).filter(|v| v.is_finite() && *v > 0.0) else {
        return full;
    };
    let half = line * 1.08 / 2.0 + l.pad + 8.0;
    let cx = w as f32 / 2.0;
    let x0 = align_down((cx - half).max(0.0) as usize);
    let x1 = align_up((cx + half).ceil() as usize, w);
    if x1 <= x0 {
        full
    } else {
        Crop { x0, x1, ..full }
    }
}

// ── WIC ─────────────────────────────────────────────────────────────────────

/// Mirror the image into the Windows wallpaper. Builds where no desktop child window is composed
/// (Progman children render nothing on some 24H2+ builds) still show the right picture, and the
/// lock-free Explorer path costs nothing at runtime.
fn set_system_wallpaper(path: &Path) {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    unsafe {
        // "Fill" (cover), like our own frame, so the glass backdrop lines up with the picture
        use windows::Win32::System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ};
        for (name, value) in [(w!("WallpaperStyle"), "10"), (w!("TileWallpaper"), "0")] {
            let v: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                w!("Control Panel\\Desktop"),
                name,
                REG_SZ.0,
                Some(v.as_ptr().cast()),
                (v.len() * 2) as u32,
            );
        }
        let _ = SystemParametersInfoW(
            SPI_SETDESKWALLPAPER,
            0,
            Some(wide.as_mut_ptr().cast()),
            SPIF_UPDATEINIFILE | SPIF_SENDCHANGE,
        );
    }
}

fn io_err(e: windows::core::Error) -> io::Error {
    io::Error::other(e)
}

fn wic_factory() -> io::Result<IWICImagingFactory> {
    unsafe {
        CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(io_err)
    }
}

fn frame_len(w: usize, h: usize) -> io::Result<usize> {
    w.checked_mul(h)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| io::Error::other("image too large"))
}

/// Premultiplied-BGRA source of an image, cropped to the target aspect (cover, centred) and scaled
/// to `w × h`. Nothing is decoded until pixels are copied out (whole or in strips).
fn cover_source(
    f: &IWICImagingFactory,
    path: &Path,
    w: u32,
    h: u32,
) -> io::Result<IWICBitmapSource> {
    if w == 0 || h == 0 {
        return Err(io::Error::other("empty target size"));
    }
    let wide = crate::platform::wide(path.as_os_str());
    unsafe {
        let dec = f
            .CreateDecoderFromFilename(
                PCWSTR(wide.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
            .map_err(io_err)?;
        let frame = dec.GetFrame(0).map_err(io_err)?;
        let (mut sw, mut sh) = (0u32, 0u32);
        frame.GetSize(&mut sw, &mut sh).map_err(io_err)?;
        if sw == 0 || sh == 0 {
            return Err(io::Error::other("empty image"));
        }
        let (src_aspect, dst_aspect) = (sw as f64 / sh as f64, w as f64 / h as f64);
        let crop = if src_aspect > dst_aspect {
            let cw = ((sh as f64 * dst_aspect).round() as u32).clamp(1, sw);
            WICRect {
                X: ((sw - cw) / 2) as i32,
                Y: 0,
                Width: cw as i32,
                Height: sh as i32,
            }
        } else {
            let ch = ((sw as f64 / dst_aspect).round() as u32).clamp(1, sh);
            WICRect {
                X: 0,
                Y: ((sh - ch) / 2) as i32,
                Width: sw as i32,
                Height: ch as i32,
            }
        };
        let clip = f.CreateBitmapClipper().map_err(io_err)?;
        clip.Initialize(&frame, &crop).map_err(io_err)?;
        let src: IWICBitmapSource = if crop.Width as u32 == w && crop.Height as u32 == h {
            windows::core::Interface::cast(&clip).map_err(io_err)?
        } else {
            let scaler = f.CreateBitmapScaler().map_err(io_err)?;
            let mode = if (crop.Width as u32) > w * 2 {
                WICBitmapInterpolationModeFant
            } else {
                WICBitmapInterpolationModeHighQualityCubic
            };
            scaler.Initialize(&clip, w, h, mode).map_err(io_err)?;
            windows::core::Interface::cast(&scaler).map_err(io_err)?
        };
        let conv = f.CreateFormatConverter().map_err(io_err)?;
        conv.Initialize(
            &src,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )
        .map_err(io_err)?;
        windows::core::Interface::cast(&conv).map_err(io_err)
    }
}

/// Copy a whole `w × h` source into `out` (exactly `w * h * 4` bytes).
fn copy_all(src: &IWICBitmapSource, w: usize, h: usize, out: &mut [u8]) -> io::Result<()> {
    let len = frame_len(w, h)?;
    if out.len() != len {
        return Err(io::Error::other("buffer size mismatch"));
    }
    unsafe { src.CopyPixels(std::ptr::null(), (w * 4) as u32, out) }.map_err(io_err)
}

/// Decode an image cover-cropped and scaled to `w × h`, as premultiplied BGRA.
fn decode_cover(f: &IWICImagingFactory, path: &Path, w: u32, h: u32) -> io::Result<Vec<u8>> {
    let src = cover_source(f, path, w, h)?;
    let mut px = vec![0u8; frame_len(w as usize, h as usize)?];
    copy_all(&src, w as usize, h as usize, &mut px)?;
    Ok(px)
}

/// Straight-RGBA thumbnail (cover crop), e.g. for the wallpaper picker (`Canvas::image`).
pub fn thumbnail_rgba(path: &Path, w: u32, h: u32) -> io::Result<Vec<u8>> {
    with_com(|| {
        let f = wic_factory()?;
        let mut px = decode_cover(&f, path, w, h)?;
        unpremultiply_to_rgba(&mut px);
        Ok(px)
    })
}

/// Accent of an image file (decoded at 64×40), see [`accent_of`].
pub fn accent_of_file(path: &Path) -> io::Result<Option<Color>> {
    with_com(|| {
        let f = wic_factory()?;
        let px = decode_cover(&f, path, 64, 40)?;
        Ok(accent_from(&px, 64, 40, true))
    })
}

fn unpremultiply_to_rgba(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0 {
        let a = p[3] as u32;
        if a > 0 && a < 255 {
            for v in &mut p[..3] {
                *v = ((*v as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
        p.swap(0, 2);
    }
}

/// Run `f` with COM initialised on this thread (balanced only when this call initialised it).
fn with_com<T>(f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let r = f();
    if hr == S_OK {
        // this call brought COM up: `render`'s per-thread factories must go before COM does
        // (when COM was already up, the caller owns them and keeps its cache)
        render::shutdown();
    }
    if hr.is_ok() {
        unsafe { CoUninitialize() };
    }
    r
}

// ── composition ─────────────────────────────────────────────────────────────

/// What stays fixed between clock updates: the base (before depth / vignette) and depth pixels of
/// the clock rectangle, and the shown frame at 1/8 scale (source of the frost).
struct Scene {
    w: usize,
    h: usize,
    bg: [f32; 3],
    rect: Crop,
    base: Vec<u8>,
    /// `None` without a depth layer or when it is fully transparent inside `rect`.
    depth: Option<Vec<u8>>,
    small: Small,
}

impl Scene {
    /// Bytes kept between clock updates.
    fn bytes(&self) -> usize {
        self.base.len() + self.depth.as_ref().map_or(0, Vec::len) + self.small.px.len()
    }
}

fn copy_rect(frame: &[u8], fw: usize, r: Crop) -> Vec<u8> {
    let mut out = Vec::with_capacity(r.len());
    for y in r.y0..r.y1 {
        out.extend_from_slice(&frame[(y * fw + r.x0) * 4..(y * fw + r.x1) * 4]);
    }
    out
}

fn paste_rect(frame: &mut [u8], fw: usize, r: Crop, src: &[u8]) {
    if src.len() < r.len() || frame.len() < r.y1 * fw * 4 || r.x1 > fw {
        return;
    }
    let rw = r.w() * 4;
    for (i, y) in (r.y0..r.y1).enumerate() {
        frame[(y * fw + r.x0) * 4..(y * fw + r.x1) * 4].copy_from_slice(&src[i * rw..(i + 1) * rw]);
    }
}

/// Composite the depth layer over the frame in strips of rows (no second full-size buffer) and keep
/// its pixels inside `r`. `None` when the layer is fully transparent there.
fn compose_depth(
    src: &IWICBitmapSource,
    frame: &mut [u8],
    w: usize,
    h: usize,
    r: Crop,
) -> io::Result<Option<Vec<u8>>> {
    const STRIP: usize = 64;
    let mut strip = vec![0u8; w * STRIP.min(h) * 4];
    let mut keep = vec![0u8; r.len()];
    let mut any = false;
    let rw = r.w() * 4;
    let mut y0 = 0;
    while y0 < h {
        let n = STRIP.min(h - y0);
        let rect = WICRect {
            X: 0,
            Y: y0 as i32,
            Width: w as i32,
            Height: n as i32,
        };
        let buf = &mut strip[..w * n * 4];
        unsafe { src.CopyPixels(&rect, (w * 4) as u32, buf) }.map_err(io_err)?;
        for ry in 0..n {
            let y = y0 + ry;
            if (r.y0..r.y1).contains(&y) {
                let row = &buf[(ry * w + r.x0) * 4..(ry * w + r.x1) * 4];
                any |= row.as_chunks::<4>().0.iter().any(|p| p[3] != 0);
                let k = (y - r.y0) * rw;
                keep[k..k + rw].copy_from_slice(row);
            }
        }
        over_premul(&mut frame[y0 * w * 4..(y0 + n) * w * 4], buf);
        y0 += n;
    }
    Ok(any.then_some(keep))
}

/// Compose the frame without the clock into `frame` (`w × h` premultiplied BGRA) and return the
/// scene to re-render the clock from, plus the decoded image's Material You source colour (before depth / vignette).
fn build_scene(
    wic: &IWICImagingFactory,
    w: usize,
    h: usize,
    image: Option<&Path>,
    bg: Color,
    frame: &mut [u8],
    wide: bool,
) -> (Scene, Option<[u8; 3]>, Option<io::Error>) {
    let bgc = bgr(bg);
    let mut err = None;
    let decoded = match image {
        Some(p) => {
            match cover_source(wic, p, w as u32, h as u32).and_then(|s| copy_all(&s, w, h, frame)) {
                Ok(()) => true,
                Err(e) => {
                    err = Some(e);
                    false
                }
            }
        }
        None => false,
    };
    if !decoded {
        let fill = [bgc[0] as u8, bgc[1] as u8, bgc[2] as u8, 255];
        for p in frame.as_chunks_mut::<4>().0 {
            *p = fill;
        }
    }
    let seed = if decoded {
        material::source_color(frame, w, h, true)
    } else {
        None
    };
    let rect = clock_crop(w, h, wide);
    let base = copy_rect(frame, w, rect);
    let depth = image
        .filter(|_| decoded)
        .and_then(depth_for)
        .and_then(|d| cover_source(wic, &d, w as u32, h as u32).ok())
        .and_then(|s| compose_depth(&s, frame, w, h, rect).ok())
        .flatten();
    finish_rows(frame, w, h, 0, bgc);
    let mut small = Small::for_frame(w, h);
    shrink_into(&mut small, frame, w, h, 0, 0);
    (
        Scene {
            w,
            h,
            bg: bgc,
            rect,
            base,
            depth,
            small,
        },
        seed,
        err,
    )
}

/// Clock text of the rectangle as premultiplied BGRA, drawn by the shared Direct2D/DirectWrite
/// renderer straight into memory (laid out at full frame width, so it matches a full render).
fn text_layer(l: &ClockLayout, w: usize, r: Crop) -> io::Result<Vec<u8>> {
    let px = render::to_pbgra(r.w() as u32, r.h() as u32, 1.0, |c| {
        for line in &l.lines {
            c.text_tracked(
                &line.text,
                Rect::new(-(r.x0 as f32), line.y - r.y0 as f32, w as f32, line.h),
                &line.font,
                line.color,
                Align::Center,
                line.tracking,
            );
        }
    })
    .map_err(io_err)?;
    if px.len() != r.len() {
        return Err(io::Error::other("clock layer size mismatch"));
    }
    Ok(px)
}

/// True when ink touches a cut edge of the rectangle (the text may be clipped there).
fn ink_clipped(text: &[u8], r: Crop, w: usize) -> bool {
    let rw = r.w();
    if rw == 0 || text.len() < r.len() {
        return false;
    }
    (0..r.h()).any(|y| {
        (r.x0 > 0 && text[y * rw * 4 + 3] != 0)
            || (r.x1 < w && text[(y * rw + rw - 1) * 4 + 3] != 0)
    })
}

/// The finished clock rectangle (base → shadow/glow/text → depth → vignette + grain).
fn clock_band(s: &Scene, l: &ClockLayout, text: &[u8], accent: Color) -> Vec<u8> {
    let r = s.rect;
    let (w, bh) = (r.w(), r.h());
    let mut buf = s.base.clone();
    if text.len() == buf.len() && bh > 0 {
        for line in &l.lines {
            let size = line.font.size;
            let shadow_sigma = size * 0.08;
            let pad = (3.0 * shadow_sigma.max(line.glow_sigma)).ceil() as usize + 2;
            let top = line.y - r.y0 as f32;
            let (ly0, ly1) = (
                (top.max(0.0) as usize).min(bh),
                ((top + line.h).ceil().max(0.0) as usize).min(bh),
            );
            // horizontal extent of the line's ink
            let (mut x0, mut x1) = (w, 0);
            for y in ly0..ly1 {
                for x in 0..w {
                    if text[(y * w + x) * 4 + 3] > 0 {
                        x0 = x0.min(x);
                        x1 = x1.max(x + 1);
                    }
                }
            }
            if x0 >= x1 {
                continue;
            }
            let (rx0, rx1) = (x0.saturating_sub(pad), (x1 + pad).min(w));
            let (ry0, ry1) = (ly0.saturating_sub(pad), (ly1 + pad).min(bh));
            let (mw, mh) = (rx1 - rx0, ry1 - ry0);
            let mut m = vec![0f32; mw * mh];
            for y in ly0..ly1 {
                for x in x0..x1 {
                    m[(y - ry0) * mw + (x - rx0)] = text[(y * w + x) * 4 + 3] as f32 / 255.0;
                }
            }
            let shadow = blur_mask(&m, mw, mh, shadow_sigma);
            let mut mask = Mask {
                a: &shadow,
                w: mw,
                h: mh,
                x0: rx0 as i64,
                y0: ry0 as i64 + (size * 0.05) as i64,
            };
            blend_mask(&mut buf, w, &mask, [0.0; 3], 0.4);
            if line.glow_sigma > 0.0 {
                let glow = blur_mask(&m, mw, mh, line.glow_sigma);
                mask.a = &glow;
                mask.y0 = ry0 as i64;
                blend_mask(&mut buf, w, &mask, bgr(accent), line.glow_alpha);
            }
        }
        over_premul(&mut buf, text);
    }
    if let Some(d) = &s.depth {
        over_premul(&mut buf, d);
    }
    finish_rect(&mut buf, w, s.w, s.h, r.x0, r.y0, s.bg);
    buf
}

fn screen_size() -> (usize, usize) {
    unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN).max(1) as usize,
            GetSystemMetrics(SM_CYSCREEN).max(1) as usize,
        )
    }
}

/// Render one wallpaper frame (primary-screen size, current local time) to a PNG via
/// `render::snapshot`. No window is created and the desktop is not touched.
pub fn snapshot_wallpaper(
    path_out: &Path,
    image_path: Option<&Path>,
    accent: Color,
    rank: char,
) -> io::Result<()> {
    with_com(|| {
        let wic = wic_factory()?;
        let (w, h) = screen_size();
        let mut frame = vec![0u8; frame_len(w, h)?];
        for wide in [false, true] {
            let (scene, _, err) = build_scene(&wic, w, h, image_path, DEFAULT_BG, &mut frame, wide);
            if let Some(e) = err {
                return Err(e);
            }
            let l = clock_layout(w, h, accent, rank, &local_time());
            let text = text_layer(&l, w, scene.rect)?;
            if !wide && ink_clipped(&text, scene.rect, w) {
                continue;
            }
            let band = clock_band(&scene, &l, &text, accent);
            paste_rect(&mut frame, w, scene.rect, &band);
            break;
        }
        unpremultiply_to_rgba(&mut frame);
        render::snapshot(path_out, w as u32, h as u32, 1.0, |c| {
            c.image(
                &frame,
                w as u32,
                h as u32,
                Rect::new(0.0, 0.0, w as f32, h as f32),
                1.0,
            )
        })
        .map_err(io_err)
    })
}

/// A top-down 32-bpp DIB section holding a full frame, so the UI thread can push it with
/// `UpdateLayeredWindow` (or paint it) without another full-size copy.
struct Dib {
    bmp: HBITMAP,
    bits: *mut u8,
    w: usize,
    h: usize,
}

// SAFETY: the bitmap is owned by this value only (GDI objects are process-wide); it moves between
// threads under the `Shown` mutex and is never selected into DCs on two threads at once.
unsafe impl Send for Dib {}

fn bmi(w: usize, h: usize) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

impl Dib {
    fn new(w: usize, h: usize) -> io::Result<Self> {
        frame_len(w, h)?;
        if w == 0 || h == 0 || w > i32::MAX as usize || h > i32::MAX as usize {
            return Err(io::Error::other("bad frame size"));
        }
        let info = bmi(w, h);
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let bmp = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }
            .map_err(io_err)?;
        if bits.is_null() {
            unsafe {
                let _ = DeleteObject(bmp.into());
            }
            return Err(io::Error::other("CreateDIBSection returned no bits"));
        }
        Ok(Self {
            bmp,
            bits: bits as *mut u8,
            w,
            h,
        })
    }
    fn px(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.bits, self.w * self.h * 4) }
    }
    fn px_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.w * self.h * 4) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(self.bmp.into());
        }
    }
}

// ── engine (render worker) ──────────────────────────────────────────────────

/// Notifications from the engine (called on the render worker thread; keep the callback short,
/// e.g. post a message to the shell).
#[derive(Clone, Debug)]
pub enum Event {
    /// A wallpaper is shown. `accent`: its dominant colour (applied already when
    /// `Config::dynamic_accent`); `rotated`: chosen by the rotation; `at_ms`: Unix ms (persist it as
    /// `Config::last_rotation_ms` when `rotated`).
    Applied {
        image: Option<PathBuf>,
        accent: Option<Color>,
        /// Material You source colour of the image (the whole theme is generated from it).
        seed: Option<[u8; 3]>,
        rotated: bool,
        at_ms: u64,
    },
    /// The image could not be decoded; the theme background is shown instead.
    Failed { image: PathBuf, error: String },
}

pub type EventSink = Box<dyn Fn(Event) + Send + 'static>;

pub struct Config {
    /// Wallpaper to show first; `None` = next image of the rank folder (if any), else the background.
    pub image: Option<PathBuf>,
    pub accent: Color,
    pub rank: char,
    /// Vignette / empty-wallpaper colour (theme `--bg`).
    pub background: Color,
    /// Rank folders root (`<root>\E` … `<root>\S`), `None` = no folder rotation.
    pub rank_root: Option<PathBuf>,
    /// Rotation period in hours, 0 = off.
    pub rotate_hours: f32,
    /// Unix ms of the last rotation (from the settings).
    pub last_rotation_ms: u64,
    /// Use each new wallpaper's dominant colour as the accent.
    pub dynamic_accent: bool,
    /// Start blurred (a panel is already open).
    pub blurred: bool,
    pub on_event: Option<EventSink>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            image: None,
            accent: DEFAULT_ACCENT,
            rank: 'E',
            background: DEFAULT_BG,
            rank_root: None,
            rotate_hours: 0.0,
            last_rotation_ms: 0,
            dynamic_accent: true,
            blurred: false,
            on_event: None,
        }
    }
}

enum Cmd {
    Image(PathBuf),
    Accent(Color),
    Rank(char),
    Blurred(bool),
    Rotate,
    /// Rotation period in hours (0 = off).
    Hours(f32),
    /// Display size changed, or the clock may be stale (resume, time change).
    Resize,
    Tick,
    Stop,
}

/// What the UI thread shows, handed over by the render worker.
#[derive(Default)]
struct Shown {
    w: usize,
    h: usize,
    /// Full opaque frame: waiting to be pushed (layered window: freed once pushed, DWM keeps the
    /// surface) or kept for WM_PAINT (non-layered window).
    frame: Option<Dib>,
    /// Newer clock rectangle for an already pushed frame: rectangle + opaque BGRA pixels.
    patch: Option<(Crop, Vec<u8>)>,
    /// Frost at 1/8 scale while a panel is open; upscaled (HALFTONE) when presented.
    frost: Option<Small>,
    /// Bumped whenever `frost` changes.
    frost_gen: u64,
}

#[derive(Default)]
struct Work {
    reload: bool,
    rotated: bool,
    clock: bool,
    frost: bool,
    stop: bool,
}

struct Engine {
    wic: IWICImagingFactory,
    image: Option<PathBuf>,
    accent: Color,
    rank: char,
    bg: Color,
    rot: Rotation,
    dynamic_accent: bool,
    blurred: bool,
    on_event: Option<EventSink>,
    scene: Option<Scene>,
    /// The clock is cut to its rectangle, unless its text once overflowed it (then full width).
    wide_clock: bool,
    shown_minute: Option<(u8, u8, u8)>,
    shared: Arc<Mutex<Shown>>,
    host: Arc<AtomicIsize>,
}

impl Engine {
    fn emit(&self, e: Event) {
        if let Some(f) = &self.on_event {
            f(e);
        }
    }

    fn notify(&self) {
        let h = self.host.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(HWND(h as *mut _)), WM_APP_FRAME, WPARAM(0), LPARAM(0));
            }
        }
    }

    /// Pick the next image of the rank folder (see [`Rotation::next`]). False when there is none.
    fn rotate(&mut self, scheduled: bool) -> bool {
        match self
            .rot
            .next(self.rank, self.image.as_deref(), unix_ms(), scheduled)
        {
            Some(p) => {
                self.image = Some(p);
                true
            }
            None => false,
        }
    }

    fn rotation_wait(&self) -> Option<u64> {
        self.rot.wait(unix_ms())
    }

    fn apply(&mut self, c: Cmd, work: &mut Work) {
        match c {
            Cmd::Image(p) => {
                self.image = Some(p);
                work.reload = true;
                work.rotated = false;
            }
            Cmd::Accent(a) => {
                if a != self.accent {
                    self.accent = a;
                    work.clock = true;
                }
            }
            Cmd::Rank(r) => {
                let r = normalize_rank(r);
                if r != self.rank {
                    self.rank = r;
                    work.clock = true;
                    // a rank switch shows that rank's wallpapers (when the current one is elsewhere)
                    let dir = self.rot.root.as_ref().map(|root| images_dir(root, r));
                    let inside = match (&dir, &self.image) {
                        (Some(d), Some(i)) => i.parent().map(|p| same_path(p, d)).unwrap_or(false),
                        _ => true,
                    };
                    if !inside && self.rotate(false) {
                        work.reload = true;
                        work.rotated = true;
                    }
                }
            }
            Cmd::Blurred(b) => {
                if b != self.blurred {
                    self.blurred = b;
                    work.frost = true;
                }
            }
            Cmd::Hours(h) => self.rot.hours = h,
            Cmd::Rotate => {
                if self.rotate(false) {
                    work.reload = true;
                    work.rotated = true;
                }
            }
            Cmd::Resize => work.reload = true,
            Cmd::Tick => work.clock = true,
            Cmd::Stop => work.stop = true,
        }
    }

    fn tick(&mut self, work: &mut Work) {
        if self.rotation_wait() == Some(0) && self.rotate(true) {
            work.reload = true;
            work.rotated = true;
        }
        let t = local_time();
        if self.shown_minute != Some((t.day, t.hour, t.minute)) {
            work.clock = true;
        }
    }

    fn run(&mut self, work: Work) {
        if work.reload || self.scene.is_none() {
            self.reload(work.rotated);
        } else if work.clock && !self.render_clock(None) {
            // the text overflowed the clock rectangle: rebuild at full width
            self.wide_clock = true;
            self.reload(false);
        }
        if work.frost || work.reload || work.clock {
            self.update_frost();
        }
        self.notify();
    }

    fn reload(&mut self, rotated: bool) {
        let (w, h) = screen_size();
        // the previous scene and frame are never needed for the next one: free them first
        self.scene = None;
        if let Ok(mut s) = self.shared.lock() {
            s.frame = None;
            s.patch = None;
        }
        let Ok(mut dib) = Dib::new(w, h) else {
            return;
        };
        let (err, seed) = loop {
            let (scene, seed, err) = build_scene(
                &self.wic,
                w,
                h,
                self.image.as_deref(),
                self.bg,
                dib.px_mut(),
                self.wide_clock,
            );
            if err.is_none() && self.dynamic_accent {
                if let Some(s) = seed {
                    self.accent = color_of(material::Scheme::dark(s).primary);
                }
            }
            // glass backdrop from the picture itself, before the clock is drawn into the frame
            render::set_backdrop(Some(glass_of(dib.px(), w, h)));
            self.scene = Some(scene);
            if self.render_clock(Some(&mut dib)) {
                break (err, seed);
            }
            // the text overflowed its rectangle (unexpected font metrics): use the full width
            self.wide_clock = true;
            self.scene = None;
        };
        match (err, &self.image) {
            (Some(e), Some(p)) => self.emit(Event::Failed {
                image: p.clone(),
                error: e.to_string(),
            }),
            _ => {
                if let Some(p) = &self.image {
                    set_system_wallpaper(p);
                }
                self.emit(Event::Applied {
                    image: self.image.clone(),
                    accent: seed.map(|s| color_of(material::Scheme::dark(s).primary)),
                    seed,
                    rotated,
                    at_ms: unix_ms(),
                })
            }
        }
        if let Ok(mut s) = self.shared.lock() {
            s.w = w;
            s.h = h;
            s.frame = Some(dib);
            s.patch = None;
        }
    }

    /// Recompose the clock rectangle into `into`, or hand it to the UI thread. False (nothing
    /// changed) when the text overflowed the rectangle.
    fn render_clock(&mut self, into: Option<&mut Dib>) -> bool {
        let Some(scene) = self.scene.as_mut() else {
            return true;
        };
        let t = local_time();
        let l = clock_layout(scene.w, scene.h, self.accent, self.rank, &t);
        let r = scene.rect;
        let text = text_layer(&l, scene.w, r).unwrap_or_default();
        if !self.wide_clock && ink_clipped(&text, r, scene.w) {
            return false;
        }
        let band = clock_band(scene, &l, &text, self.accent);
        drop(text);
        shrink_into(&mut scene.small, &band, r.w(), r.h(), r.x0, r.y0);
        match into {
            Some(d) => paste_rect(d.px_mut(), scene.w, r, &band),
            None => {
                if let Ok(mut g) = self.shared.lock() {
                    let s = &mut *g;
                    match s.frame.as_mut() {
                        Some(f) if f.w == scene.w && f.h == scene.h => {
                            paste_rect(f.px_mut(), scene.w, r, &band)
                        }
                        _ => s.patch = Some((r, band)),
                    }
                }
            }
        }
        self.shown_minute = Some((t.day, t.hour, t.minute));
        true
    }

    fn update_frost(&mut self) {
        let next = match (&self.scene, self.blurred) {
            (Some(sc), true) => Some(frost_of(&sc.small)),
            _ => None,
        };
        if let Ok(mut s) = self.shared.lock() {
            if next.is_some() || s.frost.is_some() {
                s.frost = next;
                s.frost_gen = s.frost_gen.wrapping_add(1);
            }
        }
    }
}

fn worker(cfg: Config, rx: Receiver<Cmd>, shared: Arc<Mutex<Shown>>, host: Arc<AtomicIsize>) {
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let Ok(wic) = wic_factory() else {
        if com.is_ok() {
            unsafe { CoUninitialize() };
        }
        return;
    };
    let mut e = Engine {
        wic,
        image: cfg.image,
        accent: cfg.accent,
        rank: normalize_rank(cfg.rank),
        bg: cfg.background,
        rot: Rotation {
            root: cfg.rank_root,
            hours: cfg.rotate_hours,
            last: cfg.last_rotation_ms,
        },
        dynamic_accent: cfg.dynamic_accent,
        blurred: cfg.blurred,
        on_event: cfg.on_event,
        scene: None,
        wide_clock: false,
        shown_minute: None,
        shared,
        host,
    };
    let mut first = Work {
        reload: true,
        ..Work::default()
    };
    if e.image.is_none() || e.rotation_wait() == Some(0) {
        first.rotated = e.rotate(true);
    }
    e.run(first);
    loop {
        let t = local_time();
        let mut wait = ms_to_next_minute(t.second, t.millis) + 25;
        if let Some(r) = e.rotation_wait() {
            wait = wait.min(r.max(MIN_ROTATION_WAIT_MS));
        }
        let mut work = Work::default();
        match rx.recv_timeout(Duration::from_millis(wait)) {
            Ok(c) => e.apply(c, &mut work),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        while let Ok(c) = rx.try_recv() {
            e.apply(c, &mut work);
        }
        if work.stop {
            break;
        }
        // a command may land just after the minute boundary: never skip a minute
        e.tick(&mut work);
        if work.reload || work.clock || work.frost {
            e.run(work);
        }
    }
    drop(e);
    render::shutdown();
    if com.is_ok() {
        unsafe { CoUninitialize() };
    }
}

// ── UI thread: the window behind the desktop icons ──────────────────────────

const WM_APP_FRAME: u32 = WM_APP + 0x31;
const WM_APP_STOP: u32 = WM_APP + 0x32;
const TIMER_ATTACH: usize = 1;
const ATTACH_RETRIES: u32 = 60; // × 500 ms after an Explorer restart
const HOST_CLASS: PCWSTR = w!("MonarchNextWallpaperHost");
const WALL_CLASS: PCWSTR = w!("MonarchNextWallpaper");
/// Opaque push: constant alpha only (the frames are opaque; GDI stretching leaves alpha undefined).
const BLEND_OPAQUE: BLENDFUNCTION = BLENDFUNCTION {
    BlendOp: 0,
    BlendFlags: 0,
    SourceConstantAlpha: 255,
    AlphaFormat: 0,
};

struct UiCtx {
    shared: Arc<Mutex<Shown>>,
    tx: Sender<Cmd>,
    attached: Arc<AtomicBool>,
    bg: Color,
}

thread_local! {
    static CTX: RefCell<Option<Rc<UiCtx>>> = const { RefCell::new(None) };
    static HOST: Cell<isize> = const { Cell::new(0) };
    static WALL: Cell<isize> = const { Cell::new(0) };
    /// Frost window above WALL while a panel is open (pushed path only).
    static FROST: Cell<isize> = const { Cell::new(0) };
    /// `Shown::frost_gen` last presented.
    static FROST_GEN: Cell<u64> = const { Cell::new(0) };
    /// WALL is a layered window fed with UpdateLayeredWindow (frames are freed once pushed).
    static PUSH: Cell<bool> = const { Cell::new(false) };
    /// A push failed once: keep frames and paint them (WM_PAINT) from then on.
    static NO_PUSH: Cell<bool> = const { Cell::new(false) };
    static STOPPING: Cell<bool> = const { Cell::new(false) };
    static RETRIES: Cell<u32> = const { Cell::new(0) };
    static TASKBAR_CREATED: Cell<u32> = const { Cell::new(0) };
}

fn ctx() -> Option<Rc<UiCtx>> {
    CTX.with(|c| c.borrow().clone())
}

fn hwnd(v: isize) -> HWND {
    HWND(v as *mut _)
}

/// Desktop host: (parent, icon view to sit below). 24H2+: (Progman, Some(SHELLDLL_DefView));
/// older builds: (WorkerW behind the icons, None).
unsafe fn desktop_host() -> Option<(HWND, Option<HWND>)> {
    let progman = FindWindowW(w!("Progman"), PCWSTR::null()).ok()?;
    let mut r = 0usize;
    // ask Explorer to create the WorkerW behind the icons (what every wallpaper engine does)
    let _ = SendMessageTimeoutW(
        progman,
        0x052C,
        WPARAM(0xD),
        LPARAM(0x1),
        SMTO_NORMAL,
        1000,
        Some(&mut r),
    );
    let _ = SendMessageTimeoutW(
        progman,
        0x052C,
        WPARAM(0),
        LPARAM(0),
        SMTO_NORMAL,
        1000,
        Some(&mut r),
    );
    if FindWindowExW(Some(progman), None, w!("WorkerW"), PCWSTR::null()).is_ok() {
        let defview =
            FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), PCWSTR::null()).ok();
        return Some((progman, defview));
    }
    let mut prev: Option<HWND> = None;
    for _ in 0..256 {
        let Ok(wk) = FindWindowExW(None, prev, w!("WorkerW"), PCWSTR::null()) else {
            break;
        };
        if FindWindowExW(Some(wk), None, w!("SHELLDLL_DefView"), PCWSTR::null()).is_ok() {
            return FindWindowExW(None, Some(wk), w!("WorkerW"), PCWSTR::null())
                .ok()
                .map(|x| (x, None));
        }
        prev = Some(wk);
    }
    None
}

/// Attach (or confirm) the wallpaper window. Never called while holding a RefCell borrow.
unsafe fn try_attach() -> bool {
    let Some(c) = ctx() else { return false };
    let cur = WALL.with(Cell::get);
    if cur != 0 {
        let wall = hwnd(cur);
        let parent_ok = GetParent(wall)
            .map(|p| IsWindow(Some(p)).as_bool())
            .unwrap_or(false);
        if IsWindow(Some(wall)).as_bool() && parent_ok {
            return true;
        }
        WALL.with(|x| x.set(0)); // stale: no re-attach scheduling from its WM_DESTROY
        let _ = DestroyWindow(wall);
    }
    destroy_frost();
    c.attached.store(false, Ordering::SeqCst);
    let Some((host, defview)) = desktop_host() else {
        return false;
    };
    let Ok(module) = GetModuleHandleW(None) else {
        return false;
    };
    let hinst = HINSTANCE(module.0);
    let (w, h) = match c.shared.lock() {
        Ok(s) if s.w > 0 => (s.w as i32, s.h as i32),
        _ => {
            let (w, h) = screen_size();
            (w as i32, h as i32)
        }
    };
    // primary monitor origin in the host's client coordinates (hosts span the virtual screen)
    let mut origin = POINT::default();
    let _ = ScreenToClient(host, &mut origin);
    let ex = if defview.is_some() {
        WS_EX_NOACTIVATE | WS_EX_LAYERED
    } else {
        WS_EX_NOACTIVATE
    };
    let push = defview.is_some() && !NO_PUSH.with(Cell::get);
    let Ok(wall) = CreateWindowExW(
        ex,
        WALL_CLASS,
        w!("Monarch Next Wallpaper"),
        WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
        origin.x,
        origin.y,
        w,
        h,
        Some(host),
        None,
        Some(hinst),
        None,
    ) else {
        return false;
    };
    if let Some(dv) = defview {
        // 24H2: Progman has no redirection bitmap, its children must be layered to be composed.
        // Pushed path: UpdateLayeredWindow (DWM keeps the frame); fallback: constant alpha + paint.
        if !push {
            let _ = SetLayeredWindowAttributes(wall, COLORREF(0), 255, LWA_ALPHA);
        }
        let _ = SetWindowPos(
            wall,
            Some(dv),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
    WALL.with(|x| x.set(wall.0 as isize));
    PUSH.with(|p| p.set(push));
    FROST_GEN.with(|g| g.set(u64::MAX)); // present the frost again, if any
    c.attached.store(true, Ordering::SeqCst);
    // a frame already pushed to a previous window is gone: render a new one
    let lost = c
        .shared
        .lock()
        .map(|s| s.w > 0 && s.frame.is_none())
        .unwrap_or(false);
    if lost {
        let _ = c.tx.send(Cmd::Resize);
    }
    if push {
        let _ = PostMessageW(
            Some(hwnd(HOST.with(Cell::get))),
            WM_APP_FRAME,
            WPARAM(0),
            LPARAM(0),
        );
    } else {
        let _ = InvalidateRect(Some(wall), None, false);
    }
    true
}

unsafe fn schedule_attach() {
    let host = HOST.with(Cell::get);
    if host == 0 || STOPPING.with(Cell::get) {
        return;
    }
    RETRIES.with(|r| r.set(ATTACH_RETRIES));
    SetTimer(Some(hwnd(host)), TIMER_ATTACH, 500, None);
}

unsafe fn destroy_frost() {
    let f = FROST.with(|x| x.replace(0));
    if f != 0 {
        let _ = DestroyWindow(hwnd(f));
    }
}

/// Destroy our window and let Explorer repaint its own wallpaper.
unsafe fn detach() {
    destroy_frost();
    let cur = WALL.with(|x| x.replace(0));
    if cur == 0 {
        return;
    }
    let wall = hwnd(cur);
    let parent = GetParent(wall).ok();
    let _ = DestroyWindow(wall);
    if let Some(p) = parent.filter(|p| IsWindow(Some(*p)).as_bool()) {
        let _ = RedrawWindow(
            Some(p),
            None,
            None,
            RDW_INVALIDATE | RDW_ERASE | RDW_FRAME | RDW_ALLCHILDREN,
        );
    }
    if let Some(c) = ctx() {
        c.attached.store(false, Ordering::SeqCst);
    }
}

/// Run `f` with `bmp` selected into a memory DC compatible with the screen.
unsafe fn with_mem_dc(bmp: HBITMAP, f: impl FnOnce(HDC, HDC) -> bool) -> bool {
    let screen = GetDC(None);
    let mem = CreateCompatibleDC(Some(screen));
    let old = SelectObject(mem, bmp.into());
    let r = f(screen, mem);
    SelectObject(mem, old);
    let _ = DeleteDC(mem);
    ReleaseDC(None, screen);
    r
}

/// Push a whole frame to a layered window (it keeps its current position).
unsafe fn push_full(h: HWND, d: &Dib) -> bool {
    with_mem_dc(d.bmp, |screen, mem| {
        let size = SIZE {
            cx: d.w as i32,
            cy: d.h as i32,
        };
        let src = POINT::default();
        UpdateLayeredWindow(
            h,
            Some(screen),
            None,
            Some(&size),
            Some(mem),
            Some(&src),
            COLORREF(0),
            Some(&BLEND_OPAQUE),
            ULW_ALPHA,
        )
        .is_ok()
    })
}

/// Push only rectangle `r` of a `w × h` layered window (the rest of its surface stays).
unsafe fn push_patch(h: HWND, w: usize, ht: usize, r: Crop, px: &[u8]) -> bool {
    if px.len() < r.len() || r.w() == 0 || r.h() == 0 {
        return true;
    }
    let Ok(mut d) = Dib::new(r.w(), r.h()) else {
        return false;
    };
    d.px_mut().copy_from_slice(&px[..r.len()]);
    with_mem_dc(d.bmp, |screen, mem| {
        let size = SIZE {
            cx: w as i32,
            cy: ht as i32,
        };
        // window pixel (x, y) reads source (x - x0, y - y0): the small DIB holds only the rectangle
        let src = POINT {
            x: -(r.x0 as i32),
            y: -(r.y0 as i32),
        };
        let dirty = RECT {
            left: r.x0 as i32,
            top: r.y0 as i32,
            right: r.x1 as i32,
            bottom: r.y1 as i32,
        };
        let info = UPDATELAYEREDWINDOWINFO {
            cbSize: std::mem::size_of::<UPDATELAYEREDWINDOWINFO>() as u32,
            hdcDst: screen,
            pptDst: std::ptr::null(),
            psize: &size,
            hdcSrc: mem,
            pptSrc: &src,
            crKey: COLORREF(0),
            pblend: &BLEND_OPAQUE,
            dwFlags: ULW_ALPHA,
            prcDirty: &dirty,
        };
        UpdateLayeredWindowIndirect(h, &info).as_bool()
    })
}

/// Upscale the 1/8 frost to `w × h` on `dc` (GDI HALFTONE ≈ smooth bilinear).
unsafe fn stretch_small(dc: HDC, s: &Small, w: usize, h: usize) {
    if s.w == 0 || s.h == 0 || s.px.len() < s.w * s.h * 4 {
        return;
    }
    SetStretchBltMode(dc, HALFTONE);
    let _ = SetBrushOrgEx(dc, 0, 0, None);
    StretchDIBits(
        dc,
        0,
        0,
        w as i32,
        h as i32,
        0,
        0,
        s.w as i32,
        s.h as i32,
        Some(s.px.as_ptr() as *const _),
        &bmi(s.w, s.h),
        DIB_RGB_COLORS,
        SRCCOPY,
    );
}

/// Show the frost in its own layered window right above the sharp one (pushed path): closing the
/// panel only destroys it, the sharp frame underneath stays current.
unsafe fn present_frost(wall: HWND, s: &Small, w: usize, h: usize) {
    let Ok(d) = Dib::new(w, h) else { return };
    with_mem_dc(d.bmp, |_, mem| {
        stretch_small(mem, s, w, h);
        true
    });
    let mut f = hwnd(FROST.with(Cell::get));
    if f.is_invalid() || !IsWindow(Some(f)).as_bool() {
        let Ok(parent) = GetParent(wall) else { return };
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let mut wr = RECT::default();
        let _ = GetWindowRect(wall, &mut wr);
        let mut at = POINT {
            x: wr.left,
            y: wr.top,
        };
        let _ = ScreenToClient(parent, &mut at);
        let Ok(nf) = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_LAYERED,
            WALL_CLASS,
            w!("Monarch Next Wallpaper Frost"),
            WS_CHILD | WS_CLIPSIBLINGS,
            at.x,
            at.y,
            w as i32,
            h as i32,
            Some(parent),
            None,
            Some(HINSTANCE(module.0)),
            None,
        ) else {
            return;
        };
        // directly above the sharp window (below the icons)
        let above = GetWindow(wall, GW_HWNDPREV).ok();
        let _ = SetWindowPos(
            nf,
            Some(above.unwrap_or(HWND_TOP)),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
        FROST.with(|x| x.set(nf.0 as isize));
        f = nf;
    }
    if push_full(f, &d) {
        let _ = ShowWindow(f, SW_SHOWNA);
    }
}

/// A new frame / clock rectangle / frost is ready (WM_APP_FRAME).
unsafe fn on_frame() {
    let cur = WALL.with(Cell::get);
    let Some(c) = ctx() else { return };
    if cur == 0 {
        return;
    }
    let wall = hwnd(cur);
    if !PUSH.with(Cell::get) {
        if let Ok((w, ht)) = c.shared.lock().map(|s| (s.w, s.h)) {
            let mut r = RECT::default();
            let _ = GetClientRect(wall, &mut r);
            if w > 0 && (r.right as usize != w || r.bottom as usize != ht) {
                let _ = SetWindowPos(
                    wall,
                    None,
                    0,
                    0,
                    w as i32,
                    ht as i32,
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        let _ = InvalidateRect(Some(wall), None, false);
        return;
    }
    let shown_gen = FROST_GEN.with(Cell::get);
    let Ok((frame, patch, frost, gen, w, h)) = c.shared.lock().map(|mut s| {
        let gen = s.frost_gen;
        let frost = (gen != shown_gen).then(|| s.frost.clone());
        (s.frame.take(), s.patch.take(), frost, gen, s.w, s.h)
    }) else {
        return;
    };
    let mut ok = true;
    if let Some(f) = &frame {
        ok = push_full(wall, f);
    }
    if ok {
        if let Some((r, px)) = &patch {
            ok = push_patch(wall, w, h, *r, px);
        }
    }
    if !ok {
        // this desktop does not take pushed frames: recreate the window in painted mode
        NO_PUSH.with(|x| x.set(true));
        if let (Some(f), Ok(mut s)) = (frame, c.shared.lock()) {
            if s.frame.is_none() {
                s.frame = Some(f);
            }
        }
        WALL.with(|x| x.set(0));
        let _ = DestroyWindow(wall);
        if !try_attach() {
            schedule_attach();
        }
        return;
    }
    drop(frame); // pushed: DWM holds the pixels now
    if let Some(next) = frost {
        FROST_GEN.with(|g| g.set(gen));
        match next {
            Some(s) => present_frost(wall, &s, w, h),
            None => destroy_frost(),
        }
    }
}

unsafe fn paint(h: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(h, &mut ps);
    let mut painted = false;
    if let Some(c) = ctx() {
        if let Ok(s) = c.shared.lock() {
            if let Some(fr) = &s.frost {
                stretch_small(dc, fr, s.w, s.h);
                painted = true;
            } else if let Some(f) = s.frame.as_ref().filter(|f| f.w == s.w && f.h == s.h) {
                SetDIBitsToDevice(
                    dc,
                    0,
                    0,
                    f.w as u32,
                    f.h as u32,
                    0,
                    0,
                    0,
                    f.h as u32,
                    f.px().as_ptr() as *const _,
                    &bmi(f.w, f.h),
                    DIB_RGB_COLORS,
                );
                painted = true;
            }
        }
        if !painted {
            let k = c.bg;
            let brush = CreateSolidBrush(COLORREF(
                (k.r * 255.0) as u32 | ((k.g * 255.0) as u32) << 8 | ((k.b * 255.0) as u32) << 16,
            ));
            FillRect(dc, &ps.rcPaint, brush);
            let _ = DeleteObject(brush.into());
        }
    }
    let _ = EndPaint(h, &ps);
}

unsafe extern "system" fn wall_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(h);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DESTROY => {
            if FROST.with(Cell::get) == h.0 as isize {
                FROST.with(|x| x.set(0));
            }
            // destroyed under us (Explorer exited): come back when the desktop does
            if WALL.with(Cell::get) == h.0 as isize {
                WALL.with(|x| x.set(0));
                if let Some(c) = ctx() {
                    c.attached.store(false, Ordering::SeqCst);
                }
                schedule_attach();
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(h, msg, wp, lp),
    }
}

unsafe extern "system" fn host_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let send = |c: Cmd| {
        if let Some(x) = ctx() {
            let _ = x.tx.send(c);
        }
    };
    if msg != 0 && msg == TASKBAR_CREATED.with(Cell::get) {
        // Explorer (re)started
        if !try_attach() {
            schedule_attach();
        }
        return LRESULT(0);
    }
    match msg {
        WM_TIMER if wp.0 == TIMER_ATTACH => {
            let left = RETRIES.with(|r| {
                let v = r.get().saturating_sub(1);
                r.set(v);
                v
            });
            if STOPPING.with(Cell::get) || try_attach() || left == 0 {
                let _ = KillTimer(Some(h), TIMER_ATTACH);
            }
            LRESULT(0)
        }
        WM_APP_FRAME => {
            on_frame();
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            send(Cmd::Resize);
            // the frost is rebuilt at the new size with the next frame
            destroy_frost();
            FROST_GEN.with(|g| g.set(u64::MAX));
            if let Some(cur) = Some(WALL.with(Cell::get)).filter(|v| *v != 0) {
                // the primary monitor may have moved inside the virtual screen
                if let Ok(p) = GetParent(hwnd(cur)) {
                    let mut origin = POINT::default();
                    let _ = ScreenToClient(p, &mut origin);
                    let _ = SetWindowPos(
                        hwnd(cur),
                        None,
                        origin.x,
                        origin.y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            LRESULT(0)
        }
        WM_TIMECHANGE => {
            send(Cmd::Tick);
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            // PBT_APMRESUMESUSPEND / PBT_APMRESUMEAUTOMATIC: the minute timer slept with us
            if wp.0 == 0x7 || wp.0 == 0x12 {
                send(Cmd::Tick);
            }
            LRESULT(1)
        }
        WM_APP_STOP => {
            STOPPING.with(|s| s.set(true));
            let _ = KillTimer(Some(h), TIMER_ATTACH);
            detach();
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

fn ui_thread(ctx_in: UiCtx, host_out: Arc<AtomicIsize>, stopping: Arc<AtomicBool>) {
    CTX.with(|c| *c.borrow_mut() = Some(Rc::new(ctx_in)));
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else {
            return;
        };
        let hinst = HINSTANCE(module.0);
        for (class, proc_) in [
            (
                HOST_CLASS,
                host_proc as unsafe extern "system" fn(_, _, _, _) -> _,
            ),
            (WALL_CLASS, wall_proc),
        ] {
            let wc = WNDCLASSW {
                lpfnWndProc: Some(proc_),
                hInstance: hinst,
                lpszClassName: class,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            };
            RegisterClassW(&wc); // fails harmlessly when already registered (restart)
        }
        TASKBAR_CREATED.with(|t| t.set(RegisterWindowMessageW(w!("TaskbarCreated"))));
        // hidden top-level window: receives TaskbarCreated / display / power broadcasts
        let Ok(host) = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            HOST_CLASS,
            w!("Monarch Next Wallpaper Host"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst),
            None,
        ) else {
            return;
        };
        HOST.with(|x| x.set(host.0 as isize));
        host_out.store(host.0 as isize, Ordering::SeqCst);
        if stopping.load(Ordering::SeqCst) {
            let _ = PostMessageW(Some(host), WM_APP_STOP, WPARAM(0), LPARAM(0));
        } else if !try_attach() {
            schedule_attach();
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    host_out.store(0, Ordering::SeqCst);
    CTX.with(|c| c.borrow_mut().take());
}

// ── public handle ───────────────────────────────────────────────────────────

/// Running wallpaper engine. Every call is non-blocking except [`Handle::stop`] (joins threads).
pub struct Handle {
    tx: Sender<Cmd>,
    host: Arc<AtomicIsize>,
    stopping: Arc<AtomicBool>,
    attached: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

/// Start the engine: a render worker and a UI thread that attaches the wallpaper window behind the
/// desktop icons. This changes the live desktop; use [`snapshot_wallpaper`] for previews.
pub fn start(config: Config) -> Handle {
    let (tx, rx) = mpsc::channel();
    let shared = Arc::new(Mutex::new(Shown::default()));
    let host = Arc::new(AtomicIsize::new(0));
    let stopping = Arc::new(AtomicBool::new(false));
    let attached = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    let ui = UiCtx {
        shared: shared.clone(),
        tx: tx.clone(),
        attached: attached.clone(),
        bg: config.background,
    };
    let (h2, s2) = (host.clone(), stopping.clone());
    if let Ok(t) = std::thread::Builder::new()
        .name("wallpaper-ui".into())
        .spawn(move || ui_thread(ui, h2, s2))
    {
        threads.push(t);
    }
    let h3 = host.clone();
    if let Ok(t) = std::thread::Builder::new()
        .name("wallpaper-render".into())
        .spawn(move || worker(config, rx, shared, h3))
    {
        threads.push(t);
    }
    Handle {
        tx,
        host,
        stopping,
        attached,
        threads,
    }
}

impl Handle {
    pub fn set_image(&self, path: impl Into<PathBuf>) {
        let _ = self.tx.send(Cmd::Image(path.into()));
    }
    pub fn set_accent(&self, accent: Color) {
        let _ = self.tx.send(Cmd::Accent(accent));
    }
    /// Rank letter (E…S). With a rank root, switches to that rank's folder when the current image
    /// lives elsewhere.
    pub fn set_rank(&self, rank: char) {
        let _ = self.tx.send(Cmd::Rank(rank));
    }
    /// Frosted + dimmed frame while a panel is open.
    pub fn set_blurred(&self, blurred: bool) {
        let _ = self.tx.send(Cmd::Blurred(blurred));
    }
    /// Rotation period in hours; 0 turns folder rotation off.
    pub fn set_rotation(&self, hours: f32) {
        let _ = self.tx.send(Cmd::Hours(hours));
    }
    /// Next image of the current rank folder now (also resets the rotation period).
    pub fn rotate_now(&self) {
        let _ = self.tx.send(Cmd::Rotate);
    }
    /// The window currently sits behind the desktop icons.
    pub fn is_attached(&self) -> bool {
        self.attached.load(Ordering::SeqCst)
    }
    /// Detach the window (Explorer repaints its own wallpaper) and end both threads.
    pub fn stop(mut self) {
        self.shutdown();
    }
    fn shutdown(&mut self) {
        if self.threads.is_empty() {
            return;
        }
        let _ = self.tx.send(Cmd::Stop);
        self.stopping.store(true, Ordering::SeqCst);
        let h = self.host.load(Ordering::SeqCst);
        if h != 0 {
            unsafe {
                let _ = PostMessageW(Some(hwnd(h)), WM_APP_STOP, WPARAM(0), LPARAM(0));
            }
        }
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band_of(w: usize, h: usize) -> (usize, usize) {
        let l = clock_layout(w, h, DEFAULT_ACCENT, 'E', &LocalTime::default());
        (l.y0, l.y1)
    }

    unsafe extern "system" fn test_proc(h: HWND, m: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        DefWindowProcW(h, m, wp, lp)
    }

    fn solid(w: usize, h: usize, rgb: [u8; 3]) -> Vec<u8> {
        [rgb[0], rgb[1], rgb[2], 255].repeat(w * h)
    }

    #[test]
    fn accent_of_vivid_red() {
        let px = solid(32, 20, [230, 20, 30]);
        let c = accent_of(&px, 32, 20).expect("vivid");
        assert!(c.r > 0.9 && c.g < 0.35 && c.b < 0.4, "{c:?}");
        let (_, s, l) = hsl(c.r as f64, c.g as f64, c.b as f64);
        assert!((0.62 - 0.01..=0.70 + 0.01).contains(&l), "l={l}");
        assert!(s >= 0.64, "s={s}");
    }

    #[test]
    fn accent_of_grey_and_dark_is_none() {
        assert_eq!(accent_of(&solid(64, 40, [128, 128, 128]), 64, 40), None);
        assert_eq!(accent_of(&solid(64, 40, [10, 12, 30]), 64, 40), None);
        assert_eq!(accent_of(&[0u8; 8], 64, 40), None); // short buffer
        assert_eq!(accent_of(&[], 0, 0), None);
    }

    #[test]
    fn accent_of_dominant_hue_wins() {
        // 3/4 vivid blue, 1/4 vivid orange, large image (grid sampling path)
        let (w, h) = (400, 300);
        let mut px = Vec::with_capacity(w * h * 4);
        for _y in 0..h {
            for x in 0..w {
                let c = if x < w * 3 / 4 {
                    [30, 110, 240]
                } else {
                    [240, 140, 20]
                };
                px.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        let c = accent_of(&px, w as u32, h as u32).expect("vivid");
        assert!(c.b > c.r && c.b > c.g, "{c:?}");
        // BGRA path gives the same answer
        let mut bgra = px.clone();
        for p in bgra.as_chunks_mut::<4>().0 {
            p.swap(0, 2);
        }
        assert_eq!(accent_from(&bgra, w, h, true), Some(c));
    }

    #[test]
    fn minute_alignment() {
        assert_eq!(ms_to_next_minute(0, 0), 60_000);
        assert_eq!(ms_to_next_minute(59, 999), 1);
        assert_eq!(ms_to_next_minute(30, 500), 29_500);
        assert_eq!(ms_to_next_minute(61, 5000), 1); // out-of-range input stays positive
    }

    #[test]
    fn clock_text() {
        let t = |hour, minute| LocalTime {
            hour,
            minute,
            ..Default::default()
        };
        assert_eq!(clock_hhmm(&t(0, 5)), "12:05");
        assert_eq!(clock_hhmm(&t(9, 30)), "09:30");
        assert_eq!(clock_hhmm(&t(12, 0)), "12:00");
        assert_eq!(clock_hhmm(&t(23, 59)), "11:59");
        let d = LocalTime {
            weekday: 4,
            day: 16,
            month: 10,
            ..Default::default()
        };
        assert_eq!(arabic_date(&d), "الخميس، ١٦ أكتوبر");
        assert_eq!(kicker('s'), "SOLO LEVELING · RANK S");
        assert_eq!(kicker('x'), "SOLO LEVELING · RANK E");
    }

    #[test]
    fn rotation_index() {
        assert_eq!(next_index(None, 0), None);
        assert_eq!(next_index(None, 3), Some(0));
        assert_eq!(next_index(Some(0), 3), Some(1));
        assert_eq!(next_index(Some(2), 3), Some(0));
        assert_eq!(next_index(Some(7), 3), Some(0));
        let list: Vec<PathBuf> = ["a.jpg", "b.png", "c.webp"]
            .iter()
            .map(|n| Path::new(r"C:\W\E").join(n))
            .collect();
        let next = |c: Option<&str>| next_in_list(&list, c.map(Path::new)).cloned();
        assert_eq!(next(None), Some(list[0].clone()));
        assert_eq!(next(Some(r"c:\w\e\A.JPG")), Some(list[1].clone()));
        assert_eq!(next(Some(r"C:\W\E\c.webp")), Some(list[0].clone()));
        assert_eq!(next(Some(r"C:\other.jpg")), Some(list[0].clone()));
        assert_eq!(next_in_list(&[], None), None);
    }

    #[test]
    fn rotation_timing() {
        assert_eq!(rotation_wait_ms(0, 1_000, 0.0), None);
        assert_eq!(rotation_wait_ms(0, 1_000, f32::NAN), None);
        let h = 3_600_000;
        assert_eq!(rotation_wait_ms(0, 10 * h, 1.0), Some(0));
        assert_eq!(rotation_wait_ms(10 * h, 10 * h + 1000, 1.0), Some(h - 1000));
        assert_eq!(rotation_wait_ms(20 * h, 10 * h, 1.0), Some(h)); // clock went back
    }

    #[test]
    fn image_names_and_folders() {
        assert!(is_image_name("Igris.JPG"));
        assert!(is_image_name("a.webp"));
        assert!(!is_image_name("igris_depth.png"));
        assert!(!is_image_name("notes.txt"));
        assert!(!is_image_name("png"));
        assert_eq!(rank_dir(Path::new(r"C:\R"), 'b'), Path::new(r"C:\R\B"));
        assert_eq!(rank_dir(Path::new(r"C:\R"), '?'), Path::new(r"C:\R\E"));

        let dir = std::env::temp_dir().join(format!("monarch-wall-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        for n in ["b.png", "A.jpg", "a_depth.png", "c.txt", "A_depth.png"] {
            std::fs::write(dir.join(n), b"x").expect("write");
        }
        let imgs = list_images(&dir);
        let names: Vec<String> = imgs
            .iter()
            .map(|p| {
                p.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        assert_eq!(names, ["A.jpg", "b.png"]);
        assert!(depth_for(&dir.join("A.jpg")).is_some());
        assert!(depth_for(&dir.join("b.png")).is_none());
        assert_eq!(next_in_folder(&dir, Some(&imgs[1])), Some(imgs[0].clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blur_preserves_mass_and_spreads() {
        let (w, h) = (41, 41);
        let mut m = vec![0f32; w * h];
        m[20 * w + 20] = 1.0;
        let b = blur_mask(&m, w, h, 3.0);
        let sum: f32 = b.iter().sum();
        assert!((sum - 1.0).abs() < 1e-3, "sum={sum}");
        assert!(b[20 * w + 20] < 0.2 && b[20 * w + 23] > 0.0);
    }

    #[test]
    fn vignette_darkens_corners_only() {
        let (w, h) = (64, 40);
        let mut px = solid(w, h, [200, 200, 200]);
        finish_rows(&mut px, w, h, 0, [0.0; 3]);
        let at = |x: usize, y: usize| px[(y * w + x) * 4] as i32;
        assert!((at(32, 20) - 200).abs() <= 7, "centre {}", at(32, 20));
        assert!(at(0, 0) < 120, "corner {}", at(0, 0));
    }

    #[test]
    fn frost_is_small_and_dims() {
        let (w, h) = (50, 30);
        let px = solid(w, h, [100, 100, 100]);
        let mut small = Small::for_frame(w, h);
        assert_eq!((small.w, small.h), (7, 4));
        shrink_into(&mut small, &px, w, h, 0, 0);
        assert!(small.px.as_chunks::<4>().0.iter().all(|p| p[0] == 100));
        let f = frost_of(&small);
        assert_eq!(f.px.len(), 7 * 4 * 4);
        assert!(f
            .px
            .as_chunks::<4>()
            .0
            .iter()
            .all(|p| (68..=72).contains(&p[0]) && p[3] == 255));
    }

    #[test]
    fn shrink_patches_only_the_rectangle_cells() {
        let (w, h) = (64, 40);
        let mut small = Small::for_frame(w, h);
        shrink_into(&mut small, &solid(w, h, [10, 10, 10]), w, h, 0, 0);
        // a 16×8 rectangle at (24, 16) turns white
        let r = Crop {
            x0: 24,
            y0: 16,
            x1: 40,
            y1: 24,
        };
        shrink_into(
            &mut small,
            &solid(r.w(), r.h(), [250, 250, 250]),
            r.w(),
            r.h(),
            24,
            16,
        );
        let at = |x: usize, y: usize| small.px[(y * small.w + x) * 4];
        assert_eq!(
            (at(3, 2), at(4, 2), at(2, 2), at(5, 2), at(3, 1)),
            (250, 250, 10, 10, 10)
        );
    }

    #[test]
    fn rect_copy_round_trip_and_clip_detection() {
        let (w, h) = (32, 16);
        let frame: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let r = Crop {
            x0: 8,
            y0: 8,
            x1: 24,
            y1: 16,
        };
        let part = copy_rect(&frame, w, r);
        assert_eq!(part.len(), r.len());
        let mut other = vec![0u8; frame.len()];
        paste_rect(&mut other, w, r, &part);
        assert_eq!(copy_rect(&other, w, r), part);
        let mut text = vec![0u8; r.len()];
        assert!(!ink_clipped(&text, r, w));
        text[(3 * r.w() + 5) * 4 + 3] = 200; // inner ink
        assert!(!ink_clipped(&text, r, w));
        text[(3 * r.w()) * 4 + 3] = 200; // ink on the cut left edge
        assert!(ink_clipped(&text, r, w));
        let full = Crop { x0: 0, x1: w, ..r };
        let t = vec![255u8; full.len()];
        assert!(!ink_clipped(&t, full, w)); // frame edges are not cuts
    }

    /// Clock rectangle vs the old full-width band, and the bytes kept at 1080p (printed with
    /// `--nocapture`).
    #[test]
    fn clock_crop_is_narrow_and_holds_the_text() {
        let (w, h) = (1920, 1080);
        let band = band_of(w, h);
        let r = clock_crop(w, h, false);
        assert!(r.x0.is_multiple_of(FROST_SCALE) && r.y0.is_multiple_of(FROST_SCALE));
        assert!(r.x1 <= w && r.y1 <= h && r.y0 <= band.0 && r.y1 >= band.1);
        let old_band = (band.1 - band.0) * w * 4;
        println!(
            "1080p clock: old band {}x{} = {} B ×2 (base+depth); new rect {}x{} = {} B",
            w,
            band.1 - band.0,
            old_band,
            r.w(),
            r.h(),
            r.len()
        );
        assert!(r.w() < w * 6 / 10, "rect {r:?}");
        // the widest text renders without touching the cut edges
        let t = LocalTime {
            weekday: 3,
            day: 28,
            month: 9,
            hour: 10,
            minute: 58,
            ..Default::default()
        };
        let l = clock_layout(w, h, DEFAULT_ACCENT, 'S', &t);
        let text = text_layer(&l, w, r).expect("text layer");
        assert!(text.iter().skip(3).step_by(4).any(|a| *a > 0), "no ink");
        assert!(!ink_clipped(&text, r, w));
    }

    #[test]
    fn rotation_backs_off_when_folder_is_empty() {
        let root = std::env::temp_dir().join(format!("monarch-rot-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(rank_dir(&root, 'E')).expect("temp dir");
        let hour = 3_600_000;
        let now = 100 * hour;
        let mut rot = Rotation {
            root: Some(root.clone()),
            hours: 1.0,
            last: 0,
        };
        assert_eq!(rot.wait(now), Some(0));
        // manual attempt: nothing found, schedule untouched
        assert_eq!(rot.next('E', None, now, false), None);
        assert_eq!(rot.wait(now), Some(0));
        // scheduled attempt: nothing found, but the next try is a full period away (no busy loop)
        assert_eq!(rot.next('E', None, now, true), None);
        assert_eq!(rot.wait(now), Some(hour));
        assert_eq!(rot.wait(now + 1000), Some(hour - 1000));
        // a missing rank folder behaves the same
        assert_eq!(rot.next('S', None, now + 2 * hour, true), None);
        assert_eq!(rot.wait(now + 2 * hour), Some(hour));
        // an image appears: the next due attempt rotates
        std::fs::write(rank_dir(&root, 'E').join("a.png"), b"x").expect("write");
        let due = now + 3 * hour;
        assert_eq!(rot.wait(due), Some(0));
        assert!(rot.next('E', None, due, true).is_some());
        assert_eq!(rot.wait(due), Some(hour));
        assert_eq!(MIN_ROTATION_WAIT_MS, 1_000);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Pushed presentation on a private, never-shown layered test window (not the desktop):
    /// full frame, then a clock-rectangle patch from a rectangle-sized DIB.
    #[test]
    fn layered_push_full_and_patch() {
        unsafe {
            let hinst = HINSTANCE(GetModuleHandleW(None).expect("module").0);
            let class = w!("MonarchNextWallpaperPushTest");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(test_proc),
                hInstance: hinst,
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);
            let (w, h) = (200usize, 120usize);
            let win = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                PCWSTR::null(),
                WS_POPUP,
                -32000,
                -32000,
                w as i32,
                h as i32,
                None,
                None,
                Some(hinst),
                None,
            )
            .expect("test window");
            let mut d = Dib::new(w, h).expect("dib");
            d.px_mut().copy_from_slice(&solid(w, h, [20, 40, 60]));
            assert!(push_full(win, &d));
            drop(d);
            let r = Crop {
                x0: 40,
                y0: 16,
                x1: 120,
                y1: 64,
            };
            assert!(push_patch(
                win,
                w,
                h,
                r,
                &solid(r.w(), r.h(), [200, 10, 10])
            ));
            let frost = frost_of(&Small {
                w: 25,
                h: 15,
                px: solid(25, 15, [90, 90, 90]),
            });
            let big = Dib::new(w, h).expect("dib");
            with_mem_dc(big.bmp, |_, mem| {
                stretch_small(mem, &frost, w, h);
                true
            });
            let _ = windows::Win32::Graphics::Gdi::GdiFlush();
            let mid = (h / 2 * w + w / 2) * 4;
            assert!((60..=66).contains(&big.px()[mid]), "{}", big.px()[mid]);
            assert!(push_full(win, &big));
            let _ = DestroyWindow(win);
        }
    }

    /// End-to-end frame (no window): `MONARCH_WALL_SNAPSHOT=<png>` keeps the output for a look.
    #[test]
    fn snapshot_renders_clock_in_memory() {
        let keep = std::env::var_os("MONARCH_WALL_SNAPSHOT").map(PathBuf::from);
        let out = keep.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("monarch-wall-snap-{}.png", std::process::id()))
        });
        snapshot_wallpaper(&out, None, DEFAULT_ACCENT, 'S').expect("snapshot");
        let (w, h) = screen_size();
        let px = thumbnail_rgba(&out, w as u32, h as u32).expect("decode");
        let r = clock_crop(w, h, false);
        // the clock inked something brighter than the plain background inside its rectangle
        let lit = (r.y0..r.y1)
            .flat_map(|y| (r.x0..r.x1).map(move |x| (y * w + x) * 4))
            .filter(|i| px[*i + 2] > 120)
            .count();
        assert!(lit > 1000, "lit={lit}");
        if keep.is_none() {
            let _ = std::fs::remove_file(&out);
        }
    }

    #[test]
    fn clock_band_is_inside_frame() {
        for (w, h) in [(1920, 1080), (3840, 2160), (1366, 768), (1080, 1920)] {
            let l = clock_layout(w, h, DEFAULT_ACCENT, 'E', &LocalTime::default());
            assert!(l.y0 < l.y1 && l.y1 <= h, "{w}x{h}: {}..{}", l.y0, l.y1);
            assert_eq!((l.y0, l.y1), band_of(w, h));
            for line in &l.lines {
                assert!(line.y >= l.y0 as f32 && line.y + line.h <= l.y1 as f32);
            }
        }
    }
}
