//! Event-driven micro-animations for the top bar ("game HUD" feel at ~zero idle cost).
//!
//! Nothing here owns a timer: [`BarFx::frame`] turns the current time into the bar's animation
//! state and says when it wants the next frame — [`FRAME`] (paced to the display) while something moves, the wait until the
//! next scheduled effect otherwise, or never. Effects are short and triggered by events:
//! * plugged in: an energy wave refills the battery ring (every 4 s for a minute, then every 30 s);
//! * reaching 100 % while charging: one bright flash;
//! * low battery on DC: a short warning pulse every 20 s (not a continuous blink);
//! * workspace switch: the highlight slides to the new workspace;
//! * Rasto, the bar's character: a blink every few seconds, a hop on workspace switch or click;
//! * CPU / RAM: a jump of 8+ points counts up to the new reading — only on AC; small changes and
//!   anything on battery just change.
use crate::topbar::{self, Fx};
use std::time::{Duration, Instant};

/// Delay meaning "next display refresh" (see `pacer.rs`); longer delays are plain timers.
pub const FRAME: u64 = 16;
/// Ambient effects nobody triggered (charging wave, full flash, low pulse): a plain 30 fps timer,
/// a repaint per refresh on a 144 Hz display would cost a sixth of a core for a small ring.
const AMBIENT: u64 = 33;
const WAVE_MS: f32 = 1400.;
/// Waves come often right after plugging in (feedback), then rarely (a heartbeat).
const WAVE_PERIOD: Duration = Duration::from_secs(4);
const WAVE_EAGER: Duration = Duration::from_secs(60);
const WAVE_CALM: Duration = Duration::from_secs(30);
const FULL_MS: f32 = 1600.;
const LOW_MS: f32 = 900.;
const LOW_PERIOD: Duration = Duration::from_secs(20);
const SLIDE_MS: f32 = 240.;
const TWEEN_MS: f32 = 360.;
const BLINK_MS: f32 = 180.;
const BLINK_PERIOD: Duration = Duration::from_secs(6);
const HOP_MS: f32 = 520.;
/// Each blink starts a short idle moment (a breath, a tail flick, a look around, a stretch), so he
/// moves like something alive while an idle bar still sleeps most of the time.
const LIFE_MS: f32 = 1500.;

#[derive(Default)]
pub struct BarFx {
    charging_since: Option<Instant>,
    full_at: Option<Instant>,
    low_since: Option<Instant>,
    last_percent: Option<f32>,
    slide: Option<(usize, Instant)>,
    tween: Option<Tween>,
    /// The companion is shown (its blink is the only effect that repeats on an idle bar).
    pub pet: bool,
    /// The companion has a window of his own: his frames repaint that, never the whole bar.
    pub pet_apart: bool,
    pet_since: Option<Instant>,
    hop: Option<Instant>,
}

struct Tween {
    at: Instant,
    from: [Option<f32>; 2],
    to: [Option<f32>; 2],
}

/// Ease-out cubic: fast start, soft landing.
pub fn ease(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    1. - (1. - t).powi(3)
}

fn ms(d: Duration) -> f32 {
    d.as_secs_f32() * 1000.
}

/// Progress of a repeating effect `len_ms` long every `period` since `since`: `Ok(progress)`
/// while it plays, `Err(wait)` until the next one starts.
fn repeating(now: Instant, since: Instant, len_ms: f32, period: Duration) -> Result<f32, u64> {
    let phase = ms(now.duration_since(since)) % ms(period);
    if phase < len_ms {
        Ok(phase / len_ms)
    } else {
        Err((ms(period) - phase).ceil() as u64)
    }
}

impl BarFx {
    /// New battery reading.
    pub fn on_battery(&mut self, now: Instant, percent: f32, charging: bool) {
        if charging {
            self.charging_since.get_or_insert(now);
            if percent >= 99.5 && self.last_percent.is_some_and(|p| p < 99.5) {
                self.full_at = Some(now);
            }
        } else {
            self.charging_since = None;
        }
        if !charging && percent < 20. {
            self.low_since.get_or_insert(now);
        } else {
            self.low_since = None;
        }
        self.last_percent = Some(percent);
    }

    /// The focused workspace moved from `from` (index in the bar) to another one.
    pub fn on_workspace(&mut self, now: Instant, from: usize) {
        self.slide = Some((from, now));
        self.hop = Some(now);
    }

    /// The companion was clicked.
    pub fn on_pet(&mut self, now: Instant) {
        self.hop = Some(now);
    }

    /// New CPU / RAM reading: counted up to when `smooth`, else shown at once.
    pub fn on_metrics(
        &mut self,
        now: Instant,
        shown: [Option<f32>; 2],
        next: [Option<f32>; 2],
        smooth: bool,
    ) {
        let moved = shown
            .iter()
            .zip(&next)
            // only a real jump is worth animating: small jitter would repaint every sample
            .any(|(a, b)| matches!((a, b), (Some(a), Some(b)) if (a - b).abs() >= 8.));
        self.tween = (smooth && moved).then_some(Tween {
            at: now,
            from: shown,
            to: next,
        });
    }

    /// Apply the animation state for `now` to the bar and return the delay until the next frame
    /// (`None`: nothing scheduled). `quiet` (screen unseen, fullscreen app, reduced motion) shows
    /// end states only and schedules nothing.
    /// The companion's blink, glance, idle moment and hop for `now`; the delay to his next frame.
    fn pet_into(&mut self, now: Instant, fx: &mut Fx) -> Option<u64> {
        let mut next: Option<u64> = None;
        let mut want = |d: u64| next = Some(next.map_or(d, |n| n.min(d)));
            let since = *self.pet_since.get_or_insert(now);
            // a glance to the side changes with every blink, so he costs no frames in between
            let beats = (ms(now.duration_since(since)) / ms(BLINK_PERIOD)) as usize;
            fx.look = [0., -1., 0., 1.][beats % 4];
            match repeating(now, since, BLINK_MS, BLINK_PERIOD) {
                Ok(p) => {
                    fx.blink = Some(p);
                    want(AMBIENT);
                }
                Err(wait) => want(wait),
            }
            match repeating(now, since, LIFE_MS, BLINK_PERIOD) {
                Ok(p) => {
                    fx.life = Some(((beats % 4) as u8, p));
                    want(AMBIENT);
                }
                Err(wait) => want(wait),
            }
            if let Some(at) = self.hop {
                let p = ms(now.duration_since(at)) / HOP_MS;
                if p < 1. {
                    fx.hop = Some(p);
                    want(FRAME);
                } else {
                    self.hop = None;
                }
            }
        next
    }

    /// The companion's frame when he is drawn in his own window (see [`BarFx::pet_apart`]).
    pub fn pet_frame(&mut self, now: Instant) -> (Fx, Option<u64>) {
        let mut fx = Fx::default();
        let next = self.pet_into(now, &mut fx);
        (fx, next)
    }

    pub fn frame(&mut self, now: Instant, top: &mut topbar::State, quiet: bool) -> Option<u64> {
        let mut fx = Fx::default();
        let mut next: Option<u64> = None;
        let mut want = |d: u64| next = Some(next.map_or(d, |n| n.min(d)));

        if let Some(t) = self.tween.as_ref() {
            let p = ms(now.duration_since(t.at)) / TWEEN_MS;
            let lerp = |a: Option<f32>, b: Option<f32>| match (a, b) {
                (Some(a), Some(b)) if !quiet && p < 1. => Some(a + (b - a) * ease(p)),
                _ => b,
            };
            top.cpu = lerp(t.from[0], t.to[0]);
            top.ram = lerp(t.from[1], t.to[1]);
            if quiet || p >= 1. {
                self.tween = None;
            } else {
                want(FRAME);
            }
        }
        if quiet {
            self.slide = None;
            self.full_at = None;
            self.hop = None;
            top.fx = fx;
            return None;
        }
        if let Some((from, at)) = self.slide {
            let p = ms(now.duration_since(at)) / SLIDE_MS;
            if p < 1. {
                fx.slide_from = Some((from, ease(p)));
                want(FRAME);
            } else {
                self.slide = None;
            }
        }
        if let Some(at) = self.full_at {
            let p = ms(now.duration_since(at)) / FULL_MS;
            if p < 1. {
                fx.full = Some(p);
                want(AMBIENT);
            } else {
                self.full_at = None;
            }
        }
        if let Some(since) = self.charging_since {
            let eager = now.duration_since(since) < WAVE_EAGER;
            let wave = if eager {
                repeating(now, since, WAVE_MS, WAVE_PERIOD)
            } else {
                repeating(now, since + WAVE_EAGER, WAVE_MS, WAVE_CALM)
            };
            match wave {
                Ok(p) => {
                    fx.charge = Some(p);
                    want(AMBIENT);
                }
                Err(wait) => want(wait),
            }
        }
        if let Some(since) = self.low_since {
            match repeating(now, since, LOW_MS, LOW_PERIOD) {
                Ok(p) => {
                    fx.low = Some(p);
                    want(AMBIENT);
                }
                Err(wait) => want(wait),
            }
        }
        if self.pet && !self.pet_apart {
            if let Some(w) = self.pet_into(now, &mut fx) {
                want(w);
            }
        }
        top.fx = fx;
        next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_bar_schedules_nothing() {
        let mut fx = BarFx::default();
        let mut top = topbar::State::default();
        let now = Instant::now();
        fx.on_battery(now, 80., false);
        assert_eq!(fx.frame(now, &mut top, false), None);
        assert_eq!(top.fx, Fx::default());
    }

    #[test]
    fn charging_waves_then_waits() {
        let mut fx = BarFx::default();
        let mut top = topbar::State::default();
        let t0 = Instant::now();
        fx.on_battery(t0, 50., true);
        assert_eq!(fx.frame(t0, &mut top, false), Some(AMBIENT));
        assert!(top.fx.charge.is_some());
        let later = t0 + Duration::from_millis(2000);
        let wait = fx.frame(later, &mut top, false).unwrap();
        assert!(top.fx.charge.is_none());
        assert_eq!(wait, 2000);
        // unplugged: back to nothing
        fx.on_battery(later, 50., false);
        assert_eq!(fx.frame(later, &mut top, false), None);
    }

    #[test]
    fn full_charge_flashes_once() {
        let mut fx = BarFx::default();
        let mut top = topbar::State::default();
        let t0 = Instant::now();
        fx.on_battery(t0, 98., true);
        fx.on_battery(t0, 100., true);
        fx.frame(t0, &mut top, false);
        assert!(top.fx.full.is_some());
        fx.frame(t0 + Duration::from_secs(2), &mut top, false);
        assert!(top.fx.full.is_none());
    }

    #[test]
    fn metrics_count_up_on_ac_and_land_exactly() {
        let mut fx = BarFx::default();
        let mut top = topbar::State::default();
        let t0 = Instant::now();
        fx.on_metrics(t0, [Some(10.), Some(50.)], [Some(60.), Some(50.)], true);
        fx.frame(t0 + Duration::from_millis(100), &mut top, false);
        let mid = top.cpu.unwrap();
        assert!(mid > 10. && mid < 60.);
        assert_eq!(fx.frame(t0 + Duration::from_secs(1), &mut top, false), None);
        assert_eq!(top.cpu, Some(60.));
        // on battery the value jumps
        fx.on_metrics(t0, [Some(60.), Some(50.)], [Some(5.), Some(50.)], false);
        assert_eq!(fx.frame(t0, &mut top, false), None);
    }

    #[test]
    fn pet_blinks_and_hops() {
        let mut fx = BarFx {
            pet: true,
            ..Default::default()
        };
        let mut top = topbar::State::default();
        let t0 = Instant::now();
        fx.frame(t0, &mut top, false);
        assert!(top.fx.blink.is_some());
        // between blinks the bar sleeps until the next one
        let wait = fx.frame(t0 + Duration::from_secs(2), &mut top, false).unwrap();
        assert!(top.fx.blink.is_none() && top.fx.life.is_none());
        assert_eq!(wait, 4000);
        let t1 = t0 + Duration::from_secs(2);
        fx.on_pet(t1);
        assert_eq!(fx.frame(t1, &mut top, false), Some(FRAME));
        assert!(top.fx.hop.is_some());
        fx.frame(t1 + Duration::from_secs(1), &mut top, false);
        assert!(top.fx.hop.is_none());
    }

    #[test]
    fn quiet_shows_end_states_only() {
        let mut fx = BarFx::default();
        let mut top = topbar::State::default();
        let t0 = Instant::now();
        fx.on_battery(t0, 50., true);
        fx.on_workspace(t0, 2);
        assert_eq!(fx.frame(t0, &mut top, true), None);
        assert_eq!(top.fx, Fx::default());
    }
}
