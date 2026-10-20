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
}

/// Clickable regions produced by a paint, for hit testing.
#[derive(Clone, Debug, PartialEq)]
pub enum Hit {
    Tray(String),
    TrayMore,
    Network,
    Monitor,
    Workspace(usize),
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
    for (i, (ws, w)) in s.workspaces.iter().zip(&ws_w).enumerate() {
        let r = Rect::new(wx, y, *w, h);
        let k = seg_radii(i, nws);
        let fg = if ws.focused { t.bg } else { t.muted };
        if ws.focused {
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
                    c.fill_round(br, Radii::all(3.), if ws.focused { t.bg } else { t.accent });
                    c.text(
                        &label,
                        br,
                        &theme::label(8.5, 700),
                        if ws.focused { t.accent } else { t.bg },
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
            if low { t.warning } else { t.muted },
        ));
    }
    if let Some(v) = s.ram {
        cells.push((
            "ram",
            v / 100.,
            format!("{}%", v.round()),
            if v > 90. { t.warning } else { t.accent },
        ));
    }
    if let Some(v) = s.cpu {
        cells.push((
            "cpu",
            v / 100.,
            format!("{}%", v.round()),
            if v > 85. { t.warning } else { t.accent },
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
            c.ring(cx + 10., y + h / 2., 20., 2.4, *v, t.faint, *col);
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
