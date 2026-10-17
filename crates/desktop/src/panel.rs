//! Side panel, identical to the legacy panel.html (NibrasShell floating menu): 370 px wide, radius 16,
//! 2 px accent border, 64 px app bar, tinted cards (primary / secondary / tertiary containers), ten tabs.
//! Pure painting from `State`; the runtime owns input, text editing, scrolling and data.
use crate::leftbar::Tab;
use crate::render::{Align, Canvas, Color, Radii, Rect};
use crate::theme::{self, oklch_shift, Tokens};
use crate::topbar::Image;

pub const WIDTH: f32 = 374.;
pub const X: f32 = 50.;
pub const Y: f32 = 43.;

#[derive(Clone, Default)]
pub struct Wallpaper {
    pub id: String,
    pub thumb: Option<Image>,
    pub on: bool,
}
#[derive(Clone, Default)]
pub struct Alert {
    pub id: u64,
    pub title: String,
    pub message: String,
    pub ago: String,
    pub perf: bool,
}
#[derive(Clone, Default)]
pub struct Proc {
    pub name: String,
    pub count: u32,
    pub cpu: f32,
    pub mem_mb: u32,
}
#[derive(Clone, Default)]
pub struct Disk {
    pub name: String,
    pub label: String,
    pub free_gb: f32,
    pub used_pct: f32,
}
#[derive(Clone, Default)]
pub struct WifiNet {
    pub ssid: String,
    pub signal: u8,
    pub secured: bool,
    pub known: bool,
    pub connected: bool,
}
#[derive(Clone, Default)]
pub struct Task {
    pub id: u64,
    pub text: String,
    pub done: bool,
    pub urgent: bool,
}

#[derive(Clone, Default)]
pub struct State {
    pub tab: Option<Tab>,
    pub scroll: f32,
    pub hover: Option<Hit>,
    pub focused_input: Option<Input>,
    pub caret_visible: bool,
    // dashboard
    pub rank: char,
    pub ranks: Vec<(char, Color)>,
    pub greeting: String,
    pub date_ar: String,
    pub banner: Option<Image>,
    pub wifi_on: Option<bool>,
    pub bt_on: Option<bool>,
    pub muted: bool,
    pub volume: Option<u8>,
    pub brightness: Option<u8>,
    pub presets: Vec<String>,
    pub preset: String,
    pub wallpapers: Vec<Wallpaper>,
    pub rotate_hours: f32,
    pub focus: Option<crate::capsule::Focus>,
    // notifications
    pub alerts: Vec<Alert>,
    // weather
    pub weather: Option<crate::capsule::Weather>,
    pub updated: String,
    // monitoring
    pub cpu: Option<f32>,
    pub ram: Option<f32>,
    pub battery: Option<f32>,
    pub threads: u32,
    pub mem_text: String,
    pub procs: Vec<Proc>,
    pub disks: Vec<Disk>,
    // network
    pub ssid: Option<String>,
    pub ip: String,
    pub down_bps: Option<f64>,
    pub up_bps: Option<f64>,
    pub today_bytes: f64,
    pub month_bytes: f64,
    pub networks: Option<Vec<WifiNet>>,
    pub open_net: Option<String>,
    // clipboard
    pub clipboard: Vec<String>,
    pub clip_query: String,
    pub clip_sel: usize,
    // todo
    pub tasks: Vec<Task>,
    pub todo_input: String,
    pub todo_urgent: bool,
    // power
    pub armed: Option<&'static str>,
    // settings
    pub dynamic_accent: bool,
    pub reduce_motion: bool,
    pub leftbar_autohide: bool,
    pub focus_pulse: String,
    pub rotate_enabled: bool,
    pub version: String,
    // ai
    pub ai_status: String,
    pub ai_key_input: String,
    pub ai_prompt: String,
    pub ai_response: String,
    pub ai_error: String,
    pub ai_busy: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Input {
    TodoText,
    ClipSearch,
    AiKey,
    AiPrompt,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Hit {
    Close,
    Radio(&'static str),
    Mute,
    Open(&'static str),
    Slider(&'static str, Rect),
    Preset(String),
    Rank(char),
    Wallpaper(String),
    NextWallpaper,
    AlertsClear,
    AlertDismiss(u64),
    WindowsNotifications,
    Diagnose,
    WifiToggle,
    Scan,
    Net(String),
    Connect(String),
    Disconnect,
    Forget(String),
    WifiSettings,
    ClipCopy(usize),
    ClipDelete(usize),
    ClipClear,
    TodoAdd,
    TodoUrgentToggle,
    TodoDone(u64),
    TodoUrgent(u64),
    TodoDelete(u64),
    Power(&'static str),
    Setting(&'static str),
    Reload,
    WindowsSettings,
    AiSaveKey,
    AiTest,
    AiSend,
    AiCapture,
    AiClear,
    AiTheme,
    AiDiagnose,
    Focus(Input),
}

pub fn title(tab: Tab) -> (&'static str, &'static str) {
    match tab {
        Tab::Dashboard => ("SYSTEM · DASHBOARD", "لوحة التحكم"),
        Tab::Notifications => ("SYSTEM · ALERTS", "الإشعارات"),
        Tab::Weather => ("ATMOSPHERE · NOW", "الطقس"),
        Tab::Monitoring => ("SYSTEM · MONITOR", "مراقبة النظام"),
        Tab::Network => ("SYSTEM · NETWORK", "الشبكة"),
        Tab::Clipboard => ("SYSTEM · CLIPBOARD", "الحافظة"),
        Tab::Ai => ("SYSTEM · GEMINI", "مساعد Gemini"),
        Tab::Settings => ("SYSTEM · PREFERENCES", "إعدادات النظام"),
        Tab::Todo => ("QUEST · LOG", "المهام"),
        Tab::Power => ("SYSTEM · POWER", "الطاقة"),
    }
}

fn fmt_bytes(b: f64) -> String {
    let u = ['B', 'K', 'M', 'G'];
    let (mut b, mut i) = (b, 0);
    while b >= 1024. && i < 3 {
        b /= 1024.;
        i += 1;
    }
    if b < 10. && i > 0 {
        format!("{b:.1}{}", u[i])
    } else {
        format!("{}{}", b.round() as i64, u[i])
    }
}

/// Stacking layout helper inside the scrolled content area.
struct Ui<'a> {
    c: &'a Canvas,
    t: &'a Tokens,
    x: f32,
    y: f32,
    w: f32,
    hits: Vec<(Rect, Hit)>,
    card_index: usize,
    s: &'a State,
}

impl<'a> Ui<'a> {
    fn kicker(&self, s: &str, r: Rect, color: Color) {
        self.c.text_tracked(
            s,
            r,
            &theme::label(10.5, 600),
            color,
            Align::Start,
            10.5 * 0.22,
        )
    }
    fn hit(&mut self, r: Rect, h: Hit) {
        self.hits.push((r, h));
    }
    fn hovered(&self, h: &Hit) -> bool {
        self.s.hover.as_ref() == Some(h)
    }
    fn container(&self, i: usize) -> (Color, Color) {
        let t = self.t;
        let surf = t.surface_solid;
        let secondary = oklch_shift(t.accent, -0.04, 48.);
        let tertiary = oklch_shift(t.accent, 0., 140.);
        let clear = Color::rgba(0, 0, 0, 0.);
        match i % 3 {
            0 => (t.accent.mix(surf, 0.11), t.line_soft),
            1 => (secondary.mix(surf, 0.11), secondary.mix(clear, 0.18)),
            _ => (tertiary.mix(surf, 0.09), tertiary.mix(clear, 0.14)),
        }
    }
    /// A card: measure pass (draw = false) gives the height, then a draw pass.
    fn card(
        &mut self,
        kicker: Option<&str>,
        header: bool,
        body: impl Fn(&mut Ui, f32, bool) -> f32,
    ) {
        let pad = 14.;
        let mut inner_y = 0.;
        if kicker.is_some() {
            inner_y += 18. + 9. + 11.;
        }
        let saved = self.hits.len();
        let h_body = body(self, self.y + pad + inner_y, false);
        self.hits.truncate(saved);
        let h = pad + inner_y + h_body + pad;
        let r = Rect::new(self.x, self.y, self.w, h);
        let (bg, border) = if header {
            (
                self.t.text.alpha(0.04).mix(self.t.surface_solid, 1.),
                self.t.text.alpha(0.10),
            )
        } else {
            self.container(self.card_index)
        };
        if header {
            self.c.chip(r, Radii::all(12.), self.t.surface_solid, None);
        }
        self.c.chip(r, Radii::all(12.), bg, Some(border));
        if !header {
            self.card_index += 1;
        }
        if let Some(k) = kicker {
            self.kicker(
                k,
                Rect::new(r.x + pad, r.y + pad, r.w - 2. * pad, 18.),
                self.t.accent,
            );
            self.c.fill_rect(
                Rect::new(r.x + pad, r.y + pad + 18. + 9., r.w - 2. * pad, 1.),
                self.t.text.alpha(0.12),
            );
        }
        body(self, self.y + pad + inner_y, true);
        self.y += h + 15.;
    }
    fn btn(&mut self, r: Rect, icon: Option<&str>, label: &str, hit: Hit, draw: bool) {
        if draw {
            let hov = self.hovered(&hit);
            self.c.chip(
                r,
                Radii::all(r.h / 2.),
                if hov {
                    self.t.accent_soft
                } else {
                    self.t.surface
                },
                Some(self.t.line),
            );
            let mut x = r.x + 14.;
            if let Some(i) = icon {
                self.c.icon(
                    i,
                    Rect::new(x, r.y + (r.h - 14.) / 2., 14., 14.),
                    self.t.accent,
                    1.8,
                );
                x += 20.;
            }
            self.c.text_tracked(
                label,
                Rect::new(x, r.y, r.right() - x - 10., r.h),
                &theme::label(11., 600),
                self.t.accent,
                Align::Start,
                11. * 0.14,
            );
        }
        self.hit(r, hit);
    }
    fn btn_w(&self, icon: bool, label: &str) -> f32 {
        28. + if icon { 20. } else { 0. }
            + self.c.measure(label, &theme::label(11., 600), 11. * 0.14)
    }
    /// Segmented small button (sg).
    #[allow(clippy::too_many_arguments)]
    fn seg(
        &mut self,
        r: Rect,
        k: Radii,
        icon: Option<&str>,
        label: &str,
        on: bool,
        hit: Hit,
        draw: bool,
        danger: bool,
    ) {
        if draw {
            let t = self.t;
            let bg = if danger {
                t.warning
            } else if on {
                t.accent
            } else if self.hovered(&hit) {
                t.accent.alpha(0.22)
            } else {
                t.accent.alpha(0.12)
            };
            let fg = if on {
                t.bg
            } else if danger {
                Color::rgb(255, 255, 255)
            } else {
                t.text
            };
            self.c.fill_round(r, k, bg);
            let lw = if label.is_empty() {
                0.
            } else {
                self.c.measure(label, &theme::body(11., 600), 0.)
            };
            let total = lw
                + if icon.is_some() {
                    13. + if lw > 0. { 5. } else { 0. }
                } else {
                    0.
                };
            let mut x = r.x + (r.w - total) / 2.;
            if let Some(i) = icon {
                self.c
                    .icon(i, Rect::new(x, r.y + (r.h - 13.) / 2., 13., 13.), fg, 1.8);
                x += 18.;
            }
            if lw > 0. {
                self.c.text(
                    label,
                    Rect::new(x, r.y, lw + 2., r.h),
                    &theme::body(11., 600),
                    fg,
                    Align::Start,
                );
            }
        }
        self.hit(r, hit);
    }
    fn input(
        &mut self,
        r: Rect,
        value: &str,
        placeholder: &str,
        which: Input,
        password: bool,
        draw: bool,
    ) {
        if draw {
            let t = self.t;
            let focused = self.s.focused_input == Some(which);
            self.c.chip(
                r,
                Radii::all(9.),
                t.surface_solid,
                Some(if focused { t.accent } else { t.line_soft }),
            );
            let shown: String = if password {
                "•".repeat(value.chars().count())
            } else {
                value.to_string()
            };
            let tr = Rect::new(r.x + 10., r.y, r.w - 20., r.h);
            if shown.is_empty() {
                self.c.text(
                    placeholder,
                    tr,
                    &theme::body(12., 400),
                    t.muted,
                    Align::Start,
                );
            } else {
                self.c
                    .text(&shown, tr, &theme::body(12., 400), t.text, Align::Start);
            }
            if focused && self.s.caret_visible {
                let cw = self.c.measure(&shown, &theme::body(12., 400), 0.).min(tr.w);
                // Arabic text is right-aligned, so its insertion point is at the left end of the run
                let x = if crate::render::is_rtl(&shown) {
                    tr.right() - cw - 2.5
                } else {
                    tr.x + cw + 1.
                };
                self.c
                    .fill_rect(Rect::new(x, r.y + 7., 1.5, r.h - 14.), t.accent);
            }
        }
        self.hit(r, Hit::Focus(which));
    }
    fn toggle_row(
        &mut self,
        y: f32,
        label: &str,
        detail: &str,
        on: bool,
        key: &'static str,
        draw: bool,
    ) -> f32 {
        let h = 10.
            + 18.
            + 4.
            + self
                .c
                .wrapped_height(detail, self.w - 28. - 50., &theme::body(10., 400), 17.)
            + 10.;
        if draw {
            let t = self.t;
            let x = self.x + 14.;
            self.c.text(
                label,
                Rect::new(x, y + 10., self.w - 28. - 50., 18.),
                &theme::body(12., 500),
                t.text,
                Align::End,
            );
            self.c.text_wrapped(
                detail,
                x,
                y + 32.,
                self.w - 28. - 50.,
                &theme::body(10., 400),
                t.muted,
                17.,
            );
            let sw = Rect::new(self.x + self.w - 14. - 32., y + (h - 18.) / 2., 32., 18.);
            self.c
                .fill_round(sw, Radii::all(9.), if on { t.accent } else { t.faint });
            self.c.fill_circle(
                if on { sw.right() - 9. } else { sw.x + 9. },
                sw.y + 9.,
                6.5,
                if on { t.bg } else { t.muted },
            );
            self.c
                .fill_rect(Rect::new(x, y + h - 1., self.w - 28., 1.), t.line_soft);
        }
        self.hit(Rect::new(self.x, y, self.w, h), Hit::Setting(key));
        h
    }
}

/// Paint the whole panel (window is `WIDTH` × `height`). Returns hit regions in window coordinates.
pub fn paint(
    c: &Canvas,
    height: f32,
    t: &Tokens,
    s: &State,
    slide: f32,
) -> (Vec<(Rect, Hit)>, f32) {
    let tab = s.tab.unwrap_or(Tab::Dashboard);
    let frame = Rect::new(2. - (1. - slide) * WIDTH, 0., WIDTH - 4., height - 2.);
    // shadow + accent glow, then the 2 px accent border
    for (g, a) in [(8., 0.05), (4., 0.10)] {
        c.fill_round(
            Rect::new(
                frame.x - g,
                frame.y - g + 4.,
                frame.w + 2. * g,
                frame.h + 2. * g,
            ),
            Radii::all(16. + g),
            Color::rgba(0, 0, 0, a),
        );
    }
    c.fill_round(
        Rect::new(frame.x - 2., frame.y - 2., frame.w + 4., frame.h + 4.),
        Radii::all(18.),
        t.accent.alpha(0.18),
    );
    c.fill_round(frame, Radii::all(16.), t.accent);
    let inner = frame.inset(2.);
    c.fill_round(inner, Radii::all(14.), Color::rgb(7, 11, 20));
    let mut hits = Vec::new();
    // app bar
    let bar = Rect::new(inner.x, inner.y, inner.w, 64.);
    c.push_clip(inner);
    c.fill_rect(bar, t.text.alpha(0.04));
    c.fill_rect(Rect::new(bar.x, bar.bottom() - 1., bar.w, 1.), t.line_soft);
    c.fill_circle(bar.x + 8. + 24., bar.y + 32., 20., t.accent_soft);
    c.icon(
        tab.icon(),
        Rect::new(bar.x + 8. + 13., bar.y + 21., 22., 22.),
        t.accent,
        1.8,
    );
    let (kick, ttl) = title(tab);
    c.text_tracked(
        kick,
        Rect::new(bar.x + 60., bar.y + 13., bar.w - 120., 14.),
        &theme::label(9.5, 600),
        t.accent,
        Align::Start,
        9.5 * 0.22,
    );
    c.text(
        ttl,
        Rect::new(bar.x + 60., bar.y + 27., bar.w - 120., 26.),
        &theme::head(18., 500),
        t.text,
        Align::End,
    );
    let x = Rect::new(bar.right() - 4. - 40., bar.y + 12., 40., 40.);
    if s.hover == Some(Hit::Close) {
        c.fill_circle(x.x + 20., x.y + 20., 20., t.text.alpha(0.08));
    }
    c.icon("x", Rect::new(x.x + 12., x.y + 12., 16., 16.), t.muted, 1.8);
    hits.push((x, Hit::Close));

    // scrolled content
    let view = Rect::new(
        inner.x,
        bar.bottom(),
        inner.w,
        inner.bottom() - bar.bottom(),
    );
    c.push_clip(view);
    c.translate(0., -s.scroll);
    let mut ui = Ui {
        c,
        t,
        x: inner.x + 15.,
        y: view.y + 15.,
        w: inner.w - 30.,
        hits: Vec::new(),
        card_index: 0,
        s,
    };
    match tab {
        Tab::Dashboard => dashboard(&mut ui),
        Tab::Notifications => notifications(&mut ui),
        Tab::Weather => weather(&mut ui),
        Tab::Monitoring => monitoring(&mut ui),
        Tab::Network => network(&mut ui),
        Tab::Clipboard => clipboard(&mut ui),
        Tab::Todo => todo(&mut ui),
        Tab::Power => power(&mut ui),
        Tab::Settings => settings(&mut ui),
        Tab::Ai => ai(&mut ui),
    }
    let content_h = ui.y - view.y;
    let scroll = s.scroll;
    for (r, h) in ui.hits {
        let r = Rect::new(r.x, r.y - scroll, r.w, r.h);
        if r.bottom() > view.y && r.y < view.bottom() {
            hits.push((r, h));
        }
    }
    c.reset_translation();
    c.pop_clip();
    c.pop_clip();
    (hits, (content_h - view.h).max(0.))
}

fn dashboard(ui: &mut Ui) {
    let s = ui.s;
    let t = *ui.t;
    // banner: wallpaper + overlapping hunter card
    let w = ui.w;
    let (x, y) = (ui.x, ui.y);
    let r = Rect::new(x, y, w, 150. + 46. + 34. + 30.);
    ui.c.chip(r, Radii::all(12.), t.surface_solid, Some(t.line_soft));
    let img = Rect::new(x + 1., y + 1., w - 2., 150.);
    match &s.banner {
        Some(i) => {
            ui.c.push_clip(img);
            ui.c.image(&i.rgba, i.w, i.h, cover(img, i.w, i.h), 1.);
            ui.c.pop_clip();
        }
        None => ui.c.fill_round_gradient(
            img,
            Radii {
                tl: 11.,
                tr: 11.,
                br: 0.,
                bl: 0.,
            },
            &[(0., t.accent), (1., oklch_shift(t.accent, -0.04, 48.))],
        ),
    }
    let av = Rect::new(x + 12., y + 150. - 42., 72., 72.);
    ui.c.fill_round(
        Rect::new(av.x - 4., av.y - 4., 80., 80.),
        Radii::all(24.),
        t.accent.alpha(0.16),
    );
    ui.c.fill_round(av, Radii::all(20.), t.accent);
    ui.c.fill_round(av.inset(2.), Radii::all(18.), t.surface_solid);
    ui.c.text(
        &s.rank.to_string(),
        av,
        &theme::head(34., 600),
        t.accent,
        Align::Center,
    );
    ui.kicker(
        &format!("HUNTER · RANK {}", s.rank),
        Rect::new(av.right() + 12., y + 150. + 4., w - 100., 16.),
        t.accent,
    );
    ui.c.text(
        &s.greeting,
        Rect::new(av.right() + 12., y + 150. + 20., w - 100., 26.),
        &theme::head(20., 600),
        t.text,
        Align::Start,
    );
    ui.c.text(
        &s.date_ar,
        Rect::new(x + 14., y + 150. + 46. + 8., w - 28., 20.),
        &theme::body(12., 400),
        t.muted,
        Align::Start,
    );
    ui.y += r.h + 15.;
    ui.card_index += 1;

    // quick actions (3×2 tiles)
    ui.card(Some("QUICK · ACTIONS"), false, |ui, y0, draw| {
        let tiles: [(&str, &str, &str, bool, Hit); 6] = [
            (
                "WI-FI",
                if s.wifi_on == Some(true) {
                    "wifi"
                } else {
                    "wifiOff"
                },
                if s.wifi_on == Some(true) { "On" } else { "Off" },
                s.wifi_on == Some(true),
                Hit::Radio("wifi"),
            ),
            (
                "BLUETOOTH",
                "bt",
                if s.bt_on == Some(true) { "On" } else { "Off" },
                s.bt_on == Some(true),
                Hit::Radio("bluetooth"),
            ),
            (
                "AUDIO",
                if s.muted { "mute" } else { "vol" },
                if s.muted { "Muted" } else { "Sound" },
                s.muted,
                Hit::Mute,
            ),
            ("VAULT", "crown", "Obsidian", false, Hit::Open("obsidian")),
            ("TASKS", "activity", "Manager", false, Hit::Open("taskmgr")),
            (
                "WINDOWS",
                "settings",
                "Settings",
                false,
                Hit::Open("settings"),
            ),
        ];
        let tw = (ui.w - 28. - 12.) / 3.;
        for (i, (k, ic, label, on, hit)) in tiles.into_iter().enumerate() {
            let r = Rect::new(
                ui.x + 14. + (i % 3) as f32 * (tw + 6.),
                y0 + (i / 3) as f32 * 70.,
                tw,
                64.,
            );
            if draw {
                let hov = ui.hovered(&hit);
                ui.c.chip(
                    r,
                    Radii::all(12.),
                    if on {
                        t.accent_soft
                    } else if hov {
                        t.surface_hover
                    } else {
                        t.surface
                    },
                    Some(if on { t.line } else { t.line_soft }),
                );
                ui.c.text_tracked(
                    k,
                    Rect::new(r.x + 12., r.y + 14., r.w - 16., 14.),
                    &theme::label(9.5, 600),
                    t.muted,
                    Align::Start,
                    9.5 * 0.22,
                );
                let fg = if on { t.accent } else { t.text };
                ui.c.icon(ic, Rect::new(r.x + 12., r.y + 36., 14., 14.), fg, 1.8);
                ui.c.text(
                    label,
                    Rect::new(r.x + 32., r.y + 33., r.w - 36., 20.),
                    &theme::body(12., 500),
                    fg,
                    Align::Start,
                );
            }
            ui.hit(r, hit);
        }
        134.
    });

    // levels
    ui.card(Some("SYSTEM · LEVELS"), false, |ui, y0, draw| {
        let mut yy = y0;
        for (ic, key, v) in [
            (if s.muted { "mute" } else { "vol" }, "volume", s.volume),
            ("sun", "brightness", s.brightness),
        ] {
            let Some(v) = v else { continue };
            let track = Rect::new(ui.x + 14. + 32., yy + 9., ui.w - 28. - 32. - 48., 6.);
            if draw {
                ui.c.icon(ic, Rect::new(ui.x + 14., yy + 4., 15., 15.), t.muted, 1.8);
                ui.c.fill_round(track, Radii::all(3.), t.faint);
                ui.c.fill_round(
                    Rect::new(track.x, track.y, track.w * v as f32 / 100., 6.),
                    Radii::all(3.),
                    t.accent,
                );
                let tx = track.x + track.w * v as f32 / 100.;
                ui.c.fill_circle(tx, track.y + 3., 10., t.accent.alpha(0.45));
                ui.c.fill_circle(tx, track.y + 3., 7., t.text);
                ui.c.text(
                    &v.to_string(),
                    Rect::new(track.right() + 8., yy, 38., 24.),
                    &theme::label(13., 600),
                    t.text,
                    Align::End,
                );
            }
            ui.hit(
                Rect::new(track.x - 8., yy, track.w + 16., 24.),
                Hit::Slider(key, track),
            );
            yy += 34.;
        }
        (yy - y0 - 10.).max(0.)
    });

    if let Some(f) = &s.focus {
        ui.card(None, false, |ui, y0, draw| {
            if draw {
                let brk = f.on_break;
                let col = if brk { t.success } else { t.accent };
                ui.c.ring(
                    ui.x + 14. + 23.,
                    y0 + 23.,
                    46.,
                    4.,
                    1. - f.remaining_ms as f32 / f.total_ms.max(1) as f32,
                    t.faint,
                    col,
                );
                ui.kicker(
                    &format!(
                        "SYSTEM · {}{}",
                        if brk { "BREAK" } else { "FOCUS" },
                        if f.running { "" } else { " · PAUSED" }
                    ),
                    Rect::new(ui.x + 74., y0 + 2., 200., 16.),
                    col,
                );
                let s2 = f.remaining_ms.div_ceil(1000);
                ui.c.text(
                    &format!("{:02}:{:02}", s2 / 60, s2 % 60),
                    Rect::new(ui.x + 74., y0 + 18., 120., 28.),
                    &theme::label(22., 600),
                    t.text,
                    Align::Start,
                );
            }
            let bw = ui.btn_w(false, "OPEN");
            ui.btn(
                Rect::new(ui.x + ui.w - 14. - bw, y0 + 6., bw, 34.),
                None,
                "OPEN",
                Hit::Open("obsidian"),
                draw,
            );
            46.
        });
    }

    // presets
    ui.card(Some("NIBRAS · THEMES"), false, |ui, y0, draw| {
        let mut names = vec!["ShadowMonarch".to_string()];
        names.extend(s.presets.iter().cloned());
        let cw = (ui.w - 28. - 12.) / 3.;
        for (i, n) in names.iter().enumerate() {
            let r = Rect::new(
                ui.x + 14. + (i % 3) as f32 * (cw + 6.),
                y0 + (i / 3) as f32 * (31. + 6.),
                cw,
                31.,
            );
            let on = *n == s.preset || (s.preset.is_empty() && i == 0);
            let label = if n == "ShadowMonarch" {
                "Shadow Monarch".to_string()
            } else {
                n.replace("Dark", "")
            };
            if draw {
                let hov = ui.hovered(&Hit::Preset(n.clone()));
                ui.c.chip(
                    r,
                    Radii::all(8.),
                    if on {
                        t.accent
                    } else if hov {
                        t.accent_soft
                    } else {
                        t.surface2
                    },
                    Some(t.line_soft),
                );
                ui.c.text(
                    &label,
                    r,
                    &theme::body(11., 600),
                    if on { t.bg } else { t.text },
                    Align::Center,
                );
            }
            ui.hit(r, Hit::Preset(n.clone()));
        }
        let rows = names.len().div_ceil(3) as f32;
        rows * 31. + (rows - 1.) * 6.
    });

    // ranks
    ui.card(Some("HUNTER · RANK"), false, |ui, y0, draw| {
        let cw = (ui.w - 28. - 30.) / 6.;
        for (i, (k, col)) in s.ranks.iter().enumerate() {
            let r = Rect::new(ui.x + 14. + i as f32 * (cw + 6.), y0, cw, 40.);
            let on = *k == s.rank;
            if draw {
                if on {
                    ui.c.fill_round(
                        Rect::new(r.x - 3., r.y - 3., r.w + 6., r.h + 6.),
                        Radii::all(13.),
                        col.alpha(0.20),
                    );
                    ui.c.chip(
                        r,
                        Radii::all(10.),
                        col.alpha(0.18).mix(t.surface, 1.),
                        Some(*col),
                    );
                } else {
                    ui.c.chip(r, Radii::all(10.), t.surface, None);
                }
                ui.c.text(
                    &k.to_string(),
                    r,
                    &theme::head(18., 500),
                    *col,
                    Align::Center,
                );
            }
            ui.hit(r, Hit::Rank(*k));
        }
        40.
    });

    // wallpapers
    ui.card(None, false, |ui, y0, draw| {
        let bw = ui.btn_w(true, "NEXT");
        if draw {
            ui.kicker(
                &format!(
                    "NIBRAS + RANK {} · WALLPAPERS · EVERY {}H",
                    s.rank, s.rotate_hours
                ),
                Rect::new(ui.x + 14., y0, ui.w - 28. - bw - 8., 34.),
                t.accent,
            );
        }
        ui.btn(
            Rect::new(ui.x + ui.w - 14. - bw, y0, bw, 34.),
            Some("restart"),
            "NEXT",
            Hit::NextWallpaper,
            draw,
        );
        let cw = (ui.w - 28. - 12.) / 3.;
        let ch = cw * 10. / 16.;
        for (i, wp) in s.wallpapers.iter().enumerate() {
            let r = Rect::new(
                ui.x + 14. + (i % 3) as f32 * (cw + 6.),
                y0 + 44. + (i / 3) as f32 * (ch + 6.),
                cw,
                ch,
            );
            if draw {
                ui.c.fill_round(r, Radii::all(9.), t.surface2);
                if let Some(im) = &wp.thumb {
                    ui.c.push_clip(r.inset(1.5));
                    ui.c.image(&im.rgba, im.w, im.h, cover(r, im.w, im.h), 1.);
                    ui.c.pop_clip();
                }
                if wp.on {
                    ui.c.fill_round(
                        Rect::new(r.x - 2., r.y - 2., r.w + 4., r.h + 4.),
                        Radii::all(11.),
                        t.accent.alpha(0.2),
                    );
                    ui.c.stroke_round(r, Radii::all(9.), t.accent, 1.5);
                }
            }
            ui.hit(r, Hit::Wallpaper(wp.id.clone()));
        }
        let rows = s.wallpapers.len().div_ceil(3) as f32;
        44. + rows * ch + (rows - 1.).max(0.) * 6.
    });
}

/// Rect that covers `r` keeping the image aspect (CSS object-fit: cover; caller clips).
fn cover(r: Rect, w: u32, h: u32) -> Rect {
    let s = (r.w / w as f32).max(r.h / h as f32);
    let (dw, dh) = (w as f32 * s, h as f32 * s);
    Rect::new(r.x + (r.w - dw) / 2., r.y + (r.h - dh) / 2., dw, dh)
}

fn header_row(ui: &mut Ui, y0: f32, title: &str, small: &str, draw: bool) {
    if draw {
        let tw = ui.c.measure(title, &theme::body(16., 700), 0.);
        ui.c.text(
            title,
            Rect::new(ui.x + 14., y0, tw + 2., 26.),
            &theme::body(16., 700),
            ui.t.text,
            Align::Start,
        );
        ui.c.text(
            small,
            Rect::new(ui.x + 14. + tw + 4., y0 + 2., 200., 24.),
            &theme::body(10.5, 400),
            ui.t.muted,
            Align::Start,
        );
    }
}
fn count_pill(ui: &Ui, r: Rect, n: usize) {
    let t = ui.t;
    ui.c.fill_round(r, Radii::all(11.), t.accent);
    ui.c.text(
        &n.to_string(),
        r,
        &theme::label(12., 700),
        t.bg,
        Align::Center,
    );
}

fn notifications(ui: &mut Ui) {
    let s = ui.s;
    let t = *ui.t;
    ui.card(None, true, |ui, y0, draw| {
        if draw {
            count_pill(ui, Rect::new(ui.x + 14., y0 + 2., 26., 22.), s.alerts.len());
            ui.c.text(
                "Alerts",
                Rect::new(ui.x + 48., y0, 60., 26.),
                &theme::body(13., 700),
                t.text,
                Align::Start,
            );
            ui.c.text(
                if s.alerts.is_empty() {
                    " · all quiet"
                } else {
                    " · system"
                },
                Rect::new(ui.x + 94., y0 + 1., 100., 26.),
                &theme::body(10.5, 400),
                t.muted,
                Align::Start,
            );
        }
        let right = ui.x + ui.w - 14.;
        ui.seg(
            Rect::new(right - 35., y0, 35., 26.),
            Radii {
                tl: 3.,
                bl: 3.,
                tr: 9.,
                br: 9.,
            },
            Some("bell"),
            "",
            false,
            Hit::WindowsNotifications,
            draw,
            false,
        );
        ui.seg(
            Rect::new(right - 37. - 72., y0, 72., 26.),
            Radii {
                tl: 9.,
                bl: 9.,
                tr: 3.,
                br: 3.,
            },
            Some("trash"),
            "Clear",
            false,
            Hit::AlertsClear,
            draw,
            false,
        );
        26.
    });
    if s.alerts.is_empty() {
        ui.card(None, false, |ui, y0, draw| {
            if draw {
                ui.c.icon(
                    "bell",
                    Rect::new(ui.x + ui.w / 2. - 9., y0 + 4., 18., 18.),
                    t.muted,
                    1.8,
                );
                ui.c.text(
                    "النظام هادئ. تنبيهات الاستهلاك المرتفع والبطارية هتظهر هنا.",
                    Rect::new(ui.x + 14., y0 + 28., ui.w - 28., 22.),
                    &theme::body(11.5, 400),
                    t.muted,
                    Align::Center,
                );
            }
            52.
        });
    }
    for a in &s.alerts {
        ui.card(None, false, |ui, y0, draw| {
            let msg_h =
                ui.c.wrapped_height(&a.message, ui.w - 28. - 40., &theme::body(11.5, 400), 18.);
            if draw {
                ui.c.fill_round(
                    Rect::new(ui.x + 14., y0, 30., 30.),
                    Radii::all(10.),
                    t.accent_soft,
                );
                ui.c.icon(
                    if a.perf { "activity" } else { "bell" },
                    Rect::new(ui.x + 21., y0 + 7., 16., 16.),
                    t.accent,
                    1.8,
                );
                ui.c.text(
                    &a.title,
                    Rect::new(ui.x + 54., y0, ui.w - 54. - 70., 30.),
                    &theme::body(12.5, 700),
                    t.text,
                    Align::Start,
                );
                ui.c.text(
                    &a.ago,
                    Rect::new(ui.x + ui.w - 14. - 24. - 36., y0, 30., 30.),
                    &theme::body(10., 400),
                    t.muted,
                    Align::End,
                );
                ui.c.text_wrapped(
                    &a.message,
                    ui.x + 54.,
                    y0 + 36.,
                    ui.w - 28. - 40.,
                    &theme::body(11.5, 400),
                    t.muted,
                    18.,
                );
            }
            let xr = Rect::new(ui.x + ui.w - 14. - 24., y0 + 3., 24., 24.);
            if draw {
                ui.c.icon(
                    "x",
                    Rect::new(xr.x + 5.5, xr.y + 5.5, 13., 13.),
                    t.muted,
                    1.8,
                );
            }
            ui.hit(xr, Hit::AlertDismiss(a.id));
            36. + msg_h
        });
    }
}

fn boxes(ui: &Ui, y: f32, items: &[(Option<&str>, String, &str)]) {
    let t = ui.t;
    let n = items.len() as f32;
    let bw = (ui.w - 28. - (n - 1.) * 6.) / n;
    for (i, (ic, v, small)) in items.iter().enumerate() {
        let r = Rect::new(ui.x + 14. + i as f32 * (bw + 6.), y, bw, 54.);
        ui.c.fill_round(r, Radii::all(10.), t.text.alpha(0.05));
        let mut yy = r.y + 8.;
        if let Some(ic) = ic {
            ui.c.icon(
                ic,
                Rect::new(r.x + bw / 2. - 7.5, yy, 15., 15.),
                t.accent,
                1.8,
            );
            yy += 18.;
        } else {
            yy += 9.;
        }
        ui.c.text(
            v,
            Rect::new(r.x + 2., yy - 2., bw - 4., 18.),
            &theme::label(13., 700),
            t.text,
            Align::Center,
        );
        ui.c.text(
            small,
            Rect::new(r.x + 2., yy + 13., bw - 4., 14.),
            &theme::body(9.5, 400),
            t.muted,
            Align::Center,
        );
    }
}

fn weather(ui: &mut Ui) {
    let s = ui.s;
    let t = *ui.t;
    let Some(w) = &s.weather else {
        ui.card(None, false, |ui, y0, draw| {
            if draw {
                ui.c.text(
                    "Loading weather…",
                    Rect::new(ui.x, y0, ui.w, 30.),
                    &theme::body(12., 400),
                    t.muted,
                    Align::Center,
                );
            }
            30.
        });
        return;
    };
    ui.card(None, true, |ui, y0, draw| {
        if draw {
            header_row(
                ui,
                y0,
                "Weather",
                &format!(
                    " · {} · updated {}",
                    if w.is_day { "day" } else { "night" },
                    s.updated
                ),
                true,
            );
            let st = w.status.as_str();
            let ic = if st.starts_with("clear") {
                if st.ends_with("night") {
                    "moon"
                } else {
                    "sun"
                }
            } else if st.contains("rain") {
                "rain"
            } else if st.starts_with("snow") {
                "snow"
            } else if st.starts_with("thunder") {
                "bolt"
            } else {
                "cloud"
            };
            ui.c.icon(ic, Rect::new(ui.x + 14., y0 + 40., 72., 72.), t.accent, 1.3);
            ui.c.text(
                &format!("{}°", w.celsius.round()),
                Rect::new(ui.x + 104., y0 + 38., 160., 56.),
                &theme::label(52., 700),
                t.text,
                Align::Start,
            );
            ui.c.text(
                crate::capsule::weather_label(st),
                Rect::new(ui.x + 104., y0 + 94., 200., 20.),
                &theme::body(12., 400),
                t.muted,
                Align::Start,
            );
            let night = !w.is_day;
            boxes(
                ui,
                y0 + 128.,
                &[
                    (Some("wind"), format!("{}", w.wind_kmh.round()), "km/h"),
                    (Some("sun"), format!("{}°", w.fahrenheit.round()), "°F"),
                    (
                        Some(if night { "moon" } else { "sun" }),
                        if night { "Night".into() } else { "Day".into() },
                        "now",
                    ),
                    (
                        Some("cloud"),
                        st.trim_end_matches("_day")
                            .trim_end_matches("_night")
                            .replace('_', " "),
                        "sky",
                    ),
                ],
            );
        }
        182.
    });
}

fn monitoring(ui: &mut Ui) {
    let s = ui.s;
    let t = *ui.t;
    ui.card(None, false, |ui, y0, draw| {
        if draw {
            let cw = (ui.w - 28.) / 3.;
            for (i, (v, label, col)) in [
                (s.cpu, "CPU", t.accent),
                (s.ram, "RAM", t.success),
                (
                    s.battery,
                    "BATTERY",
                    if s.battery.unwrap_or(100.) < 20. {
                        t.warning
                    } else {
                        t.accent
                    },
                ),
            ]
            .iter()
            .enumerate()
            {
                let cx = ui.x + 14. + i as f32 * cw + cw / 2.;
                ui.c.ring(cx, y0 + 48., 84., 6., v.unwrap_or(0.) / 100., t.faint, *col);
                ui.c.text(
                    &v.map(|v| format!("{}%", v.round())).unwrap_or("—".into()),
                    Rect::new(cx - 40., y0 + 34., 80., 28.),
                    &theme::label(20., 700),
                    t.text,
                    Align::Center,
                );
                ui.c.text_tracked(
                    label,
                    Rect::new(cx - 50., y0 + 98., 100., 16.),
                    &theme::label(10.5, 600),
                    t.accent,
                    Align::Center,
                    2.3,
                );
            }
            ui.c.text(
                &format!("{} threads · {}", s.threads, s.mem_text),
                Rect::new(ui.x + 14., y0 + 118., ui.w - 28., 18.),
                &theme::body(11.5, 400),
                t.muted,
                Align::Center,
            );
        }
        136.
    });
    ui.card(None, false, |ui, y0, draw| {
        if draw {
            ui.kicker(
                "TOP · PROCESSES",
                Rect::new(ui.x + 14., y0, 200., 34.),
                t.accent,
            );
        }
        let bw = ui.btn_w(false, "REFRESH");
        ui.btn(
            Rect::new(ui.x + ui.w - 14. - bw, y0, bw, 34.),
            None,
            "REFRESH",
            Hit::Diagnose,
            draw,
        );
        let mut y = y0 + 44.;
        if s.procs.is_empty() {
            if draw {
                ui.c.text(
                    "Sampling…",
                    Rect::new(ui.x + 18., y, 200., 28.),
                    &theme::body(11.5, 400),
                    t.muted,
                    Align::Start,
                );
            }
            y += 28.;
        }
        for p in &s.procs {
            if draw {
                let name = if p.count > 1 {
                    format!("{}  ×{}", p.name, p.count)
                } else {
                    p.name.clone()
                };
                ui.c.text(
                    &name,
                    Rect::new(ui.x + 18., y, ui.w - 160., 27.),
                    &theme::body(11.5, 500),
                    t.text,
                    Align::Start,
                );
                ui.c.text(
                    &format!("{}%", (p.cpu * 10.).round() / 10.),
                    Rect::new(ui.x + ui.w - 150., y, 70., 27.),
                    &theme::label(12.5, 600),
                    t.text,
                    Align::End,
                );
                ui.c.text(
                    &format!("{} MB", p.mem_mb),
                    Rect::new(ui.x + ui.w - 80., y, 62., 27.),
                    &theme::label(12.5, 600),
                    t.muted,
                    Align::End,
                );
                ui.c.fill_rect(Rect::new(ui.x + 14., y + 27., ui.w - 28., 1.), t.line_soft);
            }
            y += 28.;
        }
        y - y0
    });
    ui.card(Some("STORAGE"), false, |ui, y0, draw| {
        let mut y = y0;
        for d in &s.disks {
            if draw {
                ui.c.text(
                    &d.name,
                    Rect::new(ui.x + 14., y, 30., 20.),
                    &theme::label(12.5, 700),
                    t.text,
                    Align::Start,
                );
                ui.c.text(
                    &d.label,
                    Rect::new(ui.x + 46., y, ui.w - 160., 20.),
                    &theme::body(11.5, 400),
                    t.muted,
                    Align::Start,
                );
                ui.c.text(
                    &if d.free_gb >= 100. {
                        format!("{:.0} GB free", d.free_gb)
                    } else {
                        format!("{:.1} GB free", d.free_gb)
                    },
                    Rect::new(ui.x + ui.w - 140., y, 126., 20.),
                    &theme::label(12.5, 600),
                    t.text,
                    Align::End,
                );
                let m = Rect::new(ui.x + 14., y + 24., ui.w - 28., 5.);
                ui.c.fill_round(m, Radii::all(3.), t.faint);
                ui.c.fill_round(
                    Rect::new(m.x, m.y, m.w * d.used_pct / 100., 5.),
                    Radii::all(3.),
                    if d.used_pct > 90. {
                        t.warning
                    } else {
                        t.accent
                    },
                );
            }
            y += 39.;
        }
        (y - y0 - 10.).max(0.)
    });
}

fn network(ui: &mut Ui) {
    let s = ui.s;
    let t = *ui.t;
    ui.card(None, true, |ui, y0, draw| {
        if draw {
            ui.c.fill_round(
                Rect::new(ui.x + 14., y0, 40., 40.),
                Radii::all(12.),
                t.accent_soft,
            );
            ui.c.icon(
                if s.wifi_on == Some(false) {
                    "wifiOff"
                } else {
                    "wifi"
                },
                Rect::new(ui.x + 24., y0 + 10., 20., 20.),
                t.accent,
                1.8,
            );
            ui.c.text(
                s.ssid.as_deref().unwrap_or("Disconnected"),
                Rect::new(ui.x + 64., y0, ui.w - 170., 22.),
                &theme::body(16., 700),
                t.text,
                Align::Start,
            );
            ui.c.text(
                &s.ip,
                Rect::new(ui.x + 64., y0 + 22., ui.w - 170., 18.),
                &theme::body(10.5, 400),
                t.muted,
                Align::Start,
            );
            boxes(
                ui,
                y0 + 52.,
                &[
                    (
                        Some("down"),
                        s.down_bps.map(fmt_bytes).unwrap_or("—".into()),
                        "/s",
                    ),
                    (
                        Some("up"),
                        s.up_bps.map(fmt_bytes).unwrap_or("—".into()),
                        "/s",
                    ),
                    (None, fmt_bytes(s.today_bytes), "today"),
                    (None, fmt_bytes(s.month_bytes), "month"),
                ],
            );
        }
        let on = s.wifi_on == Some(true);
        ui.seg(
            Rect::new(ui.x + ui.w - 14. - 80., y0 + 7., 80., 26.),
            Radii::all(9.),
            None,
            if on { "Wi-Fi On" } else { "Wi-Fi Off" },
            on,
            Hit::WifiToggle,
            draw,
            false,
        );
        106.
    });
    ui.card(None, false, |ui, y0, draw| {
        if draw {
            ui.kicker(
                "AVAILABLE · NETWORKS",
                Rect::new(ui.x + 14., y0, 200., 26.),
                t.accent,
            );
        }
        ui.seg(
            Rect::new(ui.x + ui.w - 14. - 70., y0, 70., 26.),
            Radii::all(9.),
            Some("restart"),
            "Scan",
            false,
            Hit::Scan,
            draw,
            false,
        );
        let mut y = y0 + 34.;
        match &s.networks {
            None => {
                if draw {
                    ui.c.text(
                        "Scanning…",
                        Rect::new(ui.x, y, ui.w, 30.),
                        &theme::body(12., 400),
                        t.muted,
                        Align::Center,
                    );
                }
                y += 30.;
            }
            Some(list) if list.is_empty() => {
                if draw {
                    ui.c.text(
                        "No networks found.",
                        Rect::new(ui.x, y, ui.w, 30.),
                        &theme::body(12., 400),
                        t.muted,
                        Align::Center,
                    );
                }
                y += 30.;
            }
            Some(list) => {
                for n in list {
                    let ex = s.open_net.as_deref() == Some(n.ssid.as_str());
                    let rh = if ex { 58. + 38. } else { 58. };
                    let r = Rect::new(ui.x + 14., y, ui.w - 28., rh);
                    if draw {
                        let bg = if ex {
                            t.accent
                        } else if n.connected {
                            t.accent.alpha(0.22)
                        } else if ui.hovered(&Hit::Net(n.ssid.clone())) {
                            oklch_shift(t.accent, -0.04, 48.).alpha(0.15)
                        } else {
                            Color::rgba(0, 0, 0, 0.)
                        };
                        ui.c.fill_round(r, Radii::all(12.), bg);
                        let fg = if ex { t.bg } else { t.text };
                        let sub = if ex { t.bg.alpha(0.75) } else { t.muted };
                        ui.c.icon("wifi", Rect::new(r.x + 12., r.y + 19., 20., 20.), fg, 1.8);
                        ui.c.text(
                            &n.ssid,
                            Rect::new(r.x + 44., r.y + 10., r.w - 80., 22.),
                            &theme::body(14., 700),
                            fg,
                            Align::Start,
                        );
                        let st = if n.connected {
                            "Connected"
                        } else if n.known {
                            "Saved"
                        } else if n.secured {
                            "Secured"
                        } else {
                            "Open"
                        };
                        ui.c.text(
                            &format!("{st} · {}%", n.signal),
                            Rect::new(r.x + 44., r.y + 31., r.w - 80., 18.),
                            &theme::body(11., 400),
                            sub,
                            Align::Start,
                        );
                        if n.secured {
                            ui.c.icon(
                                "lock",
                                Rect::new(r.right() - 30., r.y + 21., 16., 16.),
                                fg,
                                1.8,
                            );
                        }
                    }
                    ui.hit(Rect::new(r.x, r.y, r.w, 58.), Hit::Net(n.ssid.clone()));
                    if ex {
                        let by = r.y + 58.;
                        let mut bx = r.x + 44.;
                        let actions: Vec<(&str, Hit, bool)> = if n.connected {
                            vec![("Disconnect", Hit::Disconnect, false)]
                        } else if n.known {
                            vec![("Connect", Hit::Connect(n.ssid.clone()), true)]
                        } else {
                            vec![("Connect in Settings", Hit::WifiSettings, true)]
                        };
                        let mut actions = actions;
                        if n.known {
                            actions.push(("Forget", Hit::Forget(n.ssid.clone()), false));
                        }
                        for (label, hit, primary) in actions {
                            let bw = ui.c.measure(label, &theme::body(11., 600), 0.) + 20.;
                            let br = Rect::new(bx, by, bw, 26.);
                            if draw {
                                ui.c.fill_round(
                                    br,
                                    Radii::all(9.),
                                    if primary { t.bg } else { t.bg.alpha(0.18) },
                                );
                                ui.c.text(
                                    label,
                                    br,
                                    &theme::body(11., 600),
                                    if primary { t.accent } else { t.bg },
                                    Align::Center,
                                );
                            }
                            ui.hit(br, hit);
                            bx += bw + 6.;
                        }
                    }
                    y += rh + 6.;
                }
            }
        }
        y - y0
    });
}

fn clipboard(ui: &mut Ui) {
    let s = ui.s;
    let t = *ui.t;
    ui.card(None, true, |ui, y0, draw| {
        if draw {
            ui.c.text(
                "Clipboard History",
                Rect::new(ui.x + 14., y0, 200., 26.),
                &theme::body(16., 700),
                t.text,
                Align::Start,
            );
            ui.c.text(
                &s.clipboard.len().to_string(),
                Rect::new(ui.x + ui.w - 14. - 72. - 30., y0, 24., 26.),
                &theme::body(10.5, 400),
                t.muted,
                Align::End,
            );
        }
        if !s.clipboard.is_empty() {
            ui.seg(
                Rect::new(ui.x + ui.w - 14. - 72., y0, 72., 26.),
                Radii::all(9.),
                Some("trash"),
                "Clear",
                false,
                Hit::ClipClear,
                draw,
                false,
            );
        }
        ui.input(
            Rect::new(ui.x + 14., y0 + 36., ui.w - 28., 30.),
            &s.clip_query,
            "Search…  (↑ ↓ Enter)",
            Input::ClipSearch,
            false,
            draw,
        );
        66.
    });
    let q = s.clip_query.to_lowercase();
    let rows: Vec<(usize, &String)> = s
        .clipboard
        .iter()
        .enumerate()
        .filter(|(_, x)| q.is_empty() || x.to_lowercase().contains(&q))
        .collect();
    ui.card(None, false, |ui, y0, draw| {
        if rows.is_empty() {
            if draw {
                ui.c.text(
                    if q.is_empty() {
                        "Copy something — it appears here."
                    } else {
                        "No match."
                    },
                    Rect::new(ui.x, y0, ui.w, 30.),
                    &theme::body(12., 400),
                    t.muted,
                    Align::Center,
                );
            }
            return 30.;
        }
        let mut y = y0 - 8.;
        for (k, (i, text)) in rows.iter().enumerate() {
            let r = Rect::new(ui.x + 6., y, ui.w - 12., 46.);
            if draw {
                let sel = k == s.clip_sel;
                if sel || ui.hovered(&Hit::ClipCopy(*i)) {
                    ui.c.fill_round(r, Radii::all(10.), t.accent.alpha(0.15));
                }
                ui.c.fill_round(
                    Rect::new(r.x + 4., r.y + 12., 3., 22.),
                    Radii::all(2.),
                    t.accent.alpha(if sel { 1. } else { 0.55 }),
                );
                let one: String = text
                    .chars()
                    .take(300)
                    .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
                    .collect();
                ui.c.text(
                    &one,
                    Rect::new(r.x + 16., r.y, r.w - 50., r.h),
                    &theme::body(11.5, 400),
                    t.text,
                    Align::Start,
                );
                ui.c.icon(
                    "x",
                    Rect::new(r.right() - 24., r.y + 16.5, 13., 13.),
                    t.muted,
                    1.8,
                );
                if k > 0 {
                    ui.c.fill_rect(Rect::new(r.x, r.y, r.w, 1.), t.text.alpha(0.05));
                }
            }
            ui.hit(
                Rect::new(r.right() - 30., r.y + 11., 24., 24.),
                Hit::ClipDelete(*i),
            );
            ui.hit(Rect::new(r.x, r.y, r.w - 32., r.h), Hit::ClipCopy(*i));
            y += 46.;
        }
        y - y0 - 8.
    });
}

fn todo(ui: &mut Ui) {
    let s = ui.s;
    let t = *ui.t;
    let left = s.tasks.iter().filter(|x| !x.done).count();
    ui.card(None, true, |ui, y0, draw| {
        if draw {
            ui.c.text(
                "Quests",
                Rect::new(ui.x + 14., y0, 200., 26.),
                &theme::body(16., 700),
                t.text,
                Align::Start,
            );
            count_pill(
                ui,
                Rect::new(ui.x + ui.w - 14. - 26., y0 + 2., 26., 22.),
                left,
            );
        }
        let iy = y0 + 36.;
        ui.input(
            Rect::new(ui.x + 14., iy, ui.w - 28. - 34. - 38. - 12., 38.),
            &s.todo_input,
            "مهمة جديدة…",
            Input::TodoText,
            false,
            draw,
        );
        let ur = Rect::new(ui.x + ui.w - 14. - 38. - 6. - 34., iy, 34., 38.);
        if draw {
            ui.c.fill_round(
                ur,
                Radii::all(9.),
                if s.todo_urgent {
                    t.warning
                } else {
                    t.accent.alpha(0.12)
                },
            );
            ui.c.text(
                "!",
                ur,
                &theme::label(15., 700),
                if s.todo_urgent {
                    Color::rgb(255, 255, 255)
                } else {
                    t.text
                },
                Align::Center,
            );
        }
        ui.hit(ur, Hit::TodoUrgentToggle);
        let ar = Rect::new(ui.x + ui.w - 14. - 38., iy, 38., 38.);
        if draw {
            ui.c.fill_round(ar, Radii::all(10.), t.accent);
            ui.c.icon(
                "plus",
                Rect::new(ar.x + 11., ar.y + 11., 16., 16.),
                t.bg,
                2.,
            );
        }
        ui.hit(ar, Hit::TodoAdd);
        74.
    });
    let mut sorted: Vec<&Task> = s.tasks.iter().collect();
    sorted.sort_by_key(|x| (x.done, !x.urgent, x.id));
    ui.card(None, false, |ui, y0, draw| {
        if sorted.is_empty() {
            if draw {
                ui.c.text(
                    "No quests yet.",
                    Rect::new(ui.x, y0, ui.w, 30.),
                    &theme::body(12., 400),
                    t.muted,
                    Align::Center,
                );
            }
            return 30.;
        }
        let mut y = y0 - 8.;
        for task in &sorted {
            let r = Rect::new(ui.x + 6., y, ui.w - 12., 40.);
            if draw {
                if task.urgent && !task.done {
                    ui.c.fill_round(r, Radii::all(8.), t.warning.alpha(0.09));
                }
                let ck = Rect::new(r.x + 8., r.y + 10., 20., 20.);
                if task.done {
                    ui.c.fill_round(ck, Radii::all(6.), t.success);
                    ui.c.icon(
                        "check",
                        Rect::new(ck.x + 3., ck.y + 3., 14., 14.),
                        t.bg,
                        2.2,
                    );
                } else {
                    ui.c.stroke_round(ck, Radii::all(6.), t.line, 1.5);
                }
                ui.c.text(
                    &task.text,
                    Rect::new(r.x + 38., r.y, r.w - 38. - 64., r.h),
                    &theme::body(12., 400),
                    if task.done { t.muted } else { t.text },
                    Align::Start,
                );
                if task.done {
                    let tw =
                        ui.c.measure(&task.text, &theme::body(12., 400), 0.)
                            .min(r.w - 102.);
                    ui.c.fill_rect(Rect::new(r.x + 38., r.y + 20., tw, 1.), t.muted);
                }
                ui.c.text(
                    "!",
                    Rect::new(r.right() - 58., r.y + 8., 24., 24.),
                    &theme::label(13., 700),
                    if task.urgent && !task.done {
                        t.warning
                    } else {
                        t.muted.alpha(0.5)
                    },
                    Align::Center,
                );
                ui.c.icon(
                    "trash",
                    Rect::new(r.right() - 28., r.y + 13.5, 13., 13.),
                    t.muted,
                    1.8,
                );
            }
            ui.hit(
                Rect::new(r.x + 4., r.y + 6., 28., 28.),
                Hit::TodoDone(task.id),
            );
            ui.hit(
                Rect::new(r.right() - 58., r.y + 8., 24., 24.),
                Hit::TodoUrgent(task.id),
            );
            ui.hit(
                Rect::new(r.right() - 34., r.y + 8., 24., 24.),
                Hit::TodoDelete(task.id),
            );
            y += 40.;
        }
        y - y0 - 8.
    });
}

fn power(ui: &mut Ui) {
    let s = ui.s;
    let t = *ui.t;
    let items: [(&str, &str, &str, bool); 6] = [
        ("lock", "lock", "LOCK", false),
        ("sleep", "moon", "SLEEP", false),
        ("hibernate", "hibernate", "HIBERNATE", false),
        ("logout", "logout", "SIGN OUT", true),
        ("restart", "restart", "RESTART", true),
        ("shutdown", "power", "SHUT DOWN", true),
    ];
    let bw = (ui.w - 8.) / 2.;
    for (i, (act, ic, label, danger)) in items.iter().enumerate() {
        let r = Rect::new(
            ui.x + (i % 2) as f32 * (bw + 8.),
            ui.y + (i / 2) as f32 * 100.,
            bw,
            92.,
        );
        let armed = s.armed == Some(*act);
        let col = if *danger { t.warning } else { t.text };
        if armed {
            ui.c.fill_round(
                Rect::new(r.x - 3., r.y - 3., r.w + 6., r.h + 6.),
                Radii::all(17.),
                t.warning.alpha(0.2),
            );
            ui.c.chip(
                r,
                Radii::all(14.),
                t.warning.alpha(0.22).mix(t.surface2, 1.),
                Some(t.warning),
            );
        } else {
            let hov = ui.hovered(&Hit::Power(act));
            ui.c.chip(
                r,
                Radii::all(14.),
                if hov { t.surface_hover } else { t.surface2 },
                Some(if hov { t.line } else { t.line_soft }),
            );
        }
        ui.c.icon(
            ic,
            Rect::new(r.x + bw / 2. - 12., r.y + 22., 24., 24.),
            col,
            1.8,
        );
        ui.c.text_tracked(
            if armed { "CLICK AGAIN" } else { label },
            Rect::new(r.x, r.y + 54., r.w, 18.),
            &theme::label(10.5, 600),
            col,
            Align::Center,
            10.5 * 0.22,
        );
        ui.hit(r, Hit::Power(act));
    }
    ui.y += 300.;
    ui.c.text(
        "Sign out / restart / shut down need a second click.",
        Rect::new(ui.x, ui.y, ui.w, 20.),
        &theme::body(11.5, 400),
        t.muted,
        Align::Center,
    );
    ui.y += 30.;
}

fn settings(ui: &mut Ui) {
    let s = ui.s;
    let t = *ui.t;
    ui.card(None, false, |ui, y0, draw| {
        if draw {
            ui.c.icon(
                "athar",
                Rect::new(ui.x + 14., y0 + 6., 22., 22.),
                t.accent,
                1.8,
            );
            ui.c.text(
                "Solo Leveling Shell",
                Rect::new(ui.x + 46., y0, 250., 22.),
                &theme::head(16., 600),
                t.text,
                Align::Start,
            );
            ui.c.text(
                &format!("واجهة ويندوز مستوحاة من NibrasShell · {}", s.version),
                Rect::new(ui.x + 46., y0 + 22., ui.w - 60., 18.),
                &theme::body(11., 400),
                t.muted,
                Align::Start,
            );
        }
        40.
    });
    ui.card(Some("APPEARANCE"), false, |ui, y0, draw| {
        let mut y = y0;
        y += ui.toggle_row(
            y,
            "لون من الخلفية",
            "تغيير لون الواجهة وحدود النوافذ حسب الصورة.",
            s.dynamic_accent,
            "dynamicAccent",
            draw,
        );
        y += ui.toggle_row(
            y,
            "تقليل الحركة",
            "انتقالات أبسط بدون الأنيميشن.",
            s.reduce_motion,
            "reduceMotion",
            draw,
        );
        y - y0
    });
    ui.card(Some("WINDOWS · FOCUS"), false, |ui, y0, draw| {
        let mut y = y0;
        y += ui.toggle_row(
            y,
            "إخفاء الشريط الجانبي تلقائيًا",
            "يظهر لما الماوس يلمس حافة الشاشة الشمال أو لما تفتح لوحة، والنوافذ تاخد العرض كله.",
            s.leftbar_autohide,
            "leftbarAutohide",
            draw,
        );
        let label = match s.focus_pulse.as_str() {
            "heartbeat" => "نبض القلب",
            "static" => "ثابتة",
            "off" => "إيقاف",
            _ => "موجة (تنفس)",
        };
        let r = Rect::new(ui.x, y, ui.w, 48.);
        if draw {
            ui.c.text(
                "إضاءة النافذة النشطة",
                Rect::new(ui.x + 14., y + 6., ui.w - 150., 18.),
                &theme::body(12., 500),
                t.text,
                Align::End,
            );
            ui.c.text(
                "نور حوالين النافذة اللي شغال عليها.",
                Rect::new(ui.x + 14., y + 26., ui.w - 150., 16.),
                &theme::body(10., 400),
                t.muted,
                Align::End,
            );
            let sel = Rect::new(ui.x + ui.w - 14. - 116., y + 10., 116., 30.);
            ui.c.chip(sel, Radii::all(8.), t.surface2, Some(t.line));
            ui.c.text(
                label,
                Rect::new(sel.x + 10., sel.y, sel.w - 20., sel.h),
                &theme::body(11.5, 400),
                t.text,
                Align::Start,
            );
        }
        ui.hit(r, Hit::Setting("focusPulse"));
        y + 48. - y0
    });
    ui.card(Some("WALLPAPERS"), false, |ui, y0, draw| {
        let mut y = y0;
        y += ui.toggle_row(
            y,
            "تغيير الخلفية تلقائيًا",
            "من مجلد الرتبة الحالية، أو مكتبة Nibras لو المجلد فارغ.",
            s.rotate_enabled,
            "rotateEnabled",
            draw,
        );
        let r = Rect::new(ui.x, y, ui.w, 48.);
        if draw {
            ui.c.text(
                "الفاصل بين الخلفيات",
                Rect::new(ui.x + 14., y + 10., ui.w - 150., 28.),
                &theme::body(12., 500),
                t.text,
                Align::End,
            );
            let sel = Rect::new(ui.x + ui.w - 14. - 90., y + 9., 90., 30.);
            ui.c.chip(sel, Radii::all(8.), t.surface2, Some(t.line));
            let h = s.rotate_hours;
            ui.c.text(
                &if h < 1. {
                    format!("{} min", (h * 60.).round())
                } else {
                    format!("{h} h")
                },
                Rect::new(sel.x + 10., sel.y, sel.w - 20., sel.h),
                &theme::body(11.5, 400),
                t.text,
                Align::Start,
            );
        }
        ui.hit(r, Hit::Setting("rotateHours"));
        y + 48. - y0
    });
    ui.card(Some("SYSTEM"), false, |ui, y0, draw| {
        let w1 = ui.btn_w(true, "إعادة تحميل الواجهة");
        ui.btn(
            Rect::new(ui.x + 14., y0, w1, 34.),
            Some("restart"),
            "إعادة تحميل الواجهة",
            Hit::Reload,
            draw,
        );
        let w2 = ui.btn_w(true, "ويندوز");
        ui.btn(
            Rect::new(ui.x + 14. + w1 + 8., y0, w2, 34.),
            Some("settings"),
            "ويندوز",
            Hit::WindowsSettings,
            draw,
        );
        34.
    });
}

fn ai(ui: &mut Ui) {
    let s = ui.s;
    let t = *ui.t;
    ui.card(None, false, |ui, y0, draw| {
        if draw {
            ui.kicker("GEMINI · DEFAULT AI", Rect::new(ui.x + 14., y0, 200., 18.), t.accent);
            let tw = ui.c.measure(&s.ai_status, &theme::label(9.5, 600), 1.5) + 12.;
            let tr = Rect::new(ui.x + ui.w - 14. - tw, y0, tw, 18.);
            ui.c.stroke_round(tr, Radii::all(6.), t.line, 1.);
            ui.c.text_tracked(&s.ai_status, tr, &theme::label(9.5, 600), t.accent, Align::Center, 1.5);
        }
        let note = "أدخل مفتاح Google AI Studio محليًا. المفتاح بيتحفظ مشفّر بحساب ويندوز ومش بيتعرض في السجلات.";
        let nh = ui.c.wrapped_height(note, ui.w - 28., &theme::body(11.5, 400), 19.);
        if draw { ui.c.text_wrapped(note, ui.x + 14., y0 + 28., ui.w - 28., &theme::body(11.5, 400), t.muted, 19.); }
        let iy = y0 + 28. + nh + 10.;
        ui.input(Rect::new(ui.x + 14., iy, ui.w - 28., 36.), &s.ai_key_input, "Gemini API key", Input::AiKey, true, draw);
        let w1 = ui.btn_w(false, "حفظ المفتاح");
        ui.btn(Rect::new(ui.x + 14., iy + 46., w1, 34.), None, "حفظ المفتاح", Hit::AiSaveKey, draw);
        let w2 = ui.btn_w(false, "اختبار الاتصال");
        ui.btn(Rect::new(ui.x + 22. + w1, iy + 46., w2, 34.), None, "اختبار الاتصال", Hit::AiTest, draw);
        iy + 80. - y0
    });
    ui.card(Some("ASK GEMINI"), false, |ui, y0, draw| {
        let r = Rect::new(ui.x + 14., y0, ui.w - 28., 110.);
        if draw {
            let focused = ui.s.focused_input == Some(Input::AiPrompt);
            ui.c.chip(
                r,
                Radii::all(10.),
                t.surface,
                Some(if focused { t.accent } else { t.line }),
            );
            if s.ai_prompt.is_empty() {
                ui.c.text_wrapped(
                    "اسأل عن أي حاجة، أو اسأل عن الصورة المرفقة…",
                    r.x + 10.,
                    r.y + 10.,
                    r.w - 20.,
                    &theme::body(12., 400),
                    t.muted,
                    21.,
                );
            } else {
                ui.c.text_wrapped(
                    &s.ai_prompt,
                    r.x + 10.,
                    r.y + 10.,
                    r.w - 20.,
                    &theme::body(12., 400),
                    t.text,
                    21.,
                );
            }
        }
        ui.hit(r, Hit::Focus(Input::AiPrompt));
        let mut x = ui.x + 14.;
        let mut y = y0 + 120.;
        for (label, hit) in [
            ("لقطة شاشة", Hit::AiCapture),
            ("محادثة جديدة", Hit::AiClear),
            ("إرسال", Hit::AiSend),
            ("اقتراح ألوان", Hit::AiTheme),
            ("تحليل CPU / RAM", Hit::AiDiagnose),
        ] {
            let bw = ui.btn_w(false, label);
            if x + bw > ui.x + ui.w - 14. {
                x = ui.x + 14.;
                y += 40.;
            }
            ui.btn(Rect::new(x, y, bw, 34.), None, label, hit, draw);
            x += bw + 6.;
        }
        y + 34. - y0
    });
    if !s.ai_error.is_empty() {
        ui.card(None, false, |ui, y0, draw| {
            let h =
                ui.c.wrapped_height(&s.ai_error, ui.w - 28., &theme::body(12., 400), 21.);
            if draw {
                ui.c.text_wrapped(
                    &s.ai_error,
                    ui.x + 14.,
                    y0,
                    ui.w - 28.,
                    &theme::body(12., 400),
                    t.warning,
                    21.,
                );
            }
            h
        });
    }
    if s.ai_busy {
        ui.card(None, false, |ui, y0, draw| {
            if draw {
                ui.c.text(
                    "Gemini بيفكر… تقدر تستخدم باقي النظام.",
                    Rect::new(ui.x, y0, ui.w, 24.),
                    &theme::body(12., 400),
                    t.muted,
                    Align::Center,
                );
            }
            24.
        });
    } else if !s.ai_response.is_empty() {
        ui.card(Some("GEMINI · RESPONSE"), false, |ui, y0, draw| {
            let h =
                ui.c.wrapped_height(&s.ai_response, ui.w - 28., &theme::body(12., 400), 22.);
            if draw {
                ui.c.text_wrapped(
                    &s.ai_response,
                    ui.x + 14.,
                    y0,
                    ui.w - 28.,
                    &theme::body(12., 400),
                    t.text,
                    22.,
                );
            }
            h
        });
    }
}

/// Representative data for snapshots.
pub fn sample(tab: Tab) -> State {
    let ranks = [
        ('E', "#62baf8"),
        ('D', "#49eef3"),
        ('C', "#52e09e"),
        ('B', "#b981f8"),
        ('A', "#f8c54f"),
        ('S', "#f55cbd"),
    ]
    .iter()
    .map(|(k, c)| (*k, Color::hex(c)))
    .collect();
    State {
        tab: Some(tab),
        rank: 'E',
        ranks,
        greeting: "مساء النور".into(),
        date_ar: "الخميس، ١٥ أكتوبر".into(),
        wifi_on: Some(true),
        bt_on: Some(true),
        volume: Some(60),
        brightness: Some(29),
        presets: [
            "DraculaDark",
            "CatppuccinDark",
            "NordDark",
            "TokyoNightDark",
            "GruvboxDark",
            "RosePineDark",
            "SolarizedDark",
            "OxocarbonDark",
        ]
        .iter()
        .map(|x| x.to_string())
        .collect(),
        preset: "ShadowMonarch".into(),
        rotate_hours: 3.,
        wallpapers: (0..6)
            .map(|i| Wallpaper {
                id: i.to_string(),
                thumb: None,
                on: i == 0,
            })
            .collect(),
        alerts: vec![
            Alert {
                id: 1,
                title: "استهلاك مرتفع".into(),
                message: "CPU 87% · RAM 80% · Obsidian".into(),
                ago: "1h".into(),
                perf: true,
            },
            Alert {
                id: 2,
                title: "استهلاك مرتفع".into(),
                message: "CPU 47% · RAM 94% · msedgewebview2".into(),
                ago: "3h".into(),
                perf: true,
            },
        ],
        weather: Some(crate::capsule::Weather {
            status: "clear_night".into(),
            celsius: 30.,
            fahrenheit: 86.,
            wind_kmh: 4.,
            is_day: false,
        }),
        updated: "01:31 AM".into(),
        cpu: Some(18.),
        ram: Some(63.),
        battery: Some(80.),
        threads: 16,
        mem_text: "9.8G / 16G".into(),
        procs: vec![
            Proc {
                name: "firefox".into(),
                count: 14,
                cpu: 0.5,
                mem_mb: 1868,
            },
            Proc {
                name: "Code".into(),
                count: 13,
                cpu: 0.5,
                mem_mb: 2264,
            },
            Proc {
                name: "explorer".into(),
                count: 1,
                cpu: 0.1,
                mem_mb: 320,
            },
        ],
        disks: vec![
            Disk {
                name: "C:".into(),
                label: String::new(),
                free_gb: 85.,
                used_pct: 69.,
            },
            Disk {
                name: "D:".into(),
                label: "New Volume".into(),
                free_gb: 54.1,
                used_pct: 73.,
            },
        ],
        ssid: Some("HomeNetwork".into()),
        ip: "192.168.1.20/24".into(),
        down_bps: Some(0.),
        up_bps: Some(0.),
        today_bytes: 149. * 1048576.,
        month_bytes: 5. * 1073741824.,
        networks: Some(vec![WifiNet {
            ssid: "HomeNetwork".into(),
            signal: 99,
            secured: true,
            known: true,
            connected: true,
        }]),
        tasks: vec![
            Task {
                id: 1,
                text: "مراجعة التقرير الأسبوعي".into(),
                done: false,
                urgent: true,
            },
            Task {
                id: 2,
                text: "Solo Leveling ep. 12".into(),
                done: true,
                urgent: false,
            },
        ],
        dynamic_accent: true,
        leftbar_autohide: true,
        focus_pulse: "breath".into(),
        rotate_enabled: true,
        version: env!("CARGO_PKG_VERSION").into(),
        ai_status: "CONNECTED".into(),
        ..Default::default()
    }
}
