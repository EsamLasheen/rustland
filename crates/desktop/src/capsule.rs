//! Smart capsule, identical to the legacy capsule.html (NibrasShell SmartCapsule): an accent-gradient
//! island at the top centre. Idle = clock; prioritised requests (volume, brightness, track, battery,
//! alerts, focus, wallpaper/theme) replace it for a few seconds; expands into a 410×220 card with
//! weather / media / focus tabs. Painting is pure: the runtime owns timers, input and window geometry.
use crate::render::{Align, Canvas, Color, Radii, Rect};
use crate::theme::{self, oklch_shift, Tokens, R};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Weather,
    Media,
    Focus,
}
impl Tab {
    pub const ALL: [Tab; 3] = [Tab::Weather, Tab::Media, Tab::Focus];
    fn icon(self) -> &'static str {
        match self {
            Tab::Weather => "cloud",
            Tab::Media => "music",
            Tab::Focus => "timer",
        }
    }
}

/// Colour scheme of a request (CapsuleManager tones).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tone {
    Primary,
    Secondary,
    Music,
    Ok,
    Warn,
    Crit,
}

/// IDLE 0 < HOVER 1 < TRANSIENT 2 < NOTIFICATION 3 < WARNING 4 < CRITICAL 5
#[derive(Clone, Debug)]
pub struct Request {
    pub priority: u8,
    pub timeout_ms: u32,
    pub icon: &'static str,
    pub text: String,
    pub progress: Option<f32>,
    pub tone: Tone,
}

#[derive(Clone, Default)]
pub struct Weather {
    pub status: String,
    pub celsius: f32,
    pub fahrenheit: f32,
    pub wind_kmh: f32,
    pub is_day: bool,
}
#[derive(Clone, Default)]
pub struct Media {
    pub player: String,
    pub title: String,
    pub artist: String,
    pub playing: bool,
    pub progress: f32,
    pub sessions: usize,
    pub index: usize,
    pub can_prev: bool,
    pub can_next: bool,
}
#[derive(Clone, Default)]
pub struct Focus {
    pub remaining_ms: u64,
    pub total_ms: u64,
    pub running: bool,
    pub on_break: bool,
    pub task: String,
}

#[derive(Clone, Default)]
pub struct State {
    pub time: String, // "12:55 AM"
    pub date: String, // "Thursday, October 15"
    pub request: Option<Request>,
    pub playing: bool,
    pub weather: Option<Weather>,
    pub media: Option<Media>,
    pub focus: Option<Focus>,
    pub expanded: bool,
    pub tab: Option<Tab>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Open(Tab),
    OpenDefault,
    Close,
    SwitchTab(Tab),
    MediaPrev,
    MediaToggle,
    MediaNext,
    NextSession,
    OpenObsidian,
}

pub const IDLE_H: f32 = 24.;
pub const CARD: (f32, f32) = (410., 220.);
pub const CARD_TOP: f32 = 40.;

/// Accent-derived capsule colours (`--cap-bg1/2`, `--cap-fg`).
pub struct Palette {
    pub bg1: Color,
    pub bg2: Color,
    pub fg: Color,
}
pub fn palette(t: &Tokens) -> Palette {
    if t.glass {
        // accent-tinted glass with light text instead of a solid accent pill
        return Palette {
            bg1: t.accent.alpha(0.42),
            bg2: oklch_shift(t.accent, -0.06, 48.).alpha(0.30),
            fg: t.text,
        };
    }
    Palette {
        bg1: t.accent,
        bg2: oklch_shift(t.accent, -0.06, 48.),
        fg: t.bg,
    }
}

fn weather_icon(status: &str) -> &'static str {
    if status.starts_with("clear") {
        if status.ends_with("night") {
            "moon"
        } else {
            "sun"
        }
    } else if status.contains("rain") {
        "rain"
    } else if status.starts_with("snow") {
        "snow"
    } else if status.starts_with("thunder") {
        "bolt"
    } else {
        "cloud"
    }
}
pub fn weather_label(status: &str) -> &'static str {
    match status {
        "clear_day" => "Clear",
        "clear_night" => "Clear night",
        "cloudy_day" => "Cloudy",
        "cloudy_night" => "Cloudy night",
        "light_rain_day" | "light_rain_night" => "Light rain",
        "heavy_rain_day" | "heavy_rain_night" => "Heavy rain",
        "snow_day" | "snow_night" => "Snow",
        "thunder_day" | "thunder_night" => "Thunder",
        _ => "—",
    }
}
fn mmss(ms: u64) -> String {
    let s = ms.div_ceil(1000);
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// Width of the idle island: clock ≥ 312; a request: text + 100 (text column clamped 220–500).
pub fn idle_width(c: &Canvas, s: &State) -> f32 {
    match &s.request {
        Some(r) => {
            c.measure(&r.text, &theme::body(12., 700), 0.)
                .clamp(190., 470.)
                + 30.
                + 100.
        }
        None => {
            let w = c.measure(&s.time, &theme::label(14., 700), 0.)
                + c.measure(&format!(" – {}", s.date), &theme::body(12.5, 700), 0.);
            (w.ceil()
                + 100.
                + if s.focus.as_ref().is_some_and(|f| f.running) {
                    40.
                } else {
                    0.
                })
            .max(312.)
        }
    }
}

fn tone_fill(c: &Canvas, r: Rect, k: Radii, p: &Palette, t: &Tokens, tone: Tone) {
    let dark = |x: Color| x.mix(Color::rgb(0, 0, 0), 0.78);
    match tone {
        Tone::Primary => c.fill_round_gradient(r, k, &[(0., p.bg1), (1., p.bg2)]),
        Tone::Secondary => c.fill_round_gradient(r, k, &[(0., p.bg2), (1., p.bg1)]),
        Tone::Music => c.fill_round_gradient(
            r,
            k,
            &[
                (0., p.bg1),
                (0.5, p.bg2),
                (1., oklch_shift(t.accent, 0., 140.)),
            ],
        ),
        Tone::Ok => c.fill_round_gradient(r, k, &[(0., t.success), (1., dark(t.success))]),
        Tone::Warn => c.fill_round_gradient(r, k, &[(0., p.bg2), (1., dark(p.bg2))]),
        Tone::Crit => c.fill_round_gradient(r, k, &[(0., t.warning), (1., dark(t.warning))]),
    }
}

/// Idle / request island in a window `win_w` wide (island centred, 4 px from the top).
pub fn paint_idle(
    c: &Canvas,
    win_w: f32,
    t: &Tokens,
    s: &State,
    island_w: f32,
) -> Vec<(Rect, Hit)> {
    let p = palette(t);
    let r = Rect::new((win_w - island_w) / 2., 4., island_w, IDLE_H);
    let k = Radii::all(R);
    // shadow: 0 4px 16px rgba(0,0,0,.45) approximated with stacked translucent rounds
    let shadow: &[(f32, f32)] = if t.glass {
        &[]
    } else {
        &[(6., 0.08), (3., 0.12), (1., 0.18)]
    };
    for &(g, a) in shadow {
        c.fill_round(
            Rect::new(r.x - g, r.y - g + 3., r.w + 2. * g, r.h + 2. * g),
            Radii::all(R + g),
            Color::rgba(0, 0, 0, a),
        );
    }
    let tone = match &s.request {
        Some(q) => q.tone,
        None if s.playing => Tone::Music,
        None => Tone::Primary,
    };
    tone_fill(c, r, k, &p, t, tone);
    let fg = if matches!(tone, Tone::Ok | Tone::Crit) {
        Color::rgb(255, 255, 255)
    } else {
        p.fg
    };
    if let Some(prog) = s.request.as_ref().and_then(|q| q.progress) {
        c.push_clip(r);
        c.fill_round(
            Rect::new(r.x, r.y, r.w * prog.clamp(0., 1.), r.h),
            Radii {
                tl: R,
                bl: R,
                tr: 0.,
                br: 0.,
            },
            fg.alpha(0.22),
        );
        c.pop_clip();
    }
    let mut hits = vec![(r, Hit::OpenDefault)];
    // left slot: weather icon (or the request's icon)
    let icon = match (&s.request, &s.weather) {
        (Some(q), _) => q.icon,
        (None, Some(w)) => weather_icon(&w.status),
        _ => "cloud",
    };
    let lr = Rect::new(r.x + 12., r.y + 4., 16., 16.);
    c.icon(icon, lr, fg, 2.);
    hits.insert(0, (Rect::new(r.x, r.y, 34., r.h), Hit::Open(Tab::Weather)));
    // centre: clock or request text
    let inner = Rect::new(r.x + 34., r.y, r.w - 68., r.h);
    match &s.request {
        Some(q) => c.text(&q.text, inner, &theme::body(12., 700), fg, Align::Center),
        None => {
            let (tf, df) = (theme::label(14., 700), theme::body(12.5, 700));
            let rest = format!(" – {}", s.date);
            let (tw, dw) = (c.measure(&s.time, &tf, 0.), c.measure(&rest, &df, 0.));
            let x0 = inner.x + (inner.w - tw - dw) / 2.;
            c.text(
                &s.time,
                Rect::new(x0, r.y, tw + 1., r.h),
                &tf,
                fg,
                Align::Start,
            );
            c.text(
                &rest,
                Rect::new(x0 + tw, r.y, dw + 1., r.h),
                &df,
                fg,
                Align::Start,
            );
        }
    }
    // right slot: focus ring + mm:ss while a session runs, else two quiet dots
    let running = s.focus.as_ref().filter(|f| f.running);
    if let Some(f) = running {
        let v = 1. - f.remaining_ms as f32 / f.total_ms.max(1) as f32;
        let label = mmss(f.remaining_ms);
        let lw = c.measure(&label, &theme::label(12., 700), 0.);
        let x = r.right() - 12. - lw - 20.;
        c.ring(x + 8., r.y + 12., 16., 2.2, v, fg.alpha(0.3), fg);
        c.text(
            &label,
            Rect::new(x + 20., r.y, lw + 2., r.h),
            &theme::label(12., 700),
            fg,
            Align::Start,
        );
        hits.insert(
            0,
            (
                Rect::new(x - 4., r.y, r.right() - x + 4., r.h),
                Hit::Open(Tab::Focus),
            ),
        );
    } else {
        for i in 0..2 {
            c.fill_circle(
                r.right() - 12. - 2.5 - i as f32 * 9.,
                r.y + 12.,
                2.5,
                fg.alpha(0.8),
            );
        }
        hits.insert(
            0,
            (
                Rect::new(r.right() - 34., r.y, 34., r.h),
                Hit::Open(Tab::Media),
            ),
        );
    }
    hits
}

/// Expanded card (410×220), `top` = 40 in the final state; `alpha` fades the content in.
pub fn paint_card(
    c: &Canvas,
    win_w: f32,
    t: &Tokens,
    s: &State,
    frame: Rect,
    alpha: f32,
) -> Vec<(Rect, Hit)> {
    let p = palette(t);
    let fg = p.fg;
    let fg10 = fg.alpha(0.10);
    let fg20 = fg.alpha(0.20);
    let fg70 = fg.alpha(0.70);
    let _ = win_w;
    // glass: no shadow (it would be frosted into a blurry halo around the card)
    let shadow: &[(f32, f32)] = if t.glass {
        &[]
    } else {
        &[(8., 0.06), (4., 0.10), (1., 0.18)]
    };
    for &(g, a) in shadow {
        c.fill_round(
            Rect::new(
                frame.x - g,
                frame.y - g + 4.,
                frame.w + 2. * g,
                frame.h + 2. * g,
            ),
            Radii::all(R + g),
            Color::rgba(0, 0, 0, a),
        );
    }
    tone_fill(
        c,
        frame,
        Radii::all(R),
        &p,
        t,
        if s.playing {
            Tone::Music
        } else {
            Tone::Primary
        },
    );
    let mut hits = Vec::new();
    if alpha <= 0.01 {
        return hits;
    }
    let a = |col: Color| col.alpha(alpha);
    let tab = s.tab.unwrap_or(Tab::Weather);
    // close (34×30, 15 px inset)
    let x = Rect::new(frame.right() - 15. - 34., frame.y + 15., 34., 30.);
    c.fill_round(x, Radii::all(10.), a(fg10));
    c.icon("x", Rect::new(x.x + 9., x.y + 7., 16., 16.), a(fg), 2.);
    hits.push((x, Hit::Close));
    // switcher: 183×32 track, sliding 61 px indicator
    let sw = Rect::new(frame.x + (frame.w - 183.) / 2., frame.y + 15., 183., 32.);
    c.fill_round(sw, Radii::all(R), a(fg10));
    let i = Tab::ALL.iter().position(|x| *x == tab).unwrap_or(0) as f32;
    c.fill_round(
        Rect::new(sw.x + i * 61., sw.y, 61., 32.),
        Radii::all(R),
        a(fg20),
    );
    for (j, tb) in Tab::ALL.iter().enumerate() {
        let br = Rect::new(sw.x + j as f32 * 61., sw.y, 61., 32.);
        c.icon(
            tb.icon(),
            Rect::new(br.x + 22.5, br.y + 8., 16., 16.),
            a(fg.alpha(if *tb == tab { 1. } else { 0.5 })),
            2.,
        );
        hits.push((br, Hit::SwitchTab(*tb)));
    }
    let pane = Rect::new(
        frame.x + 18.,
        frame.y + 60.,
        frame.w - 36.,
        frame.h - 60. - 16.,
    );
    match tab {
        Tab::Media => match &s.media {
            None => {
                c.icon(
                    "music",
                    Rect::new(pane.x + pane.w / 2. - 10., pane.y + 30., 20., 20.),
                    a(fg70),
                    2.,
                );
                c.text(
                    "Nothing is playing",
                    Rect::new(pane.x, pane.y + 56., pane.w, 20.),
                    &theme::body(12., 400),
                    a(fg70),
                    Align::Center,
                );
            }
            Some(m) => {
                let art = Rect::new(pane.x, pane.y, 110., 110.);
                c.fill_round(art, Radii::all(R), a(fg10));
                c.icon(
                    "music",
                    Rect::new(art.x + 35., art.y + 35., 40., 40.),
                    a(fg),
                    1.6,
                );
                let col = Rect::new(art.right() + 15., pane.y, pane.w - 125., 110.);
                c.text_tracked(
                    &m.player.to_uppercase(),
                    Rect::new(col.x, col.y, col.w, 12.),
                    &theme::label(9., 700),
                    a(fg70),
                    Align::Start,
                    1.8,
                );
                c.text(
                    &m.title,
                    Rect::new(col.x, col.y + 14., col.w, 22.),
                    &theme::body(14., 700),
                    a(fg),
                    Align::Start,
                );
                c.text(
                    &m.artist,
                    Rect::new(col.x, col.y + 36., col.w, 18.),
                    &theme::body(12., 400),
                    a(fg70),
                    Align::Start,
                );
                let prog = Rect::new(col.x, col.y + 62., col.w, 4.);
                c.fill_round(prog, Radii::all(2.), a(fg10));
                c.fill_round(
                    Rect::new(prog.x, prog.y, prog.w * m.progress.clamp(0., 1.), 4.),
                    Radii::all(2.),
                    a(fg),
                );
                let cy = col.y + 74.;
                let prev = Rect::new(col.x, cy, 30., 30.);
                let main = Rect::new(col.x + 36., cy - 2., 50., 34.);
                let next = Rect::new(col.x + 92., cy, 30., 30.);
                c.fill_round(main, Radii::all(R), a(fg10));
                c.icon(
                    "prev",
                    Rect::new(prev.x + 6.5, prev.y + 6.5, 17., 17.),
                    a(fg.alpha(if m.can_prev { 1. } else { 0.35 })),
                    2.,
                );
                c.icon(
                    if m.playing { "pause" } else { "play" },
                    Rect::new(main.x + 16.5, main.y + 8.5, 17., 17.),
                    a(fg),
                    2.,
                );
                c.icon(
                    "next",
                    Rect::new(next.x + 6.5, next.y + 6.5, 17., 17.),
                    a(fg.alpha(if m.can_next { 1. } else { 0.35 })),
                    2.,
                );
                hits.extend([
                    (prev, Hit::MediaPrev),
                    (main, Hit::MediaToggle),
                    (next, Hit::MediaNext),
                ]);
                if m.sessions > 1 {
                    let label = format!("{}/{}", m.index + 1, m.sessions);
                    let sr = Rect::new(col.right() - 34., cy, 34., 30.);
                    c.text_tracked(&label, sr, &theme::label(9., 700), a(fg70), Align::End, 1.2);
                    hits.push((sr, Hit::NextSession));
                }
            }
        },
        Tab::Weather => match &s.weather {
            None => c.text(
                "Loading weather…",
                pane,
                &theme::body(12., 400),
                a(fg70),
                Align::Center,
            ),
            Some(w) => {
                let l = Rect::new(pane.x + 2., pane.y + 24., 220., 100.);
                c.icon(
                    weather_icon(&w.status),
                    Rect::new(l.x, l.y + 6., 44., 44.),
                    a(fg),
                    1.5,
                );
                c.text(
                    &format!("{}°", w.celsius.round()),
                    Rect::new(l.x + 54., l.y, 140., 56.),
                    &theme::label(42., 700),
                    a(fg),
                    Align::Start,
                );
                c.text(
                    weather_label(&w.status),
                    Rect::new(l.x, l.y + 60., 200., 20.),
                    &theme::body(12., 700),
                    a(fg.alpha(0.9)),
                    Align::Start,
                );
                c.fill_rect(
                    Rect::new(l.right() + 8., pane.y + 2., 1., 128.),
                    a(fg.alpha(0.15)),
                );
                let sx = l.right() + 24.;
                for (j, (k, v)) in [
                    ("WIND", format!("{} km/h", w.wind_kmh.round())),
                    (
                        if w.is_day { "DAY" } else { "NIGHT" },
                        format!("{}°F", w.fahrenheit.round()),
                    ),
                ]
                .iter()
                .enumerate()
                {
                    let y = pane.y + 26. + j as f32 * 47.;
                    c.text_tracked(
                        k,
                        Rect::new(sx, y, 120., 12.),
                        &theme::label(9., 700),
                        a(fg70),
                        Align::Start,
                        1.6,
                    );
                    c.text(
                        v,
                        Rect::new(sx, y + 13., 140., 22.),
                        &theme::label(16., 700),
                        a(fg),
                        Align::Start,
                    );
                }
            }
        },
        Tab::Focus => match &s.focus {
            None => {
                c.text(
                    "No focus session.",
                    Rect::new(pane.x, pane.y + 39., pane.w, 20.),
                    &theme::body(12., 400),
                    a(fg70),
                    Align::Center,
                );
                let b = Rect::new(pane.x + (pane.w - 132.) / 2., pane.y + 70., 132., 30.);
                c.fill_round(b, Radii::all(R), a(fg10));
                c.icon("crown", Rect::new(b.x + 14., b.y + 8., 14., 14.), a(fg), 2.);
                c.text_tracked(
                    "OPEN OBSIDIAN",
                    Rect::new(b.x + 34., b.y, b.w - 40., b.h),
                    &theme::label(10., 700),
                    a(fg),
                    Align::Start,
                    1.6,
                );
                hits.push((b, Hit::OpenObsidian));
            }
            Some(f) => {
                let v = 1. - f.remaining_ms as f32 / f.total_ms.max(1) as f32;
                let (cx, cy) = (pane.x + 55., pane.y + 55.);
                c.ring(cx, cy, 110., 6., v, fg.alpha(0.25 * alpha), a(fg));
                c.text(
                    &mmss(f.remaining_ms),
                    Rect::new(cx - 50., cy - 20., 100., 30.),
                    &theme::label(28., 700),
                    a(fg),
                    Align::Center,
                );
                c.text_tracked(
                    &format!(
                        "{}{}",
                        if f.on_break { "BREAK" } else { "FOCUS" },
                        if f.running { "" } else { " · PAUSED" }
                    ),
                    Rect::new(cx - 50., cy + 10., 100., 14.),
                    &theme::label(9., 700),
                    a(fg.alpha(0.75)),
                    Align::Center,
                    1.8,
                );
                let tx = pane.x + 128.;
                c.text_tracked(
                    &format!("{} MIN SESSION", f.total_ms / 60_000),
                    Rect::new(tx, pane.y + 14., pane.w - 128., 14.),
                    &theme::label(9., 700),
                    a(fg.alpha(0.75)),
                    Align::Start,
                    1.8,
                );
                c.text(
                    &f.task,
                    Rect::new(tx, pane.y + 32., pane.w - 128., 20.),
                    &theme::body(12., 600),
                    a(fg),
                    Align::Start,
                );
                let b = Rect::new(tx, pane.y + 62., 160., 30.);
                c.fill_round(b, Radii::all(R), a(fg10));
                c.icon("crown", Rect::new(b.x + 14., b.y + 8., 14., 14.), a(fg), 2.);
                c.text_tracked(
                    "OPEN OBSIDIAN",
                    Rect::new(b.x + 34., b.y, b.w - 40., b.h),
                    &theme::label(10., 700),
                    a(fg),
                    Align::Start,
                    1.6,
                );
                hits.push((b, Hit::OpenObsidian));
            }
        },
    }
    hits
}

/// Representative data for snapshots (matches the legacy capture of 2026-10-15).
pub fn sample() -> State {
    State {
        time: "12:55 AM".into(),
        date: "Thursday, October 15".into(),
        weather: Some(Weather {
            status: "clear_night".into(),
            celsius: 30.,
            fahrenheit: 87.,
            wind_kmh: 4.,
            is_day: false,
        }),
        ..Default::default()
    }
}
