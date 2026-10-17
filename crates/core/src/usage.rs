//! Network data usage per local day and performance alerts — same files and rules as the legacy daemon
//! (network-usage.json / alerts.json), so history carries over.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Bytes {
    pub down: f64,
    pub up: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    #[serde(default)]
    pub days: BTreeMap<String, Bytes>,
    #[serde(default)]
    pub since: Option<u64>,
}

pub const KEEP_DAYS: usize = 62;

impl Usage {
    /// Add a counter delta to `day` ("YYYY-MM-DD"); negative deltas (counter reset) are ignored.
    pub fn add(&mut self, day: &str, down: f64, up: f64, now_ms: u64) {
        let e = self.days.entry(day.to_string()).or_default();
        e.down += down.max(0.);
        e.up += up.max(0.);
        self.since.get_or_insert(now_ms);
        while self.days.len() > KEEP_DAYS {
            let first = self.days.keys().next().cloned();
            if let Some(k) = first {
                self.days.remove(&k);
            }
        }
    }
    /// (today, this month) for `day`.
    pub fn summary(&self, day: &str) -> (Bytes, Bytes) {
        let today = self.days.get(day).copied().unwrap_or_default();
        let month = day.get(..7).unwrap_or(day);
        let m = self.days.iter().filter(|(k, _)| k.starts_with(month)).fold(
            Bytes::default(),
            |a, (_, v)| Bytes {
                down: a.down + v.down,
                up: a.up + v.up,
            },
        );
        (today, m)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Alert {
    pub id: u64,
    pub kind: String,
    pub title: String,
    pub message: String,
    pub time: u64,
}

pub const MAX_ALERTS: usize = 40;

pub fn push_alert(list: &mut Vec<Alert>, a: Alert) {
    list.insert(0, a);
    list.truncate(MAX_ALERTS);
}

/// Sustained-load detector: 5 consecutive hot samples (CPU ≥ 85 % or RAM ≥ 92 %), 10 min cooldown.
#[derive(Clone, Debug, Default)]
pub struct HotDetector {
    hot: u32,
    last_alert_ms: u64,
}
impl HotDetector {
    pub fn sample(&mut self, cpu: f64, mem: f64, now_ms: u64) -> bool {
        self.hot = if cpu >= 85. || mem >= 92. {
            self.hot + 1
        } else {
            0
        };
        if self.hot >= 5 && now_ms.saturating_sub(self.last_alert_ms) > 600_000 {
            self.hot = 0;
            self.last_alert_ms = now_ms;
            return true;
        }
        false
    }
}

/// "3 min ago" style age, matching the legacy panel.
pub fn ago(now_ms: u64, t_ms: u64) -> String {
    let s = now_ms.saturating_sub(t_ms) / 1000;
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_accumulates_and_summarises() {
        let mut u = Usage::default();
        u.add("2026-10-15", 100., 10., 1);
        u.add("2026-10-16", 50., 5., 2);
        u.add("2026-10-16", -5., 5., 3);
        u.add("2026-09-30", 1000., 1000., 4);
        let (t, m) = u.summary("2026-10-16");
        assert_eq!(t, Bytes { down: 50., up: 10. });
        assert_eq!(
            m,
            Bytes {
                down: 150.,
                up: 20.
            }
        );
        assert_eq!(u.since, Some(1));
    }
    #[test]
    fn usage_prunes_and_reads_legacy() {
        let legacy = r#"{"days":{"2026-10-13":{"down":1000000,"up":200000},"2026-10-14":{"down":3000000.0,"up":400000.0}},"since":1790000000000}"#;
        let mut u: Usage = serde_json::from_str(legacy).unwrap();
        assert_eq!(u.days.len(), 2);
        for d in 1..=70 {
            u.add(
                &format!("2027-{:02}-{:02}", 1 + d / 28, 1 + d % 28),
                1.,
                1.,
                0,
            );
        }
        assert_eq!(u.days.len(), KEEP_DAYS);
        assert!(!u.days.contains_key("2026-10-13"));
    }
    #[test]
    fn hot_detector_needs_five_and_cools_down() {
        let mut h = HotDetector::default();
        for i in 0..4 {
            assert!(!h.sample(90., 50., 1_000_000 + i));
        }
        assert!(h.sample(90., 50., 1_000_005));
        for i in 0..10 {
            assert!(!h.sample(99., 99., 1_000_010 + i));
        }
        assert!(!h.sample(10., 10., 2_000_000));
        let t = 1_000_005 + 600_001;
        for i in 0..4 {
            assert!(!h.sample(10., 95., t + i));
        }
        assert!(h.sample(10., 95., t + 5));
    }
    #[test]
    fn alerts_are_bounded_newest_first() {
        let mut l = vec![];
        for i in 0..50 {
            push_alert(
                &mut l,
                Alert {
                    id: i,
                    kind: "performance".into(),
                    title: "t".into(),
                    message: "m".into(),
                    time: i,
                },
            );
        }
        assert_eq!(l.len(), MAX_ALERTS);
        assert_eq!(l[0].id, 49);
        assert_eq!(ago(10_000_000, 10_000_000 - 180_000), "3m");
    }
}
