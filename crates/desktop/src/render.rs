//! Native renderer: Direct2D shapes + DirectWrite text (Arabic shaping, bidi) with the shell's
//! fonts embedded in the executable (private in-memory collection, nothing installed).
//! One `Canvas` API draws into either a layered window (per-pixel alpha, UpdateLayeredWindow)
//! or a WIC bitmap (PNG snapshots for pixel comparison without touching the desktop).
use std::cell::RefCell;
use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Graphics::{
            Direct2D::Common::*, Direct2D::*, DirectWrite::*, Dxgi::Common::*, Gdi::*, Imaging::*,
        },
        System::Com::*,
        UI::WindowsAndMessaging::*,
    },
};

static FONT_FILES: &[&[u8]] = &[
    include_bytes!("../assets/fonts/Rajdhani-Bold.ttf"),
    include_bytes!("../assets/fonts/Rajdhani-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Alexandria.ttf"),
    include_bytes!("../assets/fonts/ReemKufi.ttf"),
];

/// Font roles of the theme (theme.css --f-label / --f-body / --f-head).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Face {
    Label,
    Body,
    Head,
}
impl Face {
    fn family(self) -> &'static str {
        match self {
            Face::Label => "Rajdhani",
            Face::Body => "Alexandria",
            Face::Head => "Reem Kufi",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}
impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self {
            r: r as f32 / 255.,
            g: g as f32 / 255.,
            b: b as f32 / 255.,
            a: 1.,
        }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: f32) -> Self {
        Self {
            r: r as f32 / 255.,
            g: g as f32 / 255.,
            b: b as f32 / 255.,
            a,
        }
    }
    pub fn hex(s: &str) -> Self {
        let p = |i: usize| u8::from_str_radix(s.get(i..i + 2).unwrap_or("ff"), 16).unwrap_or(255);
        Self::rgb(p(1), p(3), p(5))
    }
    pub fn to_hex(self) -> String {
        let q = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
        format!("#{:02x}{:02x}{:02x}", q(self.r), q(self.g), q(self.b))
    }
    pub fn alpha(self, a: f32) -> Self {
        Self {
            a: self.a * a,
            ..self
        }
    }
    /// CSS color-mix(in srgb, self p, other).
    pub fn mix(self, other: Color, p: f32) -> Self {
        let q = 1. - p;
        Self {
            r: self.r * p + other.r * q,
            g: self.g * p + other.g * q,
            b: self.b * p + other.b * q,
            a: self.a * p + other.a * q,
        }
    }
    fn d2d(self) -> D2D1_COLOR_F {
        D2D1_COLOR_F {
            r: self.r,
            g: self.g,
            b: self.b,
            a: self.a,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn inset(&self, d: f32) -> Self {
        Self::new(self.x + d, self.y + d, self.w - 2. * d, self.h - 2. * d)
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    fn d2d(&self) -> D2D_RECT_F {
        D2D_RECT_F {
            left: self.x,
            top: self.y,
            right: self.right(),
            bottom: self.bottom(),
        }
    }
}

/// Per-corner radii (NibrasShell segmented groups: outer r, inner r/4).
#[derive(Clone, Copy, Debug)]
pub struct Radii {
    pub tl: f32,
    pub tr: f32,
    pub br: f32,
    pub bl: f32,
}
impl Radii {
    pub const fn all(r: f32) -> Self {
        Self {
            tl: r,
            tr: r,
            br: r,
            bl: r,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Align {
    Start,
    Center,
    End,
}

pub struct Font {
    pub face: Face,
    pub size: f32,
    pub weight: i32,
}
impl Font {
    pub const fn new(face: Face, size: f32, weight: i32) -> Self {
        Self { face, size, weight }
    }
}

/// Text format cache key: face, size ×100, weight, alignment.
type FormatKey = (Face, u32, i32, u8);

struct Gfx {
    d2d: ID2D1Factory,
    dw: IDWriteFactory5,
    fonts: IDWriteFontCollection1,
    fallback: IDWriteFontFallback,
    wic: IWICImagingFactory,
    formats: RefCell<Vec<(FormatKey, IDWriteTextFormat)>>,
}
thread_local! { static GFX: RefCell<Option<std::rc::Rc<Gfx>>> = const { RefCell::new(None) }; }

fn gfx() -> Result<std::rc::Rc<Gfx>> {
    GFX.with(|g| {
        if let Some(x) = g.borrow().as_ref() {
            return Ok(x.clone());
        }
        let x = std::rc::Rc::new(unsafe { Gfx::create()? });
        *g.borrow_mut() = Some(x.clone());
        Ok(x)
    })
}

impl Gfx {
    unsafe fn create() -> Result<Self> {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let dw: IDWriteFactory5 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
        // embedded fonts -> private collection
        let loader = dw.CreateInMemoryFontFileLoader()?;
        dw.RegisterFontFileLoader(&loader)?;
        let builder = dw.CreateFontSetBuilder()?;
        for data in FONT_FILES {
            let file = loader.CreateInMemoryFontFileReference(
                &dw,
                data.as_ptr() as *const _,
                data.len() as u32,
                None,
            )?;
            builder.AddFontFile(&file)?;
        }
        let set = builder.CreateFontSet()?;
        let fonts = dw.CreateFontCollectionFromFontSet(&set)?;
        // Arabic / symbols in a Latin face (Rajdhani) fall back to Alexandria, then to the system
        let fb = dw.CreateFontFallbackBuilder()?;
        let arabic = [
            DWRITE_UNICODE_RANGE {
                first: 0x0600,
                last: 0x06FF,
            },
            DWRITE_UNICODE_RANGE {
                first: 0x0750,
                last: 0x077F,
            },
            DWRITE_UNICODE_RANGE {
                first: 0x08A0,
                last: 0x08FF,
            },
            DWRITE_UNICODE_RANGE {
                first: 0xFB50,
                last: 0xFDFF,
            },
            DWRITE_UNICODE_RANGE {
                first: 0xFE70,
                last: 0xFEFF,
            },
        ];
        let alex = crate::platform::wide("Alexandria");
        let families = [alex.as_ptr()];
        fb.AddMapping(&arabic, &families, &fonts, None, None, 1.0)?;
        let system = dw.GetSystemFontFallback()?;
        fb.AddMappings(&system)?;
        let fallback = fb.CreateFontFallback()?;
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        Ok(Self {
            d2d,
            dw,
            fonts,
            fallback,
            wic,
            formats: RefCell::new(Vec::new()),
        })
    }

    fn format(&self, font: &Font, align: Align) -> Result<IDWriteTextFormat> {
        let key = (
            font.face,
            (font.size * 100.) as u32,
            font.weight,
            align as u8,
        );
        if let Some((_, f)) = self.formats.borrow().iter().find(|(k, _)| *k == key) {
            return Ok(f.clone());
        }
        let family = crate::platform::wide(font.face.family());
        let f = unsafe {
            let f = self.dw.CreateTextFormat(
                PCWSTR(family.as_ptr()),
                &self.fonts,
                DWRITE_FONT_WEIGHT(font.weight),
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                font.size,
                w!("en-us"),
            )?;
            f.SetTextAlignment(match align {
                Align::Start => DWRITE_TEXT_ALIGNMENT_LEADING,
                Align::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                Align::End => DWRITE_TEXT_ALIGNMENT_TRAILING,
            })?;
            f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            let trim = DWRITE_TRIMMING {
                granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                delimiter: 0,
                delimiterCount: 0,
            };
            let sign = self.dw.CreateEllipsisTrimmingSign(&f)?;
            f.SetTrimming(&trim, &sign)?;
            f.cast::<IDWriteTextFormat1>()?
                .SetFontFallback(&self.fallback)?;
            f
        };
        let mut v = self.formats.borrow_mut();
        if v.len() > 48 {
            v.remove(0);
        } // bounded cache
        v.push((key, f.clone()));
        Ok(f)
    }
}

/// Release the factories while COM is still initialised (thread-local destructors run too late).
pub fn shutdown() {
    MEASURE.with(|m| {
        m.borrow_mut().take();
    });
    GFX.with(|g| {
        g.borrow_mut().take();
    });
}

/// Drawing surface (DIPs; `scale` maps DIPs to pixels for the current DPI).
pub struct Canvas {
    rt: ID2D1RenderTarget,
    gfx: std::rc::Rc<Gfx>,
}

/// True when the text needs right-to-left reading order (any Arabic letter).
pub fn is_rtl(s: &str) -> bool {
    s.chars().any(|c| {
        ('\u{0600}'..='\u{06FF}').contains(&c)
            || ('\u{0750}'..='\u{08FF}').contains(&c)
            || ('\u{FB50}'..='\u{FEFF}').contains(&c)
    })
}

impl Canvas {
    fn brush(&self, c: Color) -> Result<ID2D1SolidColorBrush> {
        unsafe { self.rt.CreateSolidColorBrush(&c.d2d(), None) }
    }

    pub fn clear(&self, c: Color) {
        unsafe { self.rt.Clear(Some(&c.d2d())) }
    }

    fn round_geometry(&self, r: Rect, k: Radii) -> Result<ID2D1PathGeometry> {
        unsafe {
            let g = self.gfx.d2d.CreatePathGeometry()?;
            let s = g.Open()?;
            let max = (r.w.min(r.h)) / 2.;
            let (tl, tr, br, bl) = (k.tl.min(max), k.tr.min(max), k.br.min(max), k.bl.min(max));
            let p = |x: f32, y: f32| windows_numerics::Vector2 { X: x, Y: y };
            let arc = |x: f32, y: f32, rad: f32| D2D1_ARC_SEGMENT {
                point: p(x, y),
                size: D2D_SIZE_F {
                    width: rad,
                    height: rad,
                },
                rotationAngle: 0.,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: D2D1_ARC_SIZE_SMALL,
            };
            s.BeginFigure(p(r.x + tl, r.y), D2D1_FIGURE_BEGIN_FILLED);
            s.AddLine(p(r.right() - tr, r.y));
            if tr > 0. {
                s.AddArc(&arc(r.right(), r.y + tr, tr));
            }
            s.AddLine(p(r.right(), r.bottom() - br));
            if br > 0. {
                s.AddArc(&arc(r.right() - br, r.bottom(), br));
            }
            s.AddLine(p(r.x + bl, r.bottom()));
            if bl > 0. {
                s.AddArc(&arc(r.x, r.bottom() - bl, bl));
            }
            s.AddLine(p(r.x, r.y + tl));
            if tl > 0. {
                s.AddArc(&arc(r.x + tl, r.y, tl));
            }
            s.EndFigure(D2D1_FIGURE_END_CLOSED);
            s.Close()?;
            Ok(g)
        }
    }

    pub fn fill_round(&self, r: Rect, k: Radii, c: Color) {
        if let (Ok(g), Ok(b)) = (self.round_geometry(r, k), self.brush(c)) {
            unsafe { self.rt.FillGeometry(&g, &b, None) }
        }
    }
    /// Horizontal linear gradient across `r` (CSS linear-gradient(90deg, stops…)).
    pub fn fill_round_gradient(&self, r: Rect, k: Radii, stops: &[(f32, Color)]) {
        let Ok(g) = self.round_geometry(r, k) else {
            return;
        };
        let stops: Vec<D2D1_GRADIENT_STOP> = stops
            .iter()
            .map(|(p, c)| D2D1_GRADIENT_STOP {
                position: *p,
                color: c.d2d(),
            })
            .collect();
        unsafe {
            let Ok(coll) = self.rt.CreateGradientStopCollection(
                &stops,
                D2D1_GAMMA_2_2,
                D2D1_EXTEND_MODE_CLAMP,
            ) else {
                return;
            };
            let props = D2D1_LINEAR_GRADIENT_BRUSH_PROPERTIES {
                startPoint: windows_numerics::Vector2 { X: r.x, Y: r.y },
                endPoint: windows_numerics::Vector2 {
                    X: r.right(),
                    Y: r.y,
                },
            };
            if let Ok(b) = self.rt.CreateLinearGradientBrush(&props, None, &coll) {
                self.rt.FillGeometry(&g, &b, None);
            }
        }
    }

    /// Clip subsequent drawing to `r` (pair with `pop_clip`).
    pub fn push_clip(&self, r: Rect) {
        unsafe {
            self.rt
                .PushAxisAlignedClip(&r.d2d(), D2D1_ANTIALIAS_MODE_PER_PRIMITIVE)
        }
    }
    pub fn pop_clip(&self) {
        unsafe { self.rt.PopAxisAlignedClip() }
    }

    pub fn stroke_round(&self, r: Rect, k: Radii, c: Color, width: f32) {
        let r = r.inset(width / 2.);
        if let (Ok(g), Ok(b)) = (self.round_geometry(r, k), self.brush(c)) {
            unsafe { self.rt.DrawGeometry(&g, &b, width, None) }
        }
    }
    /// Box with fill + 1px border (CSS: background + border), the shell's chip.
    pub fn chip(&self, r: Rect, k: Radii, fill: Color, border: Option<Color>) {
        self.fill_round(r, k, fill);
        if let Some(b) = border {
            self.stroke_round(r, k, b, 1.)
        }
    }
    pub fn fill_rect(&self, r: Rect, c: Color) {
        if let Ok(b) = self.brush(c) {
            unsafe { self.rt.FillRectangle(&r.d2d(), &b) }
        }
    }
    pub fn fill_circle(&self, cx: f32, cy: f32, rad: f32, c: Color) {
        if let Ok(b) = self.brush(c) {
            let e = D2D1_ELLIPSE {
                point: windows_numerics::Vector2 { X: cx, Y: cy },
                radiusX: rad,
                radiusY: rad,
            };
            unsafe { self.rt.FillEllipse(&e, &b) }
        }
    }
    pub fn stroke_circle(&self, cx: f32, cy: f32, rad: f32, c: Color, width: f32) {
        if let Ok(b) = self.brush(c) {
            let e = D2D1_ELLIPSE {
                point: windows_numerics::Vector2 { X: cx, Y: cy },
                radiusX: rad,
                radiusY: rad,
            };
            unsafe { self.rt.DrawEllipse(&e, &b, width, None) }
        }
    }
    /// Progress ring (NibrasShell TopbarCircularProgress): faint track + rounded arc from 12 o'clock.
    #[allow(clippy::too_many_arguments)] // geometry + two colours: a struct would only rename the arguments
    pub fn ring(
        &self,
        cx: f32,
        cy: f32,
        size: f32,
        stroke: f32,
        value: f32,
        track: Color,
        c: Color,
    ) {
        let rad = (size - stroke) / 2.;
        self.stroke_circle(cx, cy, rad, track, stroke);
        let v = value.clamp(0., 1.);
        if v <= 0. {
            return;
        }
        unsafe {
            let Ok(g) = self.gfx.d2d.CreatePathGeometry() else {
                return;
            };
            let Ok(s) = g.Open() else { return };
            let p = |a: f32| windows_numerics::Vector2 {
                X: cx + rad * a.sin(),
                Y: cy - rad * a.cos(),
            };
            let end = v.min(0.9999) * std::f32::consts::TAU;
            s.BeginFigure(p(0.), D2D1_FIGURE_BEGIN_HOLLOW);
            s.AddArc(&D2D1_ARC_SEGMENT {
                point: p(end),
                size: D2D_SIZE_F {
                    width: rad,
                    height: rad,
                },
                rotationAngle: 0.,
                sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                arcSize: if v > 0.5 {
                    D2D1_ARC_SIZE_LARGE
                } else {
                    D2D1_ARC_SIZE_SMALL
                },
            });
            s.EndFigure(D2D1_FIGURE_END_OPEN);
            let _ = s.Close();
            let props = D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                ..Default::default()
            };
            if let (Ok(st), Ok(b)) = (self.gfx.d2d.CreateStrokeStyle(&props, None), self.brush(c)) {
                self.rt.DrawGeometry(&g, &b, stroke, &st);
            }
        }
    }

    fn layout(
        &self,
        text: &str,
        font: &Font,
        max_w: f32,
        h: f32,
        align: Align,
        tracking: f32,
    ) -> Result<IDWriteTextLayout> {
        // DirectWrite's leading/trailing follow the reading direction; our Align is visual (left/right),
        // so mirror it for right-to-left text
        let align = match (is_rtl(text), align) {
            (true, Align::Start) => Align::End,
            (true, Align::End) => Align::Start,
            (_, a) => a,
        };
        let f = self.gfx.format(font, align)?;
        let w: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let l = self
                .gfx
                .dw
                .CreateTextLayout(&w, &f, max_w.max(1.), h.max(1.))?;
            if is_rtl(text) {
                l.SetReadingDirection(DWRITE_READING_DIRECTION_RIGHT_TO_LEFT)?;
            }
            if tracking != 0. {
                let range = DWRITE_TEXT_RANGE {
                    startPosition: 0,
                    length: w.len() as u32,
                };
                l.cast::<IDWriteTextLayout1>()?
                    .SetCharacterSpacing(0., tracking, 0., range)?;
            }
            Ok(l)
        }
    }
    /// Natural width of a single line.
    pub fn measure(&self, text: &str, font: &Font, tracking: f32) -> f32 {
        self.layout(text, font, 10_000., 100., Align::Start, tracking)
            .ok()
            .and_then(|l| unsafe {
                let mut m = DWRITE_TEXT_METRICS::default();
                l.GetMetrics(&mut m).ok().map(|_| m)
            })
            .map(|m| m.widthIncludingTrailingWhitespace)
            .unwrap_or(0.)
    }
    /// One line of text in `r`, vertically centred, ellipsis when it does not fit.
    pub fn text(&self, s: &str, r: Rect, font: &Font, c: Color, align: Align) {
        self.text_tracked(s, r, font, c, align, 0.)
    }
    pub fn text_tracked(
        &self,
        s: &str,
        r: Rect,
        font: &Font,
        c: Color,
        align: Align,
        tracking: f32,
    ) {
        if s.is_empty() {
            return;
        }
        if let (Ok(l), Ok(b)) = (
            self.layout(s, font, r.w, r.h, align, tracking),
            self.brush(c),
        ) {
            unsafe {
                self.rt.DrawTextLayout(
                    windows_numerics::Vector2 { X: r.x, Y: r.y },
                    &l,
                    &b,
                    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                )
            }
        }
    }

    /// Multi-line text wrapped to `w`, starting at (`x`, `y`). Returns the height used.
    #[allow(clippy::too_many_arguments)]
    pub fn text_wrapped(
        &self,
        s: &str,
        x: f32,
        y: f32,
        w: f32,
        font: &Font,
        c: Color,
        line_height: f32,
    ) -> f32 {
        if s.is_empty() {
            return 0.;
        }
        let Ok(f) = self.gfx.format(font, Align::Start) else {
            return 0.;
        };
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(l) = self.gfx.dw.CreateTextLayout(&wide, &f, w.max(1.), 10_000.) else {
                return 0.;
            };
            let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            let _ = l.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_NEAR);
            let _ = l.SetLineSpacing(
                DWRITE_LINE_SPACING_METHOD_UNIFORM,
                line_height,
                line_height * 0.8,
            );
            if is_rtl(s) {
                let _ = l.SetReadingDirection(DWRITE_READING_DIRECTION_RIGHT_TO_LEFT);
            }
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            if let Ok(b) = self.brush(c) {
                self.rt.DrawTextLayout(
                    windows_numerics::Vector2 { X: x, Y: y },
                    &l,
                    &b,
                    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
                );
            }
            m.height
        }
    }

    /// Height that `text_wrapped` would use (layout only, nothing drawn).
    pub fn wrapped_height(&self, s: &str, w: f32, font: &Font, line_height: f32) -> f32 {
        if s.is_empty() {
            return 0.;
        }
        let Ok(f) = self.gfx.format(font, Align::Start) else {
            return 0.;
        };
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            let Ok(l) = self.gfx.dw.CreateTextLayout(&wide, &f, w.max(1.), 10_000.) else {
                return 0.;
            };
            let _ = l.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP);
            let _ = l.SetLineSpacing(
                DWRITE_LINE_SPACING_METHOD_UNIFORM,
                line_height,
                line_height * 0.8,
            );
            let mut m = DWRITE_TEXT_METRICS::default();
            let _ = l.GetMetrics(&mut m);
            m.height
        }
    }

    /// Shift all following drawing by (dx, dy) (scrolling content); `reset_translation` undoes it.
    pub fn translate(&self, dx: f32, dy: f32) {
        unsafe {
            self.rt.SetTransform(&windows_numerics::Matrix3x2 {
                M11: 1.,
                M12: 0.,
                M21: 0.,
                M22: 1.,
                M31: dx,
                M32: dy,
            });
        }
    }
    pub fn reset_translation(&self) {
        unsafe {
            self.rt.SetTransform(&windows_numerics::Matrix3x2 {
                M11: 1.,
                M12: 0.,
                M21: 0.,
                M22: 1.,
                M31: 0.,
                M32: 0.,
            })
        }
    }

    /// Lucide icon (24×24 viewBox, stroke-based) into `r`.
    pub fn icon(&self, name: &str, r: Rect, c: Color, stroke: f32) {
        let Some(svg) = crate::icons::get(name) else {
            return;
        };
        let k = r.w / 24.;
        unsafe {
            let m = windows_numerics::Matrix3x2 {
                M11: k,
                M12: 0.,
                M21: 0.,
                M22: k,
                M31: r.x,
                M32: r.y,
            };
            let mut old = windows_numerics::Matrix3x2::default();
            self.rt.GetTransform(&mut old);
            self.rt.SetTransform(&(m * old));
            let props = D2D1_STROKE_STYLE_PROPERTIES {
                startCap: D2D1_CAP_STYLE_ROUND,
                endCap: D2D1_CAP_STYLE_ROUND,
                lineJoin: D2D1_LINE_JOIN_ROUND,
                ..Default::default()
            };
            if let (Ok(st), Ok(b)) = (self.gfx.d2d.CreateStrokeStyle(&props, None), self.brush(c)) {
                for shape in crate::icons::shapes(svg) {
                    if let Ok(g) = crate::icons::geometry(&self.gfx.d2d, &shape) {
                        if shape.filled {
                            self.rt.FillGeometry(&g, &b, None);
                        }
                        self.rt.DrawGeometry(&g, &b, stroke / k * k, &st);
                    }
                }
            }
            self.rt.SetTransform(&old);
        }
    }

    /// Bitmap from straight RGBA pixels (app icons, wallpaper thumbs).
    pub fn image(&self, rgba: &[u8], w: u32, h: u32, r: Rect, opacity: f32) {
        let mut pm = rgba.to_vec();
        for p in pm.as_chunks_mut::<4>().0 {
            let a = p[3] as u32;
            p.swap(0, 2);
            for v in &mut p[..3] {
                *v = (*v as u32 * a / 255) as u8; // premultiply
            }
        }
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.,
            dpiY: 96.,
        };
        unsafe {
            if let Ok(bmp) = self.rt.CreateBitmap(
                D2D_SIZE_U {
                    width: w,
                    height: h,
                },
                Some(pm.as_ptr() as *const _),
                w * 4,
                &props,
            ) {
                self.rt.DrawBitmap(
                    &bmp,
                    Some(&r.d2d()),
                    opacity,
                    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                    None,
                );
            }
        }
    }
}

/// Render `paint` into a PNG (snapshots for pixel comparison; no window, no desktop change).
pub fn snapshot(
    path: &std::path::Path,
    w: u32,
    h: u32,
    scale: f32,
    paint: impl FnOnce(&Canvas),
) -> Result<()> {
    let g = gfx()?;
    unsafe {
        let (pw, ph) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
        let bmp =
            g.wic
                .CreateBitmap(pw, ph, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96. * scale,
            dpiY: 96. * scale,
            ..Default::default()
        };
        let rt = g.d2d.CreateWicBitmapRenderTarget(&bmp, &props)?;
        rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        rt.BeginDraw();
        let canvas = Canvas {
            rt: rt.clone(),
            gfx: g.clone(),
        };
        canvas.clear(Color::rgba(0, 0, 0, 0.));
        paint(&canvas);
        rt.EndDraw(None, None)?;
        let stream = g.wic.CreateStream()?;
        let wpath = crate::platform::wide(path.as_os_str());
        stream.InitializeFromFilename(PCWSTR(wpath.as_ptr()), GENERIC_WRITE.0)?;
        let enc = g
            .wic
            .CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        enc.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let mut frame = None;
        enc.CreateNewFrame(&mut frame, std::ptr::null_mut())?;
        let frame = frame.ok_or_else(|| Error::from(E_FAIL))?;
        frame.Initialize(None)?;
        frame.WriteSource(&bmp, std::ptr::null())?;
        frame.Commit()?;
        enc.Commit()?;
        Ok(())
    }
}

/// Render `paint` into memory as premultiplied BGRA (`w*scale × h*scale`, rows tightly packed):
/// no file, no window (e.g. the wallpaper clock layer).
pub fn to_pbgra(w: u32, h: u32, scale: f32, paint: impl FnOnce(&Canvas)) -> Result<Vec<u8>> {
    let g = gfx()?;
    unsafe {
        let (pw, ph) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
        let bmp =
            g.wic
                .CreateBitmap(pw, ph, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96. * scale,
            dpiY: 96. * scale,
            ..Default::default()
        };
        {
            let rt = g.d2d.CreateWicBitmapRenderTarget(&bmp, &props)?;
            rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            rt.BeginDraw();
            let canvas = Canvas {
                rt: rt.clone(),
                gfx: g.clone(),
            };
            canvas.clear(Color::rgba(0, 0, 0, 0.));
            paint(&canvas);
            rt.EndDraw(None, None)?;
        }
        let len = (pw as usize) * (ph as usize) * 4;
        let mut px = vec![0u8; len];
        bmp.CopyPixels(std::ptr::null(), pw * 4, &mut px)?;
        Ok(px)
    }
}

/// Run text measurement outside a paint (layout decisions such as window sizes) on a 1×1 target.
pub fn measure_with<T>(f: impl FnOnce(&Canvas) -> T) -> Option<T> {
    // one tiny target per thread, reused: layout decisions happen on hover and every clock tick
    if MEASURE.with(|m| m.borrow().is_none()) {
        let g = gfx().ok()?;
        let canvas = unsafe {
            let bmp = g
                .wic
                .CreateBitmap(1, 1, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad)
                .ok()?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                ..Default::default()
            };
            let rt = g.d2d.CreateWicBitmapRenderTarget(&bmp, &props).ok()?;
            Canvas { rt, gfx: g.clone() }
        };
        MEASURE.with(|m| *m.borrow_mut() = Some(canvas));
    }
    MEASURE.with(|m| m.borrow().as_ref().map(f))
}
thread_local! { static MEASURE: RefCell<Option<Canvas>> = const { RefCell::new(None) }; }
/// Paint a layered (per-pixel alpha) window: render into a DIB and push it with UpdateLayeredWindow.
/// GDI objects of one present, released on every path (including `?` errors).
struct Surface {
    screen: HDC,
    mem: HDC,
    dib: Option<HBITMAP>,
    old: Option<HGDIOBJ>,
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            if let Some(o) = self.old.take() {
                SelectObject(self.mem, o);
            }
            if let Some(d) = self.dib.take() {
                let _ = DeleteObject(d.into());
            }
            let _ = DeleteDC(self.mem);
            ReleaseDC(None, self.screen);
        }
    }
}

pub fn present(h: HWND, w: i32, ht: i32, scale: f32, paint: impl FnOnce(&Canvas)) -> Result<()> {
    if w <= 0 || ht <= 0 {
        return Ok(()); // destroyed or not yet sized: nothing to draw, nothing to allocate
    }
    let g = gfx()?;
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let mut surface = Surface {
            screen,
            mem,
            dib: None,
            old: None,
        };
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -ht,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(mem), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        surface.dib = Some(dib);
        surface.old = Some(SelectObject(mem, dib.into()));
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96. * scale,
            dpiY: 96. * scale,
            ..Default::default()
        };
        // software rasterizer: the shell surfaces are small; a hardware target loads the GPU driver (~20 MB)
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_SOFTWARE,
            ..props
        };
        let rt = g.d2d.CreateDCRenderTarget(&props)?;
        rt.BindDC(
            mem,
            &RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: ht,
            },
        )?;
        rt.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
        rt.BeginDraw();
        let canvas = Canvas {
            rt: rt.cast()?,
            gfx: g.clone(),
        };
        canvas.clear(Color::rgba(0, 0, 0, 0.));
        paint(&canvas);
        let r = rt.EndDraw(None, None);
        let mut wr = RECT::default();
        let _ = GetWindowRect(h, &mut wr);
        let pos = POINT {
            x: wr.left,
            y: wr.top,
        };
        let size = SIZE { cx: w, cy: ht };
        let src = POINT::default();
        let blend = BLENDFUNCTION {
            BlendOp: 0,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1,
        };
        let _ = UpdateLayeredWindow(
            h,
            Some(screen),
            Some(&pos),
            Some(&size),
            Some(mem),
            Some(&src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        drop(surface);
        r
    }
}
