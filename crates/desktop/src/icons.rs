//! Lucide icon markup → Direct2D path geometry (stroke 24×24 viewBox, `fill="currentColor"` shapes filled).
use windows::{
    core::Result,
    Win32::Graphics::Direct2D::{Common::*, *},
};
use windows_numerics::Vector2;

#[path = "icons_data.rs"]
mod data;

pub fn get(name: &str) -> Option<&'static str> {
    data::ICONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| *s)
}

pub struct Shape {
    pub kind: &'static str,
    attrs: Vec<(String, String)>,
    pub filled: bool,
}
impl Shape {
    fn num(&self, k: &str) -> f32 {
        self.attrs
            .iter()
            .find(|(a, _)| a == k)
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0.)
    }
    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(a, _)| a == k)
            .map(|(_, v)| v.as_str())
    }
}

/// Split `<path d=".."/><circle .../>` markup into shapes.
pub fn shapes(svg: &str) -> Vec<Shape> {
    let mut out = Vec::new();
    for part in svg.split('<').skip(1) {
        let mut it = part.splitn(2, |c: char| c.is_whitespace());
        let tag = it.next().unwrap_or("");
        let kind: &'static str = match tag {
            "path" => "path",
            "circle" => "circle",
            "rect" => "rect",
            "ellipse" => "ellipse",
            _ => continue,
        };
        let rest = it.next().unwrap_or("");
        let mut attrs = Vec::new();
        let mut s = rest;
        while let Some(eq) = s.find("=\"") {
            let key = s[..eq].trim().trim_start_matches('/').to_string();
            let after = &s[eq + 2..];
            let Some(close) = after.find('"') else { break };
            attrs.push((key, after[..close].to_string()));
            s = &after[close + 1..];
        }
        let filled = attrs
            .iter()
            .any(|(k, v)| k == "fill" && v == "currentColor");
        out.push(Shape {
            kind,
            attrs,
            filled,
        });
    }
    out
}

fn tokens(d: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let b = d.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        if c.is_ascii_alphabetic() {
            out.push(Tok::Cmd(c));
            i += 1;
            continue;
        }
        if c == '-' || c == '.' || c.is_ascii_digit() {
            let start = i;
            let mut seen_dot = false;
            if c == '-' {
                i += 1;
            }
            while i < b.len() {
                let ch = b[i] as char;
                if ch.is_ascii_digit() {
                    i += 1;
                } else if ch == '.' && !seen_dot {
                    seen_dot = true;
                    i += 1;
                } else if (ch == 'e' || ch == 'E') && i + 1 < b.len() {
                    i += 1;
                    if b[i] == b'-' || b[i] == b'+' {
                        i += 1;
                    }
                } else {
                    break;
                }
            }
            if let Ok(v) = d[start..i].parse() {
                out.push(Tok::Num(v));
            }
            continue;
        }
        i += 1;
    }
    out
}
enum Tok {
    Cmd(char),
    Num(f32),
}

/// SVG path data → D2D geometry. Arc flags are single digits that may be packed ("0 01 1").
pub fn geometry(f: &ID2D1Factory, s: &Shape) -> Result<ID2D1PathGeometry> {
    unsafe {
        let g = f.CreatePathGeometry()?;
        let sink = g.Open()?;
        let begin = if s.filled {
            D2D1_FIGURE_BEGIN_FILLED
        } else {
            D2D1_FIGURE_BEGIN_HOLLOW
        };
        let p = |x: f32, y: f32| Vector2 { X: x, Y: y };
        match s.kind {
            "circle" | "ellipse" => {
                let (cx, cy) = (s.num("cx"), s.num("cy"));
                let (rx, ry) = if s.kind == "circle" {
                    (s.num("r"), s.num("r"))
                } else {
                    (s.num("rx"), s.num("ry"))
                };
                let arc = |x: f32, y: f32| D2D1_ARC_SEGMENT {
                    point: p(x, y),
                    size: D2D_SIZE_F {
                        width: rx,
                        height: ry,
                    },
                    rotationAngle: 0.,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: D2D1_ARC_SIZE_SMALL,
                };
                sink.BeginFigure(p(cx - rx, cy), begin);
                sink.AddArc(&arc(cx + rx, cy));
                sink.AddArc(&arc(cx - rx, cy));
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            }
            "rect" => {
                let (x, y, w, h) = (s.num("x"), s.num("y"), s.num("width"), s.num("height"));
                let r = s.num("rx").min(w / 2.).min(h / 2.);
                let arc = |ax: f32, ay: f32| D2D1_ARC_SEGMENT {
                    point: p(ax, ay),
                    size: D2D_SIZE_F {
                        width: r,
                        height: r,
                    },
                    rotationAngle: 0.,
                    sweepDirection: D2D1_SWEEP_DIRECTION_CLOCKWISE,
                    arcSize: D2D1_ARC_SIZE_SMALL,
                };
                sink.BeginFigure(p(x + r, y), begin);
                sink.AddLine(p(x + w - r, y));
                if r > 0. {
                    sink.AddArc(&arc(x + w, y + r));
                }
                sink.AddLine(p(x + w, y + h - r));
                if r > 0. {
                    sink.AddArc(&arc(x + w - r, y + h));
                }
                sink.AddLine(p(x + r, y + h));
                if r > 0. {
                    sink.AddArc(&arc(x, y + h - r));
                }
                sink.AddLine(p(x, y + r));
                if r > 0. {
                    sink.AddArc(&arc(x + r, y));
                }
                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
            }
            _ => {
                let toks = tokens(s.attr("d").unwrap_or(""));
                let (mut cx, mut cy, mut sx, mut sy) = (0f32, 0f32, 0f32, 0f32);
                let (mut lcx, mut lcy) = (0f32, 0f32); // last control point (S / T reflection)
                let mut open = false;
                let mut cmd = 'M';
                let mut i = 0;
                let mut nums: Vec<f32> = Vec::new();
                macro_rules! need {
                    ($n:expr) => {{
                        nums.clear();
                        while nums.len() < $n {
                            match toks.get(i) {
                                Some(Tok::Num(v)) => {
                                    nums.push(*v);
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        nums.len() == $n
                    }};
                }
                while i < toks.len() {
                    if let Tok::Cmd(c) = toks[i] {
                        cmd = c;
                        i += 1;
                        if c == 'Z' || c == 'z' {
                            if open {
                                sink.EndFigure(D2D1_FIGURE_END_CLOSED);
                                open = false;
                            }
                            cx = sx;
                            cy = sy;
                            continue;
                        }
                    }
                    let rel = cmd.is_ascii_lowercase();
                    let (ox, oy) = if rel { (cx, cy) } else { (0., 0.) };
                    match cmd.to_ascii_uppercase() {
                        'M' => {
                            if !need!(2) {
                                break;
                            }
                            if open {
                                sink.EndFigure(D2D1_FIGURE_END_OPEN);
                            }
                            cx = ox + nums[0];
                            cy = oy + nums[1];
                            sx = cx;
                            sy = cy;
                            sink.BeginFigure(p(cx, cy), begin);
                            open = true;
                            cmd = if rel { 'l' } else { 'L' }; // subsequent pairs are line-to
                            lcx = cx;
                            lcy = cy;
                        }
                        'L' => {
                            if !need!(2) {
                                break;
                            }
                            cx = ox + nums[0];
                            cy = oy + nums[1];
                            sink.AddLine(p(cx, cy));
                            lcx = cx;
                            lcy = cy;
                        }
                        'H' => {
                            if !need!(1) {
                                break;
                            }
                            cx = if rel { cx + nums[0] } else { nums[0] };
                            sink.AddLine(p(cx, cy));
                            lcx = cx;
                            lcy = cy;
                        }
                        'V' => {
                            if !need!(1) {
                                break;
                            }
                            cy = if rel { cy + nums[0] } else { nums[0] };
                            sink.AddLine(p(cx, cy));
                            lcx = cx;
                            lcy = cy;
                        }
                        'C' => {
                            if !need!(6) {
                                break;
                            }
                            let (x1, y1, x2, y2, x, y) = (
                                ox + nums[0],
                                oy + nums[1],
                                ox + nums[2],
                                oy + nums[3],
                                ox + nums[4],
                                oy + nums[5],
                            );
                            sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                                point1: p(x1, y1),
                                point2: p(x2, y2),
                                point3: p(x, y),
                            });
                            lcx = x2;
                            lcy = y2;
                            cx = x;
                            cy = y;
                        }
                        'S' => {
                            if !need!(4) {
                                break;
                            }
                            let (x1, y1) = (2. * cx - lcx, 2. * cy - lcy);
                            let (x2, y2, x, y) =
                                (ox + nums[0], oy + nums[1], ox + nums[2], oy + nums[3]);
                            sink.AddBezier(&D2D1_BEZIER_SEGMENT {
                                point1: p(x1, y1),
                                point2: p(x2, y2),
                                point3: p(x, y),
                            });
                            lcx = x2;
                            lcy = y2;
                            cx = x;
                            cy = y;
                        }
                        'Q' => {
                            if !need!(4) {
                                break;
                            }
                            let (x1, y1, x, y) =
                                (ox + nums[0], oy + nums[1], ox + nums[2], oy + nums[3]);
                            sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                                point1: p(x1, y1),
                                point2: p(x, y),
                            });
                            lcx = x1;
                            lcy = y1;
                            cx = x;
                            cy = y;
                        }
                        'T' => {
                            if !need!(2) {
                                break;
                            }
                            let (x1, y1) = (2. * cx - lcx, 2. * cy - lcy);
                            let (x, y) = (ox + nums[0], oy + nums[1]);
                            sink.AddQuadraticBezier(&D2D1_QUADRATIC_BEZIER_SEGMENT {
                                point1: p(x1, y1),
                                point2: p(x, y),
                            });
                            lcx = x1;
                            lcy = y1;
                            cx = x;
                            cy = y;
                        }
                        'A' => {
                            // rx ry rot large sweep x y  (flags may be packed without separators)
                            nums.clear();
                            let mut take = |nums: &mut Vec<f32>, flag: bool| -> bool {
                                match toks.get(i) {
                                    Some(Tok::Num(v)) => {
                                        nums.push(*v);
                                        i += 1;
                                        let _ = flag;
                                        true
                                    }
                                    _ => false,
                                }
                            };
                            let mut ok = true;
                            for _ in 0..7 {
                                ok &= take(&mut nums, false);
                            }
                            if !ok {
                                break;
                            }
                            let (rx, ry, rot, large, sweep) =
                                (nums[0], nums[1], nums[2], nums[3] != 0., nums[4] != 0.);
                            let (x, y) = (ox + nums[5], oy + nums[6]);
                            sink.AddArc(&D2D1_ARC_SEGMENT {
                                point: p(x, y),
                                size: D2D_SIZE_F {
                                    width: rx,
                                    height: ry,
                                },
                                rotationAngle: rot,
                                sweepDirection: if sweep {
                                    D2D1_SWEEP_DIRECTION_CLOCKWISE
                                } else {
                                    D2D1_SWEEP_DIRECTION_COUNTER_CLOCKWISE
                                },
                                arcSize: if large {
                                    D2D1_ARC_SIZE_LARGE
                                } else {
                                    D2D1_ARC_SIZE_SMALL
                                },
                            });
                            cx = x;
                            cy = y;
                            lcx = cx;
                            lcy = cy;
                        }
                        _ => {
                            i += 1;
                        }
                    }
                }
                if open {
                    sink.EndFigure(D2D1_FIGURE_END_OPEN);
                }
            }
        }
        sink.Close()?;
        Ok(g)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_icon_parses() {
        for (name, svg) in data::ICONS {
            assert!(!shapes(svg).is_empty(), "{name}");
        }
        assert!(get("wifi").is_some() && get("nope").is_none());
    }
}
