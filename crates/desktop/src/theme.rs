//! Design tokens of the Shadow Monarch theme (identical to the legacy theme.css), resolved for an accent.
use crate::render::{Color, Face, Font};

#[derive(Clone, Copy)]
pub struct Tokens {
    pub accent: Color,
    pub bg: Color,
    pub success: Color,
    pub warning: Color,
    pub surface: Color,
    pub surface2: Color,
    pub surface_solid: Color,
    pub surface_hover: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub line: Color,
    pub line_soft: Color,
    pub accent_soft: Color,
}

pub const R: f32 = 12.; // --r (elementRadius)
pub const R_IN: f32 = 3.; // --r-in (elementRadius / 4)
pub const CHIP_H: f32 = 24.; // --chip-h
pub const BAR_H: f32 = 32.; // top bar height

impl Tokens {
    pub fn new(accent: Color) -> Self {
        let clear = Color::rgba(0, 0, 0, 0.);
        Self {
            accent,
            bg: Color::hex("#05080f"),
            success: Color::hex("#2dd4bf"),
            warning: Color::hex("#ff3b52"),
            surface: Color::rgba(9, 14, 25, 0.86),
            surface2: Color::rgba(15, 23, 39, 0.90),
            surface_solid: Color::rgba(7, 11, 20, 0.95),
            surface_hover: Color::rgba(26, 37, 60, 0.92),
            text: Color::hex("#dfe8f6"),
            muted: Color::hex("#7d8ba3"),
            faint: Color::hex("#3a4660"),
            line: accent.mix(clear, 0.30),
            line_soft: accent.mix(clear, 0.14),
            accent_soft: accent.mix(clear, 0.16),
        }
    }
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}
fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0., 1.);
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1. / 2.4) - 0.055
    }
}

/// CSS `oklch(from <c> calc(l + dl) c calc(h + dh))`: shift lightness and hue in OKLCH (gamut-clipped).
pub fn oklch_shift(c: Color, dl: f32, dh_deg: f32) -> Color {
    let (r, g, b) = (to_linear(c.r), to_linear(c.g), to_linear(c.b));
    let l_ = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m_ = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s_ = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    let l = 0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_;
    let a = 1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_;
    let bb = 0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_;
    let (chroma, hue) = ((a * a + bb * bb).sqrt(), bb.atan2(a) + dh_deg.to_radians());
    let (l, a, bb) = (l + dl, chroma * hue.cos(), chroma * hue.sin());
    let l_ = (l + 0.396_337_78 * a + 0.215_803_76 * bb).powi(3);
    let m_ = (l - 0.105_561_346 * a - 0.063_854_17 * bb).powi(3);
    let s_ = (l - 0.089_484_18 * a - 1.291_485_5 * bb).powi(3);
    Color {
        r: to_srgb(4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_),
        g: to_srgb(-1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_),
        b: to_srgb(-0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_),
        a: c.a,
    }
}

/// Fonts by role: --f-label (Rajdhani), --f-body (Alexandria), --f-head (Reem Kufi).
pub const fn label(size: f32, weight: i32) -> Font {
    Font::new(Face::Label, size, weight)
}
pub const fn body(size: f32, weight: i32) -> Font {
    Font::new(Face::Body, size, weight)
}
pub const fn head(size: f32, weight: i32) -> Font {
    Font::new(Face::Head, size, weight)
}
