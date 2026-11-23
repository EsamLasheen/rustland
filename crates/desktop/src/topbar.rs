//! Top bar, pixel-for-pixel the legacy topbar.html (NibrasShell Topbar.qml):
//! left  [tray | network speed | active window]   right [monitors | workspaces]
//! Segments are joined: outer radius 12, inner radius 3, 2px seams.
use crate::render::{Align, Canvas, Color, Radii, Rect};
use crate::theme::{self, Tokens, CHIP_H, R, R_IN};

/// An RGBA image (app / tray icon).
#[derive(Clone)]
pub struct Image {
    pub rgba: std::rc::Rc<Vec<u8>>,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone)]
pub struct TrayIcon {
    pub id: String,
    pub tooltip: String,
    pub icon: Option<Image>,
}

/// Same icons, tooltips and pictures (programs re-send unchanged icons all the time).
pub fn same_tray(a: &[TrayIcon], b: &[TrayIcon]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.id == y.id
                && x.tooltip == y.tooltip
                && match (&x.icon, &y.icon) {
                    (Some(p), Some(q)) => p.w == q.w && p.h == q.h && p.rgba == q.rgba,
                    (None, None) => true,
                    _ => false,
                }
        })
}

#[derive(Clone, Default)]
pub struct Workspace {
    pub name: String,
    pub focused: bool,
    pub apps: Vec<(String, Option<Image>, u32)>,
}

#[derive(Clone, Default)]
pub struct State {
    pub tray: Vec<TrayIcon>,
    pub tray_expanded: bool,
    pub connected: bool,
    pub ssid: Option<String>,
    pub down_bps: Option<f64>,
    pub up_bps: Option<f64>,
    pub window_title: String,
    pub window_icon: Option<Image>,
    pub window_letter: Option<char>,
    pub battery: Option<(f32, bool)>, // percent, charging
    pub ram: Option<f32>,
    pub cpu: Option<f32>,
    pub workspaces: Vec<Workspace>,
    /// Running micro-animations (see `fx.rs`); default = still.
    pub fx: Fx,
    /// Show Rasto beside the meters (false while he is out walking).
    pub pet: bool,
    /// Rasto has had enough of being clicked.
    pub pet_angry: bool,
    /// Rasto is drawn in a window of his own over his spot: the bar only reserves the spot.
    pub pet_apart: bool,
}

/// Progress (0..1) of the bar's event animations at the moment of painting.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Fx {
    /// Energy wave refilling the battery ring while plugged in.
    pub charge: Option<f32>,
    /// Flash on reaching a full charge.
    pub full: Option<f32>,
    /// Low-battery warning pulse.
    pub low: Option<f32>,
    /// Workspace highlight sliding from this bar index.
    pub slide_from: Option<(usize, f32)>,
    /// The companion is blinking.
    pub blink: Option<f32>,
    /// The companion is hopping (workspace switch, click).
    pub hop: Option<f32>,
    /// Where he is looking, -1..1.
    pub look: f32,
    /// A short idle moment of the companion: (kind 0 breath, 1 tail flick, 2 look around,
    /// 3 stretch; progress 0..1).
    pub life: Option<(u8, f32)>,
}
/// Clickable regions produced by a paint, for hit testing.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Tray(String),
    TrayMore,
    Network,
    Monitor,
    Workspace(usize),
    Pet,
}

const TRAY_MAX: usize = 7;

fn rate(b: Option<f64>) -> String {
    let Some(mut b) = b else { return "—".into() };
    let u = ['B', 'K', 'M', 'G'];
    let mut i = 0;
    while b >= 1000. && i < 3 {
        b /= 1024.;
        i += 1;
    }
    if b < 10. && i > 0 {
        format!("{b:.1}{}", u[i])
    } else {
        format!("{}{}", b.round() as i64, u[i])
    }
}

/// Segment radii: first gets the outer-left radius, last the outer-right, the rest inner.
fn seg_radii(i: usize, n: usize) -> Radii {
    Radii {
        tl: if i == 0 { R } else { R_IN },
        bl: if i == 0 { R } else { R_IN },
        tr: if i + 1 == n { R } else { R_IN },
        br: if i + 1 == n { R } else { R_IN },
    }
}

fn letter_tile(c: &Canvas, r: Rect, letter: char, color: Color) {
    c.fill_round(r, Radii::all(4.), color.alpha(0.18));
    c.text(
        &letter.to_string(),
        r,
        &theme::label(9., 700),
        color,
        Align::Center,
    );
}

/// Meter colour for a load in percent: the accent while calm, turning to the warning colour
/// gradually between `from` and `to` (a hard switch at one value made the bar flicker between
/// two colours around it).
fn load_color(t: &Tokens, v: f32, from: f32, to: f32) -> Color {
    let p = ((v - from) / (to - from)).clamp(0., 1.);
    t.warning.mix(t.accent, p)
}

/// What Rasto's face is doing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RastoMood {
    pub blink: Option<f32>,
    pub grin: bool,
    pub angry: bool,
    pub sweat: bool,
    /// Tail on the left: he is walking left.
    pub flip: bool,
    /// Pupils toward this direction, -1..1 on each axis.
    pub gaze: (f32, f32),
    /// Walking: phase of his steps (radians).
    pub step: Option<f32>,
    /// Just landed: 1 flattens him.
    pub squash: f32,
    /// Tail swing, -1..1.
    pub wag: f32,
    /// The pointer is too close: wide eyes and a "!".
    pub scared: bool,
    /// A short idle moment on the bar: (kind, progress 0..1); see [`Fx::life`].
    pub life: Option<(u8, f32)>,
    /// A hop on the bar, progress 0..1: crouch, stretch in the air, squash on landing.
    pub hop: Option<f32>,
}

/// Rasto's outfit for the theme in use: each preset dresses him differently, and a wallpaper
/// accent (dynamic colours) gives him sparkles. His colours always come from the accent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RastoKit {
    Monarch = 0,
    Dracula,
    Catppuccin,
    Nord,
    TokyoNight,
    Gruvbox,
    RosePine,
    Solarized,
    Oxocarbon,
    Sparkle,
}

static RASTO_KIT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

impl RastoKit {
    pub fn of(preset: &str, dynamic: bool) -> Self {
        if dynamic {
            return Self::Sparkle;
        }
        let p = preset.to_ascii_lowercase();
        [
            ("dracula", Self::Dracula),
            ("catppuccin", Self::Catppuccin),
            ("nord", Self::Nord),
            ("tokyo", Self::TokyoNight),
            ("gruvbox", Self::Gruvbox),
            ("rosepine", Self::RosePine),
            ("solarized", Self::Solarized),
            ("oxocarbon", Self::Oxocarbon),
            ("gemini", Self::Sparkle),
        ]
        .into_iter()
        .find(|(k, _)| p.contains(k))
        .map_or(Self::Monarch, |(_, v)| v)
    }
    pub fn current() -> Self {
        match RASTO_KIT.load(std::sync::atomic::Ordering::Relaxed) {
            1 => Self::Dracula,
            2 => Self::Catppuccin,
            3 => Self::Nord,
            4 => Self::TokyoNight,
            5 => Self::Gruvbox,
            6 => Self::RosePine,
            7 => Self::Solarized,
            8 => Self::Oxocarbon,
            9 => Self::Sparkle,
            _ => Self::Monarch,
        }
    }
}

/// The theme changed: dress Rasto for it.
pub fn set_rasto_kit(preset: &str, dynamic: bool) {
    RASTO_KIT.store(RastoKit::of(preset, dynamic) as u8, std::sync::atomic::Ordering::Relaxed);
}

/// Body, horn and eye colours from the accent: a dark body tinted with it, glowing eyes in a
/// light shade of it (a few outfits keep their signature eye colour).
struct RastoPalette {
    top: Color,
    bottom: Color,
    belly: Color,
    horn: Color,
    eye: Color,
    rim: Color,
}

fn rasto_palette(accent: Color, kit: RastoKit) -> RastoPalette {
    let deep = Color::rgb(18, 8, 31);
    let white = Color::rgb(255, 255, 255);
    let eye = match kit {
        RastoKit::Dracula => Color::hex("#ff5555"),
        RastoKit::Gruvbox => Color::hex("#fe8019"),
        RastoKit::RosePine => Color::hex("#eb6f92"),
        RastoKit::Monarch => Color::hex("#9fd8ff"),
        _ => accent.mix(white, 0.62),
    };
    let horn = match kit {
        RastoKit::Nord => Color::hex("#e5f2f7"),
        RastoKit::Dracula => Color::hex("#f8f8f2"),
        _ => accent.mix(white, 0.72),
    };
    RastoPalette {
        top: accent.mix(deep, 0.46),
        bottom: accent.mix(deep, 0.2),
        belly: accent.mix(deep, 0.62).alpha(0.75),
        horn,
        eye,
        rim: accent.mix(white, 0.8),
    }
}

/// Thin line as a quad (the canvas has no stroked lines).
fn rasto_line(c: &Canvas, a: (f32, f32), b: (f32, f32), w: f32, col: Color) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.hypot(dy).max(0.001);
    let (nx, ny) = (-dy / len * w / 2., dx / len * w / 2.);
    c.fill_poly(&[(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)], col);
}

/// Four-point star.
fn rasto_star(c: &Canvas, x: f32, y: f32, r: f32, col: Color) {
    let k = r * 0.28;
    c.fill_poly(&[(x, y - r), (x + k, y - k), (x + r, y), (x + k, y + k), (x, y + r), (x - k, y + k), (x - r, y), (x - k, y - k)], col);
}

/// Rasto (راستو), the bar's little villain: a round imp with horns, a tail, slanted glowing eyes
/// and one fang. His colours follow the theme's accent (so a wallpaper's colours too), and each
/// theme gives him an outfit. He breathes, flicks his tail, looks around and stretches in short
/// idle moments, hops with anticipation and a squash on landing, and sweats when the CPU is busy.
pub fn paint_rasto(c: &Canvas, cx: f32, top: f32, t: &Tokens, mood: RastoMood) {
    use std::f32::consts::PI;
    let kit = RastoKit::current();
    let pal = rasto_palette(t.accent, kit);
    let white = Color::rgba(255, 255, 255, 1.);
    let tail = if mood.flip { -1. } else { 1. };

    // ── motion ──
    // squash > 0 flattens (crouch, landing), < 0 stretches (in the air, a stretch); feet stay put
    let mut sq = mood.squash.clamp(0., 1.);
    let mut lift = 0.;
    if let Some(p) = mood.hop {
        if p < 0.18 {
            sq = (p / 0.18 * PI / 2.).sin() * 0.7;
        } else if p < 0.82 {
            let q = (p - 0.18) / 0.64;
            lift = (q * PI).sin() * 5.;
            sq = -(1. - (2. * q - 1.).abs()) * 0.55;
        } else {
            sq = ((p - 0.82) / 0.18 * PI).sin() * 0.8;
        }
    }
    let mut wag = mood.wag;
    let mut gaze = mood.gaze;
    let mut breath = 0.;
    let mut perk = 0.;
    if let Some((kind, p)) = mood.life {
        breath = (p * PI).sin();
        match kind {
            // tail flick: a damped double swing
            1 => wag += (p * 4. * PI).sin() * (1. - p) * 1.2,
            // a slow look around
            2 => gaze = ((p * 2. * PI).sin(), -0.35 * breath),
            // a stretch: taller, horns perked
            3 => {
                sq -= breath * 0.6;
                perk = breath;
            }
            _ => {}
        }
    }
    let feet = top + 15.;
    let top = top + 3. * sq - lift - breath * 0.6;
    let (bw, bh) = (18. + 2.5 * sq + breath * 0.4, 15.5 - 3. * sq + breath * 0.6);
    let lean = gaze.0 * 0.6;

    // ── behind the body ──
    // soft shadow, smaller while he is in the air
    let shw = 15. * (1. - lift / 12.);
    c.fill_round(
        Rect::new(cx - shw / 2., feet + 1.4, shw, 2.6),
        Radii::all(1.3),
        Color::rgba(0, 0, 0, 0.32 * (1. - lift / 9.)),
    );
    match kit {
        RastoKit::Solarized => {
            // sun halo with turning rays
            let spin = mood.life.map_or(0., |(_, p)| p * PI / 4.);
            let (hx, hy) = (cx + lean, top + 6.);
            c.fill_circle(hx, hy, 12.5, Color::hex("#ffd36e").alpha(0.1));
            c.fill_circle(hx, hy, 10.5, Color::hex("#ffd36e").alpha(0.08));
            for i in 0..8 {
                let a = spin + i as f32 * PI / 4.;
                let (s, co) = a.sin_cos();
                rasto_line(c, (hx + co * 11., hy + s * 11.), (hx + co * 13.5, hy + s * 13.5), 1.1, Color::hex("#f5c542").alpha(0.85));
            }
        }
        RastoKit::Dracula => {
            // cape with a high collar behind the head
            let cape = Color::hex("#2b0a12");
            let lining = Color::hex("#ff5555").alpha(0.85);
            for m in [-1., 1.] {
                c.fill_poly(&[(cx + m * 5., top + 4.), (cx + m * 12., top - 3. + wag * 0.3), (cx + m * 10.5, top + 9.)], lining);
                c.fill_poly(&[(cx + m * 6., top + 5.), (cx + m * 11., top - 1.), (cx + m * 10., top + 9.)], cape);
            }
        }
        _ => {}
    }
    // tail with an arrow tip; the tip trails the swing (secondary motion)
    let w = wag * 1.6;
    let tw = w * 1.35;
    c.fill_poly(
        &[
            (cx + tail * 6., top + 10.),
            (cx + tail * 10., top + 10.6 + w * 0.3),
            (cx + tail * 12.2, top + 8.2 + tw),
            (cx + tail * 13.2, top + 9.2 + tw),
            (cx + tail * 11.2, top + 12.3 + w * 0.4),
            (cx + tail * 6., top + 13.5),
        ],
        pal.bottom,
    );
    c.fill_poly(
        &[(cx + tail * 10.2, top + 8.6 + tw), (cx + tail * 14.3, top + 5.4 + tw), (cx + tail * 13.7, top + 10.1 + tw)],
        pal.horn,
    );
    // horns (cat ears for Catppuccin); they lag a squash and perk up in a stretch
    let hlag = sq * 0.9 - perk * 1.2;
    for m in [-1., 1.] {
        if kit == RastoKit::Catppuccin {
            c.fill_poly(&[(cx + m * 8., top + 4.), (cx + m * 7.5 + lean, top - 4. + hlag), (cx + m * 2.5, top + 1.)], pal.bottom);
            c.fill_poly(&[(cx + m * 6.8, top + 2.8), (cx + m * 6.8 + lean, top - 1.8 + hlag), (cx + m * 3.8, top + 1.2)], Color::hex("#f5c2e7"));
        } else {
            // slim devil horns curving outward, with a shaded inner edge for volume
            let tip = (cx + m * 9.6 + lean, top - 4.4 + hlag);
            c.fill_poly(&[(cx + m * 7.2, top + 3.4), (cx + m * 8.9, top - 0.6 + hlag * 0.5), tip, (cx + m * 7.4, top - 0.9 + hlag * 0.5), (cx + m * 4.4, top + 1.6)], pal.horn);
            c.fill_poly(&[(cx + m * 5.6, top + 2.2), (cx + m * 7.4, top - 0.9 + hlag * 0.5), tip, (cx + m * 6.6, top + 0.4)], pal.horn.mix(Color::rgb(0, 0, 0), 0.68));
        }
    }
    // feet take turns while he walks
    for m in [-1., 1.] {
        let up = mood.step.map_or(0., |p| {
            let side = if m < 0. { p } else { p + PI };
            side.sin().max(0.) * 2.2
        });
        c.fill_circle(cx + m * 4.5, feet - up - lift, 2.3, pal.bottom);
        c.fill_circle(cx + m * 4.5 - 0.5, feet - up - lift - 0.7, 0.8, pal.top.alpha(0.6));
    }

    // ── body ──
    let body = Rect::new(cx - bw / 2. + lean * 0.3, top, bw, bh);
    let round = Radii { tl: 9., tr: 9., br: 7., bl: 7. };
    c.fill_round_vgradient(body, round, &[(0., pal.top), (1., pal.bottom)]);
    c.fill_round(Rect::new(cx - 5. + lean * 0.3, top + bh - 6.2, 10., 5.), Radii::all(2.5), pal.belly);
    c.bevel(body, round);
    c.stroke_round(body, round, pal.rim.alpha(0.5), 0.8);
    // a glossy highlight on the crown of the head
    c.fill_round(Rect::new(cx - 5.5 + lean, top + 1.2, 6., 2.), Radii::all(1.), white.alpha(0.16));

    // ── face ──
    let ex = cx + lean;
    let ey = top + 6.5;
    let open = mood.blink.map_or(1., |p| (2. * p - 1.).abs());
    let (gx, gy) = gaze;
    if kit == RastoKit::TokyoNight {
        let v = Rect::new(ex - 8., ey - 2.6, 16., 5.4);
        c.fill_round(v, Radii::all(2.7), Color::hex("#0b0f1f"));
        c.stroke_round(v, Radii::all(2.7), pal.eye.alpha(0.9), 0.8);
    }
    for m in [-1., 1.] {
        if open < 0.4 {
            c.fill_poly(&[(ex + m * 6.8, ey - 1.), (ex + m * 1.6, ey + 1.1), (ex + m * 1.6, ey + 2.), (ex + m * 6.8, ey)], pal.eye);
        } else if mood.scared {
            c.fill_circle(ex + m * 4., ey, 3., white);
            c.fill_circle(ex + m * 4. + gx * 1.2, ey + gy * 1.2, 1.3, Color::rgb(20, 8, 34));
            c.fill_circle(ex + m * 4. + gx * 1.2 - 0.5, ey + gy * 1.2 - 0.5, 0.45, white);
        } else {
            // glow, the slanted eye, a dark pupil and a catchlight that make him look alive
            c.fill_circle(ex + m * 4.2, ey + 0.6, 3.9, pal.eye.alpha(0.2));
            c.fill_poly(&[(ex + m * 7., ey - 1.8), (ex + m * 1.5, ey + 0.8), (ex + m * 2.3, ey + 2.6), (ex + m * 6.2, ey + 2.)], pal.eye);
            let (px, py) = (ex + m * 4. + gx * 1.1, ey + 1.1 + gy * 0.5);
            c.fill_circle(px, py, 0.95, Color::rgb(24, 6, 22));
            c.fill_circle(px - 0.45, py - 0.5, 0.42, white.alpha(0.95));
        }
    }
    if kit == RastoKit::Oxocarbon {
        // a monocle on his right eye, on a little chain
        let (mx, my) = (ex + 4.2, ey + 0.6);
        c.stroke_round(Rect::new(mx - 3.6, my - 3.6, 7.2, 7.2), Radii::all(3.6), Color::hex("#dde1e6"), 0.9);
        rasto_line(c, (mx + 2.6, my + 2.6), (mx + 5., my + 8.), 0.6, Color::hex("#dde1e6").alpha(0.8));
    }
    // cheeks
    for m in [-1., 1.] {
        c.fill_circle(ex + m * 6.3, ey + 3.4, 1.3, Color::hex("#ff6b9a").alpha(0.28));
    }
    // smirk, higher on his right, with one fang (two for Dracula); wider when grinning
    let my = top + 11.2;
    if mood.scared {
        c.fill_circle(ex, my, 1.6, white);
    } else {
        let wide = if mood.grin { 1.5 } else { 0. };
        c.fill_poly(&[(ex - 3.5 - wide, my + 0.2), (ex + 4.5 + wide, my - 1.3), (ex + 4.5 + wide, my - 0.5), (ex - 3.5 - wide, my + 1.)], white);
        c.fill_poly(&[(ex + 1.2, my - 0.5), (ex + 3.6, my - 1.), (ex + 2.6, my + 2.)], white);
        if kit == RastoKit::Dracula {
            c.fill_poly(&[(ex - 2.6, my), (ex - 0.6, my - 0.4), (ex - 1.5, my + 2.4)], white);
        }
    }
    if kit == RastoKit::Catppuccin {
        for m in [-1., 1.] {
            for k in [-1., 0., 1.] {
                rasto_line(c, (ex + m * 7.2, my - 0.8 + k * 0.9), (ex + m * 11.5, my - 1.4 + k * 1.8), 0.45, white.alpha(0.6));
            }
        }
    }

    // ── outfit on top ──
    match kit {
        RastoKit::Monarch => {
            // a small floating crown and shadow wisps rising at his feet
            let cy = top - 5.2 - breath * 0.8;
            let gold = Color::hex("#ffd36e").mix(t.accent, 0.8);
            c.fill_poly(&[(ex - 4., cy + 3.), (ex - 4.2, cy - 1.), (ex - 2., cy + 1.), (ex, cy - 2.), (ex + 2., cy + 1.), (ex + 4.2, cy - 1.), (ex + 4., cy + 3.)], gold);
            c.fill_circle(ex, cy - 2.2, 0.7, pal.eye);
            let rise = mood.life.map_or(0., |(_, p)| p);
            for (i, dx) in [-6., 0.5, 6.].into_iter().enumerate() {
                let q = (rise + i as f32 * 0.33) % 1.;
                c.fill_circle(cx + dx, feet + 1. - q * 5., 1.4 * (1. - q), t.accent.alpha(0.35 * (1. - q)));
            }
        }
        RastoKit::Nord => {
            // a knitted scarf whose end flutters with his tail
            let sy = top + bh - 4.2;
            let wool = Color::hex("#88c0d0");
            c.fill_round(Rect::new(cx - bw / 2. + 1. + lean * 0.3, sy, bw - 2., 2.6), Radii::all(1.3), wool);
            for k in 0..3 {
                c.fill_rect(Rect::new(cx - 5. + k as f32 * 4. + lean * 0.3, sy, 1.2, 2.6), Color::hex("#eceff4").alpha(0.7));
            }
            let f = -tail * (2. + wag.abs());
            c.fill_poly(&[(cx - tail * 4., sy + 2.), (cx - tail * 6. + f * 0.5, sy + 6.), (cx - tail * 3.5 + f * 0.3, sy + 6.5), (cx - tail * 2., sy + 2.4)], wool);
        }
        RastoKit::Gruvbox => {
            // a sprout on his head that sways
            let sx = ex - 1. + wag * 0.6;
            rasto_line(c, (ex - 1., top + 0.6), (sx, top - 3.5), 0.8, Color::hex("#98971a"));
            c.fill_poly(&[(sx, top - 3.5), (sx + 3.6, top - 5.6), (sx + 1.2, top - 2.2)], Color::hex("#b8bb26"));
            c.fill_poly(&[(sx, top - 3.), (sx - 2.8, top - 4.6), (sx - 0.8, top - 1.8)], Color::hex("#b8bb26").alpha(0.85));
        }
        RastoKit::RosePine => {
            // a rose tucked on his left horn
            let (rx, ry) = (ex - 6.8, top - 0.4);
            c.fill_poly(&[(rx + 1., ry + 1.), (rx + 3.8, ry + 2.6), (rx + 1.6, ry + 3.)], Color::hex("#31748f"));
            c.fill_circle(rx, ry, 2.1, Color::hex("#eb6f92"));
            c.fill_circle(rx + 0.4, ry - 0.3, 1.1, Color::hex("#ebbcba"));
        }
        RastoKit::TokyoNight => {
            // antenna with a blinking light
            rasto_line(c, (ex + 3., top + 0.8), (ex + 5., top - 3.5), 0.6, pal.rim.alpha(0.8));
            let on = mood.life.map_or(0.5, |(_, p)| (p * 6. * PI).sin() * 0.5 + 0.5);
            c.fill_circle(ex + 5., top - 3.8, 1.2, pal.eye.alpha(0.4 + 0.6 * on));
        }
        RastoKit::Sparkle => {
            // stars in the wallpaper's colours circling him
            let spin = mood.life.map_or(0., |(_, p)| p * 2. * PI) + mood.step.unwrap_or(0.) * 0.3;
            for i in 0..3 {
                let a = spin + i as f32 * 2. * PI / 3.;
                let (s, co) = a.sin_cos();
                let r = 1.6 + 0.8 * ((a * 2.).sin() * 0.5 + 0.5);
                rasto_star(c, cx + co * 12., top + 6. + s * 7., r, pal.eye.mix(white, 0.7).alpha(0.9));
            }
        }
        _ => {}
    }

    // busy CPU: a drop of sweat
    if mood.sweat {
        c.fill_circle(cx - 10., top + 3., 1.6, Color::rgba(140, 215, 255, 0.95));
        c.fill_poly(&[(cx - 11.4, top + 2.6), (cx - 10., top), (cx - 8.6, top + 2.6)], Color::rgba(140, 215, 255, 0.95));
    }
    // annoyed: brows pressed down and steam from the horns
    if mood.angry && !mood.scared {
        for m in [-1., 1.] {
            c.fill_poly(&[(ex + m * 7.5, ey - 3.2), (ex + m * 1.2, ey - 0.4), (ex + m * 1.2, ey + 0.6), (ex + m * 7.5, ey - 2.)], Color::rgba(20, 8, 34, 1.));
            c.fill_circle(cx + m * 10.5, top - 1.5, 1.5, Color::rgba(255, 255, 255, 0.7));
        }
    }
    if mood.scared {
        c.fill_rect(Rect::new(cx + 11., top - 4., 1.8, 5.), Color::rgba(255, 225, 120, 1.));
        c.fill_circle(cx + 11.9, top + 2.8, 1., Color::rgba(255, 225, 120, 1.));
    }
}

/// Paint the bar for a width (DIPs). Returns hit regions.
pub fn paint(c: &Canvas, width: f32, t: &Tokens, s: &State) -> Vec<(Rect, Hit)> {
    let mut hits = Vec::new();
    let y = (theme::BAR_H - CHIP_H) / 2.;
    let h = CHIP_H;
    let left_max = width / 2. - 206.; // keep clear of the centred capsule

    // ── left: measure segments first (they are joined, so radii depend on the count) ──
    enum Seg {
        Tray(Vec<TrayIcon>, Option<usize>),
        Net,
        Win,
    }
    let mut segs: Vec<(Seg, f32)> = Vec::new();
    let icons: Vec<TrayIcon> = s.tray.clone();
    if !icons.is_empty() {
        let shown = if s.tray_expanded {
            icons.len()
        } else {
            icons.len().min(TRAY_MAX)
        };
        let more = if icons.len() > TRAY_MAX {
            Some(icons.len() - TRAY_MAX)
        } else {
            None
        };
        let more_w = more
            .map(|n| {
                c.measure(
                    &if s.tray_expanded {
                        "‹".into()
                    } else {
                        format!("+{n}")
                    },
                    &theme::label(11., 700),
                    0.,
                ) + 8.
            })
            .unwrap_or(0.);
        let w = 12.
            + shown as f32 * 22.
            + shown.saturating_sub(1) as f32 * 2.
            + if more.is_some() { 2. + more_w } else { 0. }
            + 2.;
        segs.push((Seg::Tray(icons[..shown].to_vec(), more), w));
    }
    let ssid_w = s
        .ssid
        .as_ref()
        .map(|n| c.measure(n, &theme::body(11., 400), 0.).min(90.) + 6.)
        .unwrap_or(0.);
    let rate_w =
        |v: Option<f64>| (11. + 2. + c.measure(&rate(v), &theme::label(12., 700), 0.)).max(50.);
    let net_w =
        2. + (9. + 14. + ssid_w + 9.) + 8. + rate_w(s.down_bps) + 8. + rate_w(s.up_bps) + 10.;
    // small screens: the network speeds go before the title would run under the capsule
    let tray_w: f32 = segs.iter().map(|(_, w)| w + 2.).sum();
    if tray_w + net_w + 2. + 120. <= left_max {
        segs.push((Seg::Net, net_w));
    }
    let used: f32 = segs.iter().map(|(_, w)| w).sum::<f32>() + 2. * segs.len() as f32;
    let title_w = c.measure(&s.window_title, &theme::body(11.5, 600), 0.);
    let win_w = (4. + 22. + 8. + title_w + 12.)
        .clamp(120., 380.)
        .min((left_max - used).max(120.));
    segs.push((Seg::Win, win_w));

    let n = segs.len();
    let mut x = 8.;
    for (i, (seg, w)) in segs.into_iter().enumerate() {
        let r = Rect::new(x, y, w, h);
        let k = seg_radii(i, n);
        match seg {
            Seg::Tray(list, more) => {
                c.chip(r, k, t.surface, Some(t.line_soft));
                let mut bx = x + 6.;
                for ic in &list {
                    let br = Rect::new(bx, y + 2., 22., 20.);
                    if let Some(img) = &ic.icon {
                        c.image(
                            &img.rgba,
                            img.w,
                            img.h,
                            Rect::new(bx + 3.5, y + 4.5, 15., 15.),
                            1.,
                        );
                    } else {
                        letter_tile(
                            c,
                            Rect::new(bx + 3., y + 4., 16., 16.),
                            ic.tooltip.chars().next().unwrap_or('?'),
                            t.muted,
                        );
                    }
                    hits.push((br, Hit::Tray(ic.id.clone())));
                    bx += 24.;
                }
                if let Some(m) = more {
                    let label = if s.tray_expanded {
                        "‹".to_string()
                    } else {
                        format!("+{m}")
                    };
                    let mw = c.measure(&label, &theme::label(11., 700), 0.) + 8.;
                    let mr = Rect::new(bx, y + 2., mw, 20.);
                    c.text(&label, mr, &theme::label(11., 700), t.muted, Align::Center);
                    hits.push((mr, Hit::TrayMore));
                }
            }
            Seg::Net => {
                c.chip(r, k, t.accent.mix(t.surface, 0.16), Some(t.line_soft));
                // inner surface pill (rounded like the segment on the left, inner radius on the right)
                let pill = Rect::new(x + 2., y + 2., 9. + 14. + ssid_w + 9., h - 4.);
                c.fill_round(
                    pill,
                    Radii {
                        tl: k.tl,
                        bl: k.bl,
                        tr: R_IN,
                        br: R_IN,
                    },
                    t.surface_solid,
                );
                c.icon(
                    if s.connected { "wifi" } else { "wifiOff" },
                    Rect::new(pill.x + 9., y + 5., 14., 14.),
                    t.accent,
                    1.8,
                );
                if let Some(ssid) = &s.ssid {
                    c.text(
                        ssid,
                        Rect::new(pill.x + 9. + 14. + 6., y, ssid_w - 6., h),
                        &theme::body(11., 400),
                        t.muted,
                        Align::Start,
                    );
                }
                let mut rx = pill.right() + 8.;
                for (ic, v) in [("down", s.down_bps), ("up", s.up_bps)] {
                    c.icon(ic, Rect::new(rx, y + 6.5, 11., 11.), t.accent, 2.4);
                    c.text(
                        &rate(v),
                        Rect::new(rx + 13., y, rate_w(v) - 13., h),
                        &theme::label(12., 700),
                        t.text,
                        Align::Start,
                    );
                    rx += rate_w(v) + 8.;
                }
                hits.push((r, Hit::Network));
            }
            Seg::Win => {
                c.fill_round(r, k, t.accent.alpha(0.72));
                let ico = Rect::new(x + 4., y + 3., 22., 18.);
                c.fill_round(ico, Radii::all(6.), Color::rgba(255, 255, 255, 0.22));
                match (&s.window_icon, s.window_letter) {
                    (Some(img), _) => c.image(
                        &img.rgba,
                        img.w,
                        img.h,
                        Rect::new(ico.x + 3., ico.y + 1., 16., 16.),
                        1.,
                    ),
                    (None, Some(l)) => c.text(
                        &l.to_string(),
                        ico,
                        &theme::label(11., 700),
                        t.bg,
                        Align::Center,
                    ),
                    (None, None) => c.icon(
                        "athar",
                        Rect::new(ico.x + 4.5, ico.y + 2.5, 13., 13.),
                        t.bg,
                        1.8,
                    ),
                }
                c.text(
                    &s.window_title,
                    Rect::new(x + 34., y, w - 34. - 12., h),
                    &theme::body(11.5, 600),
                    t.bg,
                    Align::Start,
                );
            }
        }
        x += w + 2.;
    }

    // ── right (laid out from the right edge): [monitors][gap 6][workspaces] ──
    let mut rx = width - 8.;
    // workspaces
    let ws_w: Vec<f32> = s
        .workspaces
        .iter()
        .map(|w| {
            if w.apps.is_empty() {
                (14. + c.measure(&w.name, &theme::label(12., 700), 0.)).max(26.)
            } else {
                let k = w.apps.len().min(4) as f32;
                (14. + k * 16. + (k - 1.) * 6.).max(26.)
            }
        })
        .collect();
    let total: f32 = ws_w.iter().sum::<f32>() + 2. * ws_w.len().saturating_sub(1) as f32;
    let mut wx = rx - total;
    let nws = s.workspaces.len();
    // workspace switch: the highlight slides from the old workspace to the new one
    let slide = s.fx.slide_from.and_then(|(from, p)| {
        let to = s.workspaces.iter().position(|w| w.focused)?;
        (from != to && from < nws).then_some((from, to, p))
    });
    if let Some((from, to, p)) = slide {
        let xs: Vec<f32> = ws_w
            .iter()
            .scan(wx, |x, w| {
                let at = *x;
                *x += w + 2.;
                Some(at)
            })
            .collect();
        for (i, w) in ws_w.iter().enumerate() {
            c.chip(
                Rect::new(xs[i], y, *w, h),
                seg_radii(i, nws),
                t.surface,
                Some(t.line_soft),
            );
        }
        let lerp = |a: f32, b: f32| a + (b - a) * p;
        let r = Rect::new(lerp(xs[from], xs[to]), y, lerp(ws_w[from], ws_w[to]), h);
        let k = seg_radii(to, nws);
        // a short accent trail behind the moving highlight
        let trail = Rect::new(
            r.x.min(lerp(xs[from], r.x)),
            y + 2.,
            (r.x - xs[from]).abs() + r.w,
            h - 4.,
        );
        c.fill_round(trail, Radii::all(R), t.accent.alpha(0.18 * (1. - p)));
        c.fill_round(r, k, t.accent);
        c.bevel(r, k);
    }
    for (i, (ws, w)) in s.workspaces.iter().zip(&ws_w).enumerate() {
        let r = Rect::new(wx, y, *w, h);
        let k = seg_radii(i, nws);
        let lit = ws.focused && slide.is_none_or(|(_, _, p)| p > 0.5);
        let fg = if lit { t.bg } else { t.muted };
        if slide.is_some() {
            // backgrounds already drawn above
        } else if ws.focused {
            // --glow: soft accent halo
            for (grow, a) in [(4., 0.10), (2., 0.18)] {
                c.fill_round(
                    Rect::new(r.x - grow, r.y - grow, r.w + 2. * grow, r.h + 2. * grow),
                    Radii {
                        tl: k.tl + grow,
                        tr: k.tr + grow,
                        br: k.br + grow,
                        bl: k.bl + grow,
                    },
                    t.accent.alpha(a),
                );
            }
            c.fill_round(r, k, t.accent);
            c.bevel(r, k);
        } else {
            c.chip(r, k, t.surface, Some(t.line_soft));
        }
        if ws.apps.is_empty() {
            c.text(&ws.name, r, &theme::label(12., 700), fg, Align::Center);
        } else {
            let k2 = ws.apps.len().min(4) as f32;
            let mut ax = r.x + (r.w - (k2 * 16. + (k2 - 1.) * 6.)) / 2.;
            for (exe, img, count) in ws.apps.iter().take(4) {
                let ar = Rect::new(ax, y + 4., 16., 16.);
                match img {
                    Some(im) => c.image(&im.rgba, im.w, im.h, ar, 1.),
                    None => letter_tile(
                        c,
                        ar,
                        exe.chars().next().unwrap_or('?').to_ascii_uppercase(),
                        fg,
                    ),
                }
                if *count > 1 {
                    let label = count.to_string();
                    let bw = (c.measure(&label, &theme::label(8.5, 700), 0.) + 4.).max(11.);
                    let br = Rect::new(ar.right() + 5. - bw, ar.bottom() + 4. - 11., bw, 11.);
                    c.fill_round(
                        Rect::new(br.x - 1., br.y - 1., br.w + 2., br.h + 2.),
                        Radii::all(4.),
                        t.surface_solid,
                    );
                    c.fill_round(br, Radii::all(3.), if lit { t.bg } else { t.accent });
                    c.text(
                        &label,
                        br,
                        &theme::label(8.5, 700),
                        if lit { t.accent } else { t.bg },
                        Align::Center,
                    );
                }
                ax += 22.;
            }
        }
        hits.push((r, Hit::Workspace(i)));
        wx += w + 2.;
    }
    rx -= total + 6.;

    // monitors: [battery] | RAM | CPU — ring with icon inside + value, thin dividers
    let mut cells: Vec<(&str, f32, String, Color)> = Vec::new();
    if let Some((p, charging)) = s.battery {
        let low = p < 20. && !charging;
        cells.push((
            if charging { "plug" } else { "battery" },
            p / 100.,
            format!("{}%", p.round()),
            if low {
                t.warning
            } else if charging {
                t.success
            } else {
                t.muted
            },
        ));
    }
    if let Some(v) = s.ram {
        cells.push((
            "ram",
            v / 100.,
            format!("{}%", v.round()),
            load_color(t, v, 70., 92.),
        ));
    }
    if let Some(v) = s.cpu {
        cells.push((
            "cpu",
            v / 100.,
            format!("{}%", v.round()),
            load_color(t, v, 60., 88.),
        ));
    }
    if !cells.is_empty() {
        let cell_w: Vec<f32> = cells
            .iter()
            .map(|(_, _, l, _)| 20. + 5. + c.measure(l, &theme::label(12.5, 700), 0.).max(28.))
            .collect();
        let mw = 20. + cell_w.iter().sum::<f32>() + (cells.len() - 1) as f32 * (7. + 1. + 7.);
        let mr = Rect::new(rx - mw, y, mw, h);
        c.chip(mr, Radii::all(R), t.surface, Some(t.line_soft));
        let mut cx = mr.x + 10.;
        for (i, ((ic, v, label, col), cw)) in cells.iter().zip(&cell_w).enumerate() {
            if i > 0 {
                c.fill_rect(Rect::new(cx, y + 6., 1., 12.), t.accent.alpha(0.30));
                cx += 1. + 7.;
            }
            let (rx0, ry0) = (cx + 10., y + h / 2.);
            let battery = i == 0 && s.battery.is_some();
            let fx = s.fx;
            // battery HUD: halo behind the ring (charging wave, full flash, low warning)
            let halo = |p: f32, grow: f32, a: f32, col: Color| {
                c.fill_circle(
                    rx0,
                    ry0,
                    9. + grow * crate::fx::ease(p),
                    col.alpha(a * (1. - p)),
                );
            };
            match (battery, fx.full, fx.charge, fx.low) {
                (true, Some(p), _, _) => halo(p, 6., 0.55, t.success),
                (true, _, Some(p), _) => halo(p, 4., 0.30, t.success),
                (true, _, _, Some(p)) => halo(p, 5., 0.45, t.warning),
                _ => {}
            }
            if !battery && *v > 0.6 {
                let heat = ((*v - 0.6) / 0.4).clamp(0., 1.);
                c.fill_circle(rx0, ry0, 10.5 + 2.5 * heat, col.alpha(0.10 + 0.22 * heat));
            }
            match (battery, fx.charge) {
                (true, Some(p)) => {
                    // energy refills the ring from empty to the charge level, a spark at its head
                    let e = crate::fx::ease(p);
                    c.ring(rx0, ry0, 20., 2.4, *v * e, t.faint, *col);
                    let a = std::f32::consts::TAU * (*v * e);
                    let r0 = (20. - 2.4) / 2.;
                    c.fill_circle(
                        rx0 + r0 * a.sin(),
                        ry0 - r0 * a.cos(),
                        2.2,
                        Color::rgba(255, 255, 255, 0.9 * (1. - p * 0.6)),
                    );
                }
                _ => c.ring(rx0, ry0, 20., 2.4, *v, t.faint, *col),
            }
            c.icon(ic, Rect::new(cx + 5., y + 7., 10., 10.), *col, 2.2);
            c.text(
                label,
                Rect::new(cx + 25., y, cw - 25., h),
                &theme::label(12.5, 700),
                *col,
                Align::Start,
            );
            cx += cw + 7.;
        }
        hits.push((mr, Hit::Monitor));
        let pet = Rect::new(mr.x - 30., y, 24., h);
        if s.pet && pet.x > width / 2. + 206. {
            let mood = RastoMood {
                blink: s.fx.blink,
                grin: s.fx.hop.is_some(),
                angry: s.pet_angry,
                sweat: s.cpu.is_some_and(|v| v >= 85.),
                gaze: (s.fx.look, 0.),
                life: s.fx.life,
                hop: s.fx.hop,
                ..Default::default()
            };
            if !s.pet_apart {
                paint_rasto(c, pet.x + pet.w / 2., pet.y + 5.5, t, mood);
            }
            hits.push((pet, Hit::Pet));
        }
    }
    hits
}

/// Representative data (matches the legacy bar captured on 2026-10-14) for snapshots.
pub fn sample() -> State {
    let ws = |name: &str, focused: bool, apps: &[(&str, u32)]| Workspace {
        name: name.into(),
        focused,
        apps: apps
            .iter()
            .map(|(e, n)| (e.to_string(), None, *n))
            .collect(),
    };
    State {
        tray: [
            "Bluetooth",
            "Malwarebytes",
            "WhatsApp",
            "OneDrive",
            "Steam",
            "Google Drive",
            "NVIDIA",
            "Discord",
            "Opera",
        ]
        .iter()
        .map(|n| TrayIcon {
            id: n.to_string(),
            tooltip: n.to_string(),
            icon: None,
        })
        .collect(),
        tray_expanded: false,
        connected: true,
        ssid: Some("HomeNetwork".into()),
        down_bps: Some(1024. * 1.0),
        up_bps: Some(0.),
        window_title: "Visual Studio Code".into(),
        window_icon: None,
        window_letter: Some('C'),
        battery: Some((80., false)),
        ram: Some(80.),
        cpu: Some(7.),
        fx: Fx::default(),
        pet: true,
        pet_angry: false,
        pet_apart: false,
        workspaces: vec![
            ws("1", false, &[]),
            ws("2", false, &[("brave", 1), ("msedge", 1)]),
            ws("3", false, &[("Obsidian", 1)]),
            ws(
                "4",
                true,
                &[("Code", 1), ("WindowsTerminal", 2), ("WhatsApp", 1)],
            ),
            ws(
                "5",
                false,
                &[("firefox", 1), ("explorer", 1), ("soffice", 1)],
            ),
        ],
    }
}

#[cfg(test)]
mod rasto_sheet {
    use super::*;

    /// Every outfit in a few poses, as a PNG at RASTO_SHEET (run with --ignored to look at him).
    #[test]
    #[ignore]
    fn rasto_contact_sheet() {
        let Ok(out) = std::env::var("RASTO_SHEET") else { return };
        let kits = [
            ("ShadowMonarch", "#7c5cff", false),
            ("DraculaDark", "#bd93f9", false),
            ("CatppuccinDark", "#cba6f7", false),
            ("NordDark", "#88c0d0", false),
            ("TokyoNightDark", "#7aa2f7", false),
            ("GruvboxDark", "#fabd2f", false),
            ("RosePineDark", "#ebbcba", false),
            ("SolarizedDark", "#268bd2", false),
            ("OxocarbonDark", "#78a9ff", false),
            ("wallpaper", "#3fbf7f", true),
        ];
        let poses: [RastoMood; 6] = [
            RastoMood::default(),
            RastoMood { life: Some((0, 0.5)), ..Default::default() },
            RastoMood { life: Some((2, 0.25)), grin: true, ..Default::default() },
            RastoMood { hop: Some(0.5), grin: true, ..Default::default() },
            RastoMood { angry: true, sweat: true, ..Default::default() },
            RastoMood { scared: true, gaze: (0.8, -0.5), ..Default::default() },
        ];
        let (cw, ch) = (44., 40.);
        let r = crate::render::snapshot(
            std::path::Path::new(&out),
            (cw * poses.len() as f32) as u32,
            (ch * kits.len() as f32) as u32,
            4.,
            |c| {
                c.fill_rect(Rect::new(0., 0., cw * 6., ch * 10.), Color::hex("#0b1020"));
                for (row, (name, accent, dynamic)) in kits.iter().enumerate() {
                    set_rasto_kit(name, *dynamic);
                    let t = Tokens::new(Color::hex(accent));
                    for (col, m) in poses.iter().enumerate() {
                        paint_rasto(c, col as f32 * cw + cw / 2., row as f32 * ch + 14., &t, *m);
                    }
                }
            },
        );
        r.unwrap();
    }
}
