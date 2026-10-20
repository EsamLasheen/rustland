//! Material You dynamic colour, natively: the algorithms of Google's material-color-utilities
//! (CAM16 / HCT colour space, Score source-colour selection, tonal palettes, dark scheme).
//! An image becomes one source colour; the source colour becomes the whole dark theme.

// ── sRGB ↔ XYZ ↔ L* ──────────────────────────────────────────────────────────────────────
const SRGB_TO_XYZ: [[f64; 3]; 3] = [
    [0.41233895, 0.35762064, 0.18051042],
    [0.2126, 0.7152, 0.0722],
    [0.01932141, 0.11916382, 0.95034478],
];
const XYZ_TO_SRGB: [[f64; 3]; 3] = [
    [
        3.2413774792388685,
        -1.5376652402851851,
        -0.49885366846268053,
    ],
    [-0.9691452513005321, 1.8758853451067872, 0.04156585616912061],
    [
        0.05562093689691305,
        -0.20395524564742123,
        1.0571799111220335,
    ],
];
const WHITE_POINT_D65: [f64; 3] = [95.047, 100.0, 108.883];

fn linearized(c: u8) -> f64 {
    let v = c as f64 / 255.0;
    if v <= 0.040449936 {
        v / 12.92 * 100.0
    } else {
        ((v + 0.055) / 1.055).powf(2.4) * 100.0
    }
}
fn delinearized(c: f64) -> u8 {
    let n = c / 100.0;
    let v = if n <= 0.0031308 {
        n * 12.92
    } else {
        1.055 * n.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}
fn xyz_from_rgb(c: [u8; 3]) -> [f64; 3] {
    let l = [linearized(c[0]), linearized(c[1]), linearized(c[2])];
    let m = SRGB_TO_XYZ;
    [0, 1, 2].map(|i| m[i][0] * l[0] + m[i][1] * l[1] + m[i][2] * l[2])
}
fn rgb_from_xyz(x: f64, y: f64, z: f64) -> [u8; 3] {
    let m = XYZ_TO_SRGB;
    [0, 1, 2].map(|i| delinearized(m[i][0] * x + m[i][1] * y + m[i][2] * z))
}
fn y_from_lstar(l: f64) -> f64 {
    if l > 8.0 {
        ((l + 16.0) / 116.0).powi(3) * 100.0
    } else {
        l / (24389.0 / 27.0) * 100.0
    }
}
fn lstar_from_y(y: f64) -> f64 {
    let t = y / 100.0;
    if t <= 216.0 / 24389.0 {
        24389.0 / 27.0 * t
    } else {
        116.0 * t.cbrt() - 16.0
    }
}
fn lstar_from_rgb(c: [u8; 3]) -> f64 {
    lstar_from_y(xyz_from_rgb(c)[1])
}
fn rgb_from_lstar(l: f64) -> [u8; 3] {
    let v = delinearized(y_from_lstar(l));
    [v, v, v]
}
fn sanitize_degrees(d: f64) -> f64 {
    d.rem_euclid(360.0)
}

// ── CAM16 ────────────────────────────────────────────────────────────────────────────────
struct Viewing {
    n: f64,
    aw: f64,
    nbb: f64,
    ncb: f64,
    c: f64,
    nc: f64,
    rgb_d: [f64; 3],
    fl: f64,
    fl_root: f64,
    z: f64,
}

fn viewing() -> &'static Viewing {
    static V: std::sync::OnceLock<Viewing> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        let wp = WHITE_POINT_D65;
        let adapting = 200.0 / std::f64::consts::PI * y_from_lstar(50.0) / 100.0;
        let background_lstar: f64 = 50.0;
        let surround = 2.0;
        let r_w = wp[0] * 0.401288 + wp[1] * 0.650173 + wp[2] * -0.051461;
        let g_w = wp[0] * -0.250268 + wp[1] * 1.204414 + wp[2] * 0.045854;
        let b_w = wp[0] * -0.002079 + wp[1] * 0.048952 + wp[2] * 0.953127;
        let f = 0.8 + surround / 10.0;
        let c = if f >= 0.9 {
            0.59 + (0.69 - 0.59) * ((f - 0.9) * 10.0)
        } else {
            0.525 + (0.59 - 0.525) * ((f - 0.8) * 10.0)
        };
        let d = (f * (1.0 - (1.0 / 3.6) * ((-adapting - 42.0) / 92.0).exp())).clamp(0.0, 1.0);
        let rgb_d = [
            d * (100.0 / r_w) + 1.0 - d,
            d * (100.0 / g_w) + 1.0 - d,
            d * (100.0 / b_w) + 1.0 - d,
        ];
        let k = 1.0 / (5.0 * adapting + 1.0);
        let k4 = k.powi(4);
        let k4f = 1.0 - k4;
        let fl = k4 * adapting + 0.1 * k4f * k4f * (5.0 * adapting).cbrt();
        let n = y_from_lstar(background_lstar.max(0.1)) / wp[1];
        let z = 1.48 + n.sqrt();
        let nbb = 0.725 / n.powf(0.2);
        let af =
            [rgb_d[0] * r_w, rgb_d[1] * g_w, rgb_d[2] * b_w].map(|v| (fl * v / 100.0).powf(0.42));
        let a = af.map(|v| 400.0 * v / (v + 27.13));
        let aw = (2.0 * a[0] + a[1] + 0.05 * a[2]) * nbb;
        Viewing {
            n,
            aw,
            nbb,
            ncb: nbb,
            c,
            nc: f,
            rgb_d,
            fl,
            fl_root: fl.powf(0.25),
            z,
        }
    })
}

#[derive(Clone, Copy, Debug)]
struct Cam16 {
    hue: f64,
    chroma: f64,
    j: f64,
    jstar: f64,
    astar: f64,
    bstar: f64,
}

impl Cam16 {
    fn from_rgb(c: [u8; 3]) -> Self {
        let v = viewing();
        let [x, y, z] = xyz_from_rgb(c);
        let r_c = 0.401288 * x + 0.650173 * y - 0.051461 * z;
        let g_c = -0.250268 * x + 1.204414 * y + 0.045854 * z;
        let b_c = -0.002079 * x + 0.048952 * y + 0.953127 * z;
        let adapt = |c: f64, d: f64| {
            let cd = d * c;
            let af = (v.fl * cd.abs() / 100.0).powf(0.42);
            cd.signum() * 400.0 * af / (af + 27.13)
        };
        let (ra, ga, ba) = (
            adapt(r_c, v.rgb_d[0]),
            adapt(g_c, v.rgb_d[1]),
            adapt(b_c, v.rgb_d[2]),
        );
        let a = (11.0 * ra - 12.0 * ga + ba) / 11.0;
        let b = (ra + ga - 2.0 * ba) / 9.0;
        let u = (20.0 * ra + 20.0 * ga + 21.0 * ba) / 20.0;
        let p2 = (40.0 * ra + 20.0 * ga + ba) / 20.0;
        let hue = sanitize_degrees(b.atan2(a).to_degrees());
        let ac = p2 * v.nbb;
        let j = 100.0 * (ac / v.aw).powf(v.c * v.z);
        let hue_prime = if hue < 20.14 { hue + 360.0 } else { hue };
        let e_hue = 0.25 * ((hue_prime.to_radians() + 2.0).cos() + 3.8);
        let p1 = 50000.0 / 13.0 * e_hue * v.nc * v.ncb;
        let t = p1 * a.hypot(b) / (u + 0.305);
        let alpha = t.powf(0.9) * (1.64 - 0.29f64.powf(v.n)).powf(0.73);
        let chroma = alpha * (j / 100.0).sqrt();
        Self::with(hue, chroma, j)
    }
    fn from_jch(j: f64, chroma: f64, hue: f64) -> Self {
        Self::with(hue, chroma, j)
    }
    fn with(hue: f64, chroma: f64, j: f64) -> Self {
        let v = viewing();
        let m = chroma * v.fl_root;
        let jstar = (1.0 + 100.0 * 0.007) * j / (1.0 + 0.007 * j);
        let mstar = 1.0 / 0.0228 * (1.0 + 0.0228 * m).ln();
        let h = hue.to_radians();
        Cam16 {
            hue,
            chroma,
            j,
            jstar,
            astar: mstar * h.cos(),
            bstar: mstar * h.sin(),
        }
    }
    fn distance(&self, o: &Cam16) -> f64 {
        let d = ((self.jstar - o.jstar).powi(2)
            + (self.astar - o.astar).powi(2)
            + (self.bstar - o.bstar).powi(2))
        .sqrt();
        1.41 * d.powf(0.63)
    }
    fn viewed(&self) -> [u8; 3] {
        let v = viewing();
        let alpha = if self.chroma == 0.0 || self.j == 0.0 {
            0.0
        } else {
            self.chroma / (self.j / 100.0).sqrt()
        };
        let t = (alpha / (1.64 - 0.29f64.powf(v.n)).powf(0.73)).powf(1.0 / 0.9);
        let h = self.hue.to_radians();
        let e_hue = 0.25 * ((h + 2.0).cos() + 3.8);
        let ac = v.aw * (self.j / 100.0).powf(1.0 / v.c / v.z);
        let p1 = e_hue * (50000.0 / 13.0) * v.nc * v.ncb;
        let p2 = ac / v.nbb;
        let (hs, hc) = h.sin_cos();
        let gamma = 23.0 * (p2 + 0.305) * t / (23.0 * p1 + 11.0 * t * hc + 108.0 * t * hs);
        let (a, b) = (gamma * hc, gamma * hs);
        let ra = (460.0 * p2 + 451.0 * a + 288.0 * b) / 1403.0;
        let ga = (460.0 * p2 - 891.0 * a - 261.0 * b) / 1403.0;
        let ba = (460.0 * p2 - 220.0 * a - 6300.0 * b) / 1403.0;
        let un = |x: f64| {
            let base = (27.13 * x.abs() / (400.0 - x.abs())).max(0.0);
            x.signum() * (100.0 / v.fl) * base.powf(1.0 / 0.42)
        };
        let (rf, gf, bf) = (
            un(ra) / v.rgb_d[0],
            un(ga) / v.rgb_d[1],
            un(ba) / v.rgb_d[2],
        );
        let x = 1.86206786 * rf - 1.01125463 * gf + 0.14918677 * bf;
        let y = 0.38752654 * rf + 0.62144744 * gf - 0.00897398 * bf;
        let z = -0.01584150 * rf - 0.03412294 * gf + 1.04996444 * bf;
        rgb_from_xyz(x, y, z)
    }
}

// ── HCT ──────────────────────────────────────────────────────────────────────────────────
/// Hue, chroma (CAM16) and tone (L*) of a colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hct {
    pub hue: f64,
    pub chroma: f64,
    pub tone: f64,
}

impl Hct {
    pub fn from_rgb(c: [u8; 3]) -> Self {
        let cam = Cam16::from_rgb(c);
        Hct {
            hue: cam.hue,
            chroma: cam.chroma,
            tone: lstar_from_rgb(c),
        }
    }
    /// The sRGB colour closest to the requested hue/chroma at exactly this tone (chroma is reduced
    /// until the colour exists).
    pub fn to_rgb(hue: f64, chroma: f64, tone: f64) -> [u8; 3] {
        if chroma < 1.0 || tone.round() <= 0.0 || tone.round() >= 100.0 {
            return rgb_from_lstar(tone);
        }
        let hue = sanitize_degrees(hue);
        let (mut low, mut high, mut mid) = (0.0, chroma, chroma);
        let mut first = true;
        let mut answer: Option<Cam16> = None;
        while (low - high).abs() >= 0.4 {
            let found = find_cam_by_j(hue, mid, tone);
            if first {
                if let Some(c) = found {
                    return c.viewed();
                }
                first = false;
                mid = low + (high - low) / 2.0;
                continue;
            }
            match found {
                None => high = mid,
                Some(c) => {
                    answer = Some(c);
                    low = mid;
                }
            }
            mid = low + (high - low) / 2.0;
        }
        answer
            .map(|c| c.viewed())
            .unwrap_or_else(|| rgb_from_lstar(tone))
    }
}

fn find_cam_by_j(hue: f64, chroma: f64, tone: f64) -> Option<Cam16> {
    let (mut low, mut high) = (0.0f64, 100.0f64);
    let (mut best_dl, mut best_de) = (1000.0f64, 1000.0f64);
    let mut best = None;
    while (low - high).abs() > 0.01 {
        let mid = low + (high - low) / 2.0;
        let clipped = Cam16::from_jch(mid, chroma, hue).viewed();
        let clipped_l = lstar_from_rgb(clipped);
        let dl = (tone - clipped_l).abs();
        if dl < 0.2 {
            let cam = Cam16::from_rgb(clipped);
            let de = cam.distance(&Cam16::from_jch(cam.j, cam.chroma, hue));
            if de <= 1.0 && de <= best_de {
                best_dl = dl;
                best_de = de;
                best = Some(cam);
            }
        }
        if best_dl == 0.0 && best_de == 0.0 {
            break;
        }
        if clipped_l < tone {
            low = mid;
        } else {
            high = mid;
        }
    }
    best
}

/// A hue and chroma at every tone (Material tonal palette).
#[derive(Clone, Copy, Debug)]
pub struct Tonal {
    pub hue: f64,
    pub chroma: f64,
}
impl Tonal {
    pub fn tone(&self, t: f64) -> [u8; 3] {
        Hct::to_rgb(self.hue, self.chroma, t)
    }
}

// ── Score: pick the source colour of an image ────────────────────────────────────────────
/// Material's Score algorithm over a colour → population map: favours hues that cover much of the
/// image (with their neighbours) and colours close to a lively chroma. `None` when nothing qualifies.
pub fn score(colors: &[([u8; 3], u32)]) -> Option<[u8; 3]> {
    const TARGET_CHROMA: f64 = 48.0;
    const WEIGHT_PROPORTION: f64 = 0.7;
    const WEIGHT_CHROMA_ABOVE: f64 = 0.3;
    const WEIGHT_CHROMA_BELOW: f64 = 0.1;
    const CUTOFF_CHROMA: f64 = 5.0;
    const CUTOFF_EXCITED_PROPORTION: f64 = 0.01;
    let hcts: Vec<(Hct, u32, [u8; 3])> = colors
        .iter()
        .map(|(c, n)| (Hct::from_rgb(*c), *n, *c))
        .collect();
    let mut hue_population = [0f64; 360];
    let mut total = 0f64;
    for (h, n, _) in &hcts {
        hue_population[(h.hue.floor() as usize) % 360] += *n as f64;
        total += *n as f64;
    }
    if total == 0.0 {
        return None;
    }
    let mut excited = [0f64; 360];
    for hue in 0..360i32 {
        let p = hue_population[hue as usize] / total;
        for i in hue - 14..=hue + 14 {
            excited[i.rem_euclid(360) as usize] += p;
        }
    }
    let mut scored: Vec<(f64, Hct, [u8; 3])> = hcts
        .iter()
        .filter_map(|(h, _, c)| {
            let p = excited[(h.hue.round() as usize) % 360];
            if h.chroma < CUTOFF_CHROMA || p <= CUTOFF_EXCITED_PROPORTION {
                return None;
            }
            let weight = if h.chroma < TARGET_CHROMA {
                WEIGHT_CHROMA_BELOW
            } else {
                WEIGHT_CHROMA_ABOVE
            };
            Some((
                p * 100.0 * WEIGHT_PROPORTION + (h.chroma - TARGET_CHROMA) * weight,
                *h,
                *c,
            ))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.first().map(|s| s.2)
}

/// Source colour of an image (RGBA, or BGRA when `bgra`): pixels are binned at 5 bits per channel
/// (each bin's mean colour stands for it), then scored.
pub fn source_color(px: &[u8], w: usize, h: usize, bgra: bool) -> Option<[u8; 3]> {
    if w == 0 || h == 0 || px.len() < w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    // ~25k samples regardless of image size
    let step = ((w * h) as f64 / 25_000.0).sqrt().max(1.0) as usize;
    let mut bins: std::collections::HashMap<u16, (u64, u64, u64, u32)> =
        std::collections::HashMap::new();
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            let i = (y * w + x) * 4;
            let (r, g, b) = if bgra {
                (px[i + 2], px[i + 1], px[i])
            } else {
                (px[i], px[i + 1], px[i + 2])
            };
            let key = ((r as u16 >> 3) << 10) | ((g as u16 >> 3) << 5) | (b as u16 >> 3);
            let e = bins.entry(key).or_default();
            e.0 += r as u64;
            e.1 += g as u64;
            e.2 += b as u64;
            e.3 += 1;
        }
    }
    let colors: Vec<([u8; 3], u32)> = bins
        .values()
        .map(|(r, g, b, n)| {
            (
                [
                    (r / *n as u64) as u8,
                    (g / *n as u64) as u8,
                    (b / *n as u64) as u8,
                ],
                *n,
            )
        })
        .collect();
    score(&colors)
}

// ── Dark scheme ──────────────────────────────────────────────────────────────────────────
/// The colours of a dark Material scheme built from one source colour ("content" variant: the
/// primary keeps the image's own colourfulness, surfaces carry a faint tint of its hue).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scheme {
    pub primary: [u8; 3],
    pub on_primary: [u8; 3],
    pub primary_container: [u8; 3],
    pub secondary: [u8; 3],
    pub tertiary: [u8; 3],
    pub error: [u8; 3],
    pub background: [u8; 3],
    pub surface_low: [u8; 3],
    pub surface: [u8; 3],
    pub surface_high: [u8; 3],
    pub surface_highest: [u8; 3],
    pub on_surface: [u8; 3],
    pub on_surface_variant: [u8; 3],
    pub outline: [u8; 3],
    pub outline_variant: [u8; 3],
}

impl Scheme {
    pub fn dark(source: [u8; 3]) -> Self {
        let s = Hct::from_rgb(source);
        let primary = Tonal {
            hue: s.hue,
            chroma: s.chroma.max(48.0),
        };
        let secondary = Tonal {
            hue: s.hue,
            chroma: (s.chroma - 32.0).max(s.chroma * 0.5).max(16.0),
        };
        let tertiary = Tonal {
            hue: sanitize_degrees(s.hue + 60.0),
            chroma: (s.chroma * 0.75).max(24.0),
        };
        let neutral = Tonal {
            hue: s.hue,
            chroma: (s.chroma / 8.0).clamp(4.0, 12.0),
        };
        let variant = Tonal {
            hue: s.hue,
            chroma: (s.chroma / 8.0 + 4.0).clamp(8.0, 16.0),
        };
        let error = Tonal {
            hue: 25.0,
            chroma: 84.0,
        };
        Scheme {
            primary: primary.tone(76.0),
            on_primary: primary.tone(20.0),
            primary_container: primary.tone(30.0),
            secondary: secondary.tone(78.0),
            tertiary: tertiary.tone(78.0),
            error: error.tone(72.0),
            background: neutral.tone(3.0),
            surface_low: neutral.tone(7.0),
            surface: neutral.tone(10.0),
            surface_high: neutral.tone(15.0),
            surface_highest: neutral.tone(21.0),
            on_surface: neutral.tone(92.0),
            on_surface_variant: variant.tone(64.0),
            outline: variant.tone(45.0),
            outline_variant: variant.tone(28.0),
        }
    }
}

/// `#rrggbb`
pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}
/// Parse `#rrggbb`.
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cam16_matches_reference_values() {
        // reference values from material-color-utilities' CAM16 tests
        let red = Cam16::from_rgb([0xff, 0x00, 0x00]);
        assert!(
            (red.j - 46.445).abs() < 0.01
                && (red.chroma - 113.357).abs() < 0.01
                && (red.hue - 27.408).abs() < 0.01,
            "{red:?}"
        );
        let blue = Cam16::from_rgb([0x00, 0x00, 0xff]);
        assert!(
            (blue.j - 25.465).abs() < 0.01
                && (blue.chroma - 87.230).abs() < 0.01
                && (blue.hue - 282.788).abs() < 0.01,
            "{blue:?}"
        );
        let green = Cam16::from_rgb([0x00, 0xff, 0x00]);
        assert!(
            (green.j - 79.331).abs() < 0.01
                && (green.chroma - 108.410).abs() < 0.01
                && (green.hue - 142.139).abs() < 0.01,
            "{green:?}"
        );
    }

    #[test]
    fn hct_round_trips() {
        for c in [
            [0x42, 0x85, 0xf4],
            [0x12, 0x80, 0x40],
            [0xc0, 0x40, 0xa0],
            [200, 200, 200],
            [0xff, 0, 0],
        ] {
            let h = Hct::from_rgb(c);
            let back = Hct::to_rgb(h.hue, h.chroma, h.tone);
            for i in 0..3 {
                assert!(
                    (back[i] as i32 - c[i] as i32).abs() <= 2,
                    "{c:?} → {back:?}"
                );
            }
        }
    }

    #[test]
    fn tones_are_monotonic_and_exact() {
        let p = Tonal {
            hue: 282.8,
            chroma: 48.0,
        };
        let mut last = -1.0;
        for t in [
            0.0, 10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0, 80.0, 90.0, 95.0, 100.0,
        ] {
            let l = lstar_from_rgb(p.tone(t));
            assert!((l - t).abs() < 1.0, "tone {t} → L* {l}");
            assert!(l > last);
            last = l;
        }
    }

    #[test]
    fn score_prefers_large_lively_hue_and_rejects_greys() {
        let colors = [
            ([10, 12, 30], 5000),
            ([70, 110, 190], 3000),
            ([230, 30, 40], 60),
            ([128, 128, 128], 4000),
        ];
        let s = score(&colors).unwrap();
        assert_eq!(s, [70, 110, 190]);
        assert_eq!(score(&[([128, 128, 128], 100), ([20, 20, 20], 100)]), None);
    }

    #[test]
    fn dark_scheme_shape() {
        let source = [0x42, 0x85, 0xf4];
        let s = Scheme::dark(source);
        let tone = |c: [u8; 3]| lstar_from_rgb(c);
        assert!(tone(s.background) < tone(s.surface_low) && tone(s.surface_low) < tone(s.surface));
        assert!(
            tone(s.surface) < tone(s.surface_high)
                && tone(s.surface_high) < tone(s.surface_highest)
        );
        assert!(tone(s.on_surface) > 88.0 && (tone(s.primary) - 76.0).abs() < 1.0);
        let p = Hct::from_rgb(s.primary);
        assert!(
            (p.hue - Hct::from_rgb(source).hue).abs() < 3.0,
            "primary keeps the source hue: {p:?}"
        );
        assert_eq!(parse_hex(&hex(s.primary)), Some(s.primary));
    }

    #[test]
    fn source_from_image() {
        let (w, h) = (160, 90);
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let c = if (40..120).contains(&x) && y > 10 {
                    [60, 100, 200]
                } else {
                    [8, 10, 20]
                };
                px.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        let s = source_color(&px, w, h, false).unwrap();
        let hct = Hct::from_rgb(s);
        assert!((250.0..300.0).contains(&hct.hue), "{hct:?}");
        assert_eq!(source_color(&[], 0, 0, false), None);
    }
}
