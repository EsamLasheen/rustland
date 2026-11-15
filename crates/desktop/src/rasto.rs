//! Rasto's life outside the bar. A trip is a list of legs: jumps between window edges, walks along
//! them, and idle stops where he keeps moving (a character that stands perfectly still reads as a
//! frozen program). The path and his remarks are pure so they can be tested; the window, timers and
//! choice of route live in `runtime.rs`. Positions are physical pixels of his top-left corner.
use std::f32::consts::{PI, TAU};
use std::time::Instant;

const JUMP_MS: f32 = 700.;
const JUMP_HEIGHT: f32 = 46.;
const WALK_PX_PER_S: f32 = 90.;
const WALK_MAX_MS: f32 = 7000.;
const LAND_MS: f32 = 160.;
const STEP_MS: f32 = 170.;
const BLINK_EVERY_MS: f32 = 2600.;
const BLINK_MS: f32 = 160.;
/// Windows further from the pointer than this (in his sizes) do not scare him.
pub const SCARE_RADIUS: f32 = 2.6;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Leg {
    /// Jump to this position.
    Jump(f32, f32),
    /// Walk along the current floor to this x.
    Walk(f32),
    /// Stay here, fidgeting, for this many ms.
    Idle(f32),
}

impl Leg {
    fn ms(&self, at: (f32, f32)) -> f32 {
        match *self {
            Leg::Jump(..) => JUMP_MS,
            Leg::Walk(x) => ((x - at.0).abs() / WALK_PX_PER_S * 1000.).min(WALK_MAX_MS),
            Leg::Idle(ms) => ms,
        }
    }
}

pub struct Trip {
    pub start: Instant,
    pub home: (f32, f32),
    pub legs: Vec<Leg>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    /// In the air: wide grin.
    pub jumping: bool,
    /// Tail side: true when he last moved to the right.
    pub facing_right: bool,
    /// Walking: phase of his steps (radians).
    pub step: Option<f32>,
    /// Just landed: 1 flattens him, 0 is his normal shape.
    pub squash: f32,
    /// Blinking (0..1), also while moving.
    pub blink: Option<f32>,
    /// Tail swing -1..1, always moving.
    pub wag: f32,
    /// Idle: pupils wandering, -1..1.
    pub look: f32,
}

impl Trip {
    /// Where he is at `now`; `None` once the last leg is over.
    pub fn frame(&self, now: Instant) -> Option<Frame> {
        self.frame_at(now.duration_since(self.start).as_secs_f32() * 1000.)
    }

    fn frame_at(&self, t: f32) -> Option<Frame> {
        let mut f = Frame {
            x: self.home.0,
            y: self.home.1,
            facing_right: true,
            wag: (t / 260.).sin(),
            blink: {
                let phase = t % BLINK_EVERY_MS;
                (phase < BLINK_MS).then_some(phase / BLINK_MS)
            },
            ..Default::default()
        };
        let mut at = self.home;
        let mut left = t;
        for leg in &self.legs {
            let len = leg.ms(at);
            if left >= len {
                left -= len;
                at = match *leg {
                    Leg::Jump(x, y) => (x, y),
                    Leg::Walk(x) => {
                        f.facing_right = x >= at.0;
                        (x, at.1)
                    }
                    Leg::Idle(_) => at,
                };
                // the landing shows in whatever follows the jump
                f.squash = if matches!(leg, Leg::Jump(..)) && left < LAND_MS {
                    1. - left / LAND_MS
                } else {
                    0.
                };
                continue;
            }
            let p = if len > 0. { left / len } else { 1. };
            match *leg {
                Leg::Jump(x, y) => {
                    f.facing_right = x >= at.0;
                    f.x = at.0 + (x - at.0) * p;
                    f.y = at.1 + (y - at.1) * p - JUMP_HEIGHT * 4. * p * (1. - p);
                    f.jumping = true;
                    f.squash = 0.;
                }
                Leg::Walk(x) => {
                    f.facing_right = x >= at.0;
                    f.x = at.0 + (x - at.0) * p;
                    let step = left / STEP_MS * PI;
                    f.y = at.1 - step.sin().abs() * 1.8;
                    f.step = Some(step);
                }
                Leg::Idle(_) => {
                    f.x = at.0;
                    // breathing, and a look around
                    f.y = at.1 - ((left / 900.) * TAU).sin().abs() * 0.8;
                    f.look = (left / 1100.).sin();
                }
            }
            return Some(f);
        }
        None
    }
}

/// Walking span along an edge from `left` to `right`, starting on the side nearer `home_x`, kept
/// `margin` inside both ends. `None` when the edge is too short to walk on.
pub fn span(left: f32, right: f32, home_x: f32, margin: f32) -> Option<(f32, f32)> {
    let (a, b) = (left + margin, right - margin);
    if b - a < 40. {
        return None;
    }
    Some(if (home_x - a).abs() <= (home_x - b).abs() {
        (a, b)
    } else {
        (b, a)
    })
}

/// How much the pointer scares him: 0 far away, 1 on top of him. `side` is his size.
pub fn scare(cursor: (f32, f32), center: (f32, f32), side: f32) -> f32 {
    let d = (cursor.0 - center.0).hypot(cursor.1 - center.1);
    (1. - d / (side * SCARE_RADIUS)).clamp(0., 1.)
}

/// His pupils turn toward the pointer: offset -1..1 on each axis.
pub fn gaze(cursor: (f32, f32), center: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (cursor.0 - center.0, cursor.1 - center.1);
    let d = dx.hypot(dy).max(1.);
    (dx / d, dy / d)
}

/// What he has to say about the machine, if anything. `hour` is local time.
pub fn remark(cpu: Option<f32>, ram: Option<f32>, battery: Option<(f32, bool)>, hour: u16) -> Option<&'static str> {
    if battery.is_some_and(|(p, charging)| p < 15. && !charging) {
        Some("راستو: البطارية بتموت... الحقها قبل ما أنام")
    } else if ram.is_some_and(|v| v >= 90.) {
        Some("راستو: أكلت الرام كلها زي ما قلتلك")
    } else if cpu.is_some_and(|v| v >= 90.) {
        Some("راستو: الجهاز بيعرق... أنا السبب طبعًا")
    } else if (1..5).contains(&hour) {
        Some("راستو: نام بقى، حتى الأشرار بتنام")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trip() -> Trip {
        Trip {
            start: Instant::now(),
            home: (1300., 4.),
            legs: vec![Leg::Jump(1000., 60.), Leg::Walk(1090.), Leg::Idle(2000.), Leg::Jump(1300., 4.)],
        }
    }

    #[test]
    fn follows_its_legs_and_ends_home() {
        let t = trip();
        assert_eq!(t.frame_at(0.).map(|f| (f.x, f.y)), Some((1300., 4.)));
        assert!(t.frame_at(100.).unwrap().jumping);
        // 90 px at 90 px/s = 1 s of walking after the 0.7 s jump
        let mid = t.frame_at(700. + 500.).unwrap();
        assert!((mid.x - 1045.).abs() < 1. && mid.step.is_some() && mid.facing_right);
        assert!(t.frame_at(700. + 1000. + 2000. + 100.).unwrap().jumping);
        assert!(t.frame_at(700. + 1000. + 2000. + 701.).is_none());
    }

    #[test]
    fn he_never_stands_still_while_out() {
        let t = trip();
        // the idle leg: something visible changes within any 100 ms
        for ms in (1800..3600).step_by(100) {
            let (a, b) = (t.frame_at(ms as f32).unwrap(), t.frame_at(ms as f32 + 100.).unwrap());
            assert_ne!(a, b, "frozen at {ms} ms");
        }
    }

    #[test]
    fn lands_with_a_squash_that_fades() {
        let t = trip();
        assert!(t.frame_at(701.).unwrap().squash > 0.9);
        assert_eq!(t.frame_at(700. + 200.).unwrap().squash, 0.);
    }

    #[test]
    fn walks_are_bounded_on_wide_windows() {
        let t = Trip {
            legs: vec![Leg::Jump(0., 60.), Leg::Walk(50_000.)],
            ..trip()
        };
        assert!(t.frame_at(700. + WALK_MAX_MS + 1.).is_none());
    }

    #[test]
    fn span_starts_near_home_and_rejects_short_edges() {
        assert_eq!(span(100., 900., 1300., 20.), Some((880., 120.)));
        assert_eq!(span(100., 900., 0., 20.), Some((120., 880.)));
        assert_eq!(span(100., 150., 0., 20.), None);
    }

    #[test]
    fn pointer_scares_him_only_up_close() {
        assert_eq!(scare((500., 500.), (0., 0.), 34.), 0.);
        assert_eq!(scare((10., 10.), (10., 10.), 34.), 1.);
        let (x, y) = gaze((110., 10.), (10., 10.));
        assert!((x - 1.).abs() < 1e-3 && y.abs() < 1e-3);
    }

    #[test]
    fn remarks_put_the_urgent_ones_first() {
        assert_eq!(remark(Some(10.), Some(50.), Some((80., true)), 14), None);
        assert!(remark(Some(95.), Some(95.), Some((10., false)), 3).unwrap().contains("البطارية"));
        assert!(remark(Some(95.), Some(95.), Some((10., true)), 14).unwrap().contains("الرام"));
        assert!(remark(None, None, None, 2).unwrap().contains("نام"));
    }
}
