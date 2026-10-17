//! Left bar, identical to the legacy leftbar.html (NibrasShell LeftBar.qml): 40 px wide, three groups
//! (top = system, centre = tools, bottom = settings + power), 30×30 buttons 10 px apart, one accent pill
//! on the open tab, count badges. Auto-hides; the runtime slides it in from the screen edge.
use crate::render::{Align, Canvas, Color, Radii, Rect};
use crate::theme::{self, Tokens, R};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Tab {
    Dashboard,
    Notifications,
    Weather,
    Monitoring,
    Network,
    Clipboard,
    Todo,
    Ai,
    Settings,
    Power,
}

impl Tab {
    pub fn icon(self) -> &'static str {
        match self {
            Tab::Dashboard => "grid",
            Tab::Notifications => "bell",
            Tab::Weather => "cloud",
            Tab::Monitoring => "activity",
            Tab::Network => "wifi",
            Tab::Clipboard => "clipboard",
            Tab::Todo => "todo",
            Tab::Ai => "athar",
            Tab::Settings => "settings",
            Tab::Power => "power",
        }
    }
    pub fn tooltip(self) -> &'static str {
        match self {
            Tab::Dashboard => "Dashboard · Win+F1",
            Tab::Notifications => "Notifications · Win+F2",
            Tab::Weather => "Weather · Win+F3",
            Tab::Monitoring => "System monitor · Win+F4",
            Tab::Network => "Network · Win+F5",
            Tab::Clipboard => "Clipboard · Win+F6",
            Tab::Todo => "To-do · Win+F7",
            Tab::Ai => "Gemini · AI",
            Tab::Settings => "Settings",
            Tab::Power => "Power · Win+Shift+L",
        }
    }
    /// Panel order (direction of the tab-switch animation).
    pub const ORDER: [Tab; 10] = [
        Tab::Dashboard,
        Tab::Notifications,
        Tab::Weather,
        Tab::Monitoring,
        Tab::Network,
        Tab::Clipboard,
        Tab::Todo,
        Tab::Ai,
        Tab::Settings,
        Tab::Power,
    ];
}

pub const GROUPS: [&[Tab]; 3] = [
    &[
        Tab::Dashboard,
        Tab::Notifications,
        Tab::Weather,
        Tab::Monitoring,
        Tab::Network,
    ],
    &[Tab::Clipboard, Tab::Todo, Tab::Ai],
    &[Tab::Settings, Tab::Power],
];

#[derive(Clone, Default)]
pub struct State {
    pub open: Option<Tab>,
    pub todo_count: usize,
    pub alert_count: usize,
    pub hover: Option<Tab>,
}

pub const WIDTH: f32 = 44.; // 40 px column + 2 px margin each side

/// Group rectangles for a bar `height` tall: top at 20, centre group vertically centred, bottom at −20.
fn group_rects(height: f32) -> [Rect; 3] {
    let gh = |n: usize| 5. + n as f32 * 30. + (n as f32 - 1.) * 10. + 5.;
    let (h0, h1, h2) = (
        gh(GROUPS[0].len()),
        gh(GROUPS[1].len()),
        gh(GROUPS[2].len()),
    );
    [
        Rect::new(2., 20., 40., h0),
        Rect::new(2., (height - h1) / 2., 40., h1),
        Rect::new(2., height - 20. - h2, 40., h2),
    ]
}

pub fn paint(c: &Canvas, height: f32, t: &Tokens, s: &State) -> Vec<(Rect, Tab)> {
    let mut hits = Vec::new();
    for (gi, (g, tabs)) in group_rects(height).iter().zip(GROUPS).enumerate() {
        for (k, a) in [(6., 0.08), (3., 0.12)] {
            c.fill_round(
                Rect::new(g.x - k, g.y - k + 4., g.w + 2. * k, g.h + 2. * k),
                Radii::all(R + k),
                Color::rgba(0, 0, 0, a),
            );
        }
        c.chip(*g, Radii::all(R), t.surface_solid, Some(t.line_soft));
        let power = gi == 2;
        for (i, tab) in tabs.iter().enumerate() {
            let b = Rect::new(g.x + 5., g.y + 5. + i as f32 * 40., 30., 30.);
            let on = s.open == Some(*tab);
            let accent = if power && *tab == Tab::Power {
                t.warning
            } else {
                t.accent
            };
            if on {
                // --glow halo + the pill
                c.fill_round(
                    Rect::new(b.x - 3., b.y - 3., 36., 36.),
                    Radii::all(R + 3.),
                    accent.alpha(0.18),
                );
                c.fill_round(b, Radii::all(R), accent);
            } else if s.hover == Some(*tab) {
                c.fill_round(b, Radii::all(9.), t.text.alpha(0.08));
            }
            let fg = if on {
                t.bg
            } else if power && *tab == Tab::Power {
                t.warning
            } else if s.hover == Some(*tab) {
                t.text
            } else {
                t.muted
            };
            c.icon(
                tab.icon(),
                Rect::new(b.x + 7.5, b.y + 7.5, 15., 15.),
                fg,
                1.8,
            );
            let count = match tab {
                Tab::Todo => s.todo_count,
                Tab::Notifications => s.alert_count,
                _ => 0,
            };
            if count > 0 {
                let label = if count > 99 {
                    "99".to_string()
                } else {
                    count.to_string()
                };
                let bw = (c.measure(&label, &theme::label(8.5, 700), 0.) + 6.).max(15.);
                let br = Rect::new(b.right() + 5. - bw, b.y - 4., bw, 15.);
                c.fill_round(
                    Rect::new(br.x - 1.5, br.y - 1.5, br.w + 3., br.h + 3.),
                    Radii::all(9.),
                    t.surface_solid,
                );
                c.fill_round(br, Radii::all(8.), if on { t.bg } else { t.accent });
                c.text(
                    &label,
                    br,
                    &theme::label(8.5, 700),
                    if on { t.accent } else { t.bg },
                    Align::Center,
                );
            }
            hits.push((b, *tab));
        }
    }
    hits
}

pub fn sample() -> State {
    State {
        open: Some(Tab::Todo),
        todo_count: 0,
        alert_count: 5,
        hover: None,
    }
}
