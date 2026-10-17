//! Capsule notification triggers, rule-for-rule from the legacy capsule.html: volume, brightness,
//! track change, charging, low battery thresholds, alerts, wallpaper. The first observation of each
//! source only records a baseline (no popup at startup).
use crate::capsule::{Request, Tone};

#[derive(Default)]
pub struct Triggers {
    vol: Option<(u8, bool)>,
    bri: Option<u8>,
    track: Option<String>,
    charging: Option<bool>,
    batt: Option<u8>,
    alert: Option<Option<u64>>,
    wall: Option<String>,
}

fn req(
    priority: u8,
    timeout_ms: u32,
    icon: &'static str,
    text: String,
    progress: Option<f32>,
    tone: Tone,
) -> Request {
    Request {
        priority,
        timeout_ms,
        icon,
        text,
        progress,
        tone,
    }
}

impl Triggers {
    pub fn volume(&mut self, volume: u8, muted: bool) -> Option<Request> {
        let prev = self.vol.replace((volume, muted))?;
        (prev != (volume, muted)).then(|| {
            if muted {
                req(2, 2000, "mute", "Muted".into(), Some(0.), Tone::Primary)
            } else {
                req(
                    2,
                    2000,
                    "vol",
                    format!("{volume}%"),
                    Some(volume as f32 / 100.),
                    Tone::Primary,
                )
            }
        })
    }
    pub fn brightness(&mut self, b: u8) -> Option<Request> {
        let prev = self.bri.replace(b)?;
        (prev != b).then(|| {
            req(
                2,
                2000,
                "sun",
                format!("{b}%"),
                Some(b as f32 / 100.),
                Tone::Primary,
            )
        })
    }
    pub fn track(&mut self, title: &str, artist: &str) -> Option<Request> {
        let key = format!("{title}|{artist}");
        let prev = self.track.replace(key.clone())?;
        (prev != key && !title.is_empty()).then(|| {
            req(
                2,
                3000,
                "music",
                if artist.is_empty() {
                    title.into()
                } else {
                    format!("{title} – {artist}")
                },
                None,
                Tone::Secondary,
            )
        })
    }
    pub fn battery(&mut self, percent: f32, charging: bool) -> Option<Request> {
        let lvl = percent.round().clamp(0., 100.) as u8;
        let mut out = None;
        if let Some(prev) = self.charging {
            if prev != charging {
                out = Some(req(
                    3,
                    3000,
                    if charging { "plug" } else { "battery" },
                    format!(
                        "{} · {lvl}%",
                        if charging { "Charging" } else { "On battery" }
                    ),
                    Some(lvl as f32 / 100.),
                    if charging { Tone::Ok } else { Tone::Primary },
                ));
            }
        }
        if out.is_none() && !charging {
            for (th, pri, ms) in [
                (5u8, 5u8, 8000u32),
                (10, 5, 6000),
                (20, 4, 4000),
                (25, 4, 4000),
            ] {
                if lvl <= th && self.batt.is_none_or(|b| b > th) {
                    out = Some(req(
                        pri,
                        ms,
                        "battery",
                        format!("Battery {lvl}%"),
                        Some(lvl as f32 / 100.),
                        if pri >= 5 { Tone::Crit } else { Tone::Warn },
                    ));
                    break;
                }
            }
        }
        self.charging = Some(charging);
        self.batt = Some(lvl);
        out
    }
    pub fn alert(&mut self, newest: Option<(u64, &str, &str)>) -> Option<Request> {
        let id = newest.map(|a| a.0);
        let prev = self.alert.replace(id)?;
        let (aid, title, message) = newest?;
        (prev != Some(aid)).then(|| {
            let crit =
                message.contains("100%") || (95..=99).any(|n| message.contains(&format!("{n}%")));
            req(
                4,
                5000,
                "activity",
                format!("{title} · {message}"),
                None,
                if crit { Tone::Crit } else { Tone::Warn },
            )
        })
    }
    pub fn wallpaper(&mut self, path: &str) -> Option<Request> {
        let prev = self.wall.replace(path.to_string())?;
        (prev != path && !path.is_empty()).then(|| {
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            req(
                2,
                3000,
                "image",
                format!("Wallpaper · {name}"),
                None,
                Tone::Primary,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn baseline_then_changes() {
        let mut t = Triggers::default();
        assert!(t.volume(40, false).is_none());
        assert!(t.volume(40, false).is_none());
        assert_eq!(t.volume(45, false).unwrap().text, "45%");
        assert_eq!(t.volume(45, true).unwrap().text, "Muted");
        assert!(t.brightness(50).is_none());
        assert_eq!(t.brightness(60).unwrap().progress, Some(0.6));
        assert!(t.track("A", "x").is_none());
        assert_eq!(t.track("B", "y").unwrap().text, "B – y");
        assert!(t.wallpaper(r"C:\w\a.jpg").is_none());
        assert_eq!(
            t.wallpaper(r"C:\w\deer.jpg").unwrap().text,
            "Wallpaper · deer.jpg"
        );
    }
    #[test]
    fn battery_thresholds_fire_once() {
        let mut t = Triggers::default();
        assert!(t.battery(30., false).is_none());
        let r = t.battery(25., false).unwrap();
        assert_eq!((r.priority, r.tone), (4, Tone::Warn));
        assert!(t.battery(24., false).is_none());
        assert_eq!(t.battery(10., false).unwrap().tone, Tone::Crit);
        assert_eq!(t.battery(10., true).unwrap().text, "Charging · 10%");
        assert!(
            t.battery(4., true).is_none(),
            "no low warning while charging"
        );
    }
    #[test]
    fn alerts_use_legacy_severity() {
        let mut t = Triggers::default();
        assert!(t.alert(None).is_none());
        assert_eq!(
            t.alert(Some((1, "High", "CPU 37% · RAM 99%")))
                .unwrap()
                .tone,
            Tone::Crit
        );
        assert!(t.alert(Some((1, "High", "CPU 37% · RAM 99%"))).is_none());
        assert_eq!(
            t.alert(Some((2, "High", "CPU 88% · RAM 82%")))
                .unwrap()
                .tone,
            Tone::Warn
        );
    }
}
