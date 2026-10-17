//! Obsidian focus-timer state file (a JSON written by a focus-timer plugin), shown in the capsule.
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct FocusState {
    pub in_session: bool,
    pub running: bool,
    pub mode: String,
    /// Session length in ms.
    pub count: f64,
    /// Elapsed ms at `saved_at`.
    pub elapsed: f64,
    pub task: String,
    pub saved_at: f64,
}

impl FocusState {
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice(bytes).ok()
    }
    /// Remaining ms, `None` outside a session (legacy `focusRemaining`).
    pub fn remaining(&self, now_ms: u64) -> Option<u64> {
        if !self.in_session {
            return None;
        }
        let rem = if self.running {
            self.count - self.elapsed - (now_ms as f64 - self.saved_at)
        } else {
            self.count - self.elapsed
        };
        Some(rem.max(0.) as u64)
    }
    pub fn on_break(&self) -> bool {
        self.mode.eq_ignore_ascii_case("BREAK")
    }
}

/// "MM:SS", rounding seconds up like the legacy `mmss`.
pub fn mmss(ms: u64) -> String {
    let s = ms.div_ceil(1000);
    format!("{:02}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_plugin_file_and_counts_down() {
        let f = FocusState::parse(br#"{"inSession":true,"running":true,"mode":"FOCUS","duration":25,"count":1500000,"elapsed":60000,"startTime":null,"task":"Write","taskData":null,"savedAt":1000}"#).unwrap();
        assert_eq!(f.remaining(1000), Some(1_440_000));
        assert_eq!(f.remaining(61_000), Some(1_380_000));
        assert_eq!(f.remaining(10_000_000), Some(0));
        assert!(!f.on_break());
        let idle = FocusState::parse(br#"{"inSession":false,"running":false,"mode":"BREAK","count":600000,"elapsed":0,"savedAt":5}"#).unwrap();
        assert_eq!(idle.remaining(99), None);
        let paused = FocusState {
            in_session: true,
            count: 600_000.,
            elapsed: 100_000.,
            ..Default::default()
        };
        assert_eq!(paused.remaining(u64::MAX / 2), Some(500_000));
        assert_eq!(mmss(1_440_000), "24:00");
        assert_eq!(mmss(59_001), "01:00");
        assert!(FocusState::parse(b"not json").is_none());
    }
}
