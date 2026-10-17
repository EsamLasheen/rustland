//! Portable product policy. No Windows handles, network hub, or external utilities.
pub mod focus;
pub mod launcher;
pub mod local_data;
pub mod shortcuts;
pub mod usage;
pub mod wm_layout;
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};

pub const APP_NAME: &str = "Solo Leveling Shell";
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Palette {
    #[default]
    Shadow,
    Ocean,
    Forest,
    Violet,
    Amber,
    Rose,
}

impl Palette {
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            Self::Shadow => (98, 186, 248),
            Self::Ocean => (73, 238, 243),
            Self::Forest => (82, 224, 158),
            Self::Violet => (185, 129, 248),
            Self::Amber => (248, 197, 79),
            Self::Rose => (245, 92, 189),
        }
    }
    pub fn next(self) -> Self {
        match self {
            Self::Shadow => Self::Ocean,
            Self::Ocean => Self::Forest,
            Self::Forest => Self::Violet,
            Self::Violet => Self::Amber,
            Self::Amber => Self::Rose,
            Self::Rose => Self::Shadow,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Shadow => "Shadow Monarch",
            Self::Ocean => "Electric cyan",
            Self::Forest => "Jade hunter",
            Self::Violet => "Shadow violet",
            Self::Amber => "Sovereign gold",
            Self::Rose => "Monarch rose",
        }
    }
    pub const ALL: [Self; 6] = [
        Self::Shadow,
        Self::Ocean,
        Self::Forest,
        Self::Violet,
        Self::Amber,
        Self::Rose,
    ];
    pub fn rank(self) -> &'static str {
        match self {
            Self::Shadow => "E",
            Self::Ocean => "D",
            Self::Forest => "C",
            Self::Violet => "B",
            Self::Amber => "A",
            Self::Rose => "S",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub schema: u32,
    pub palette: Palette,
    pub bar_height: u32,
    pub gap: u32,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            palette: Palette::Shadow,
            bar_height: 32,
            gap: 10,
        }
    }
}
impl Settings {
    pub fn validate(&self) -> io::Result<()> {
        if self.schema != SCHEMA {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported settings version; original file preserved",
            ));
        }
        if !(30..=80).contains(&self.bar_height) || self.gap > 40 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Settings values are outside supported limits",
            ));
        }
        Ok(())
    }
    pub fn load(path: &Path) -> io::Result<Self> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        let s: Self = serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        s.validate()?;
        Ok(s)
    }
    pub fn bytes(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        serde_json::to_vec_pretty(self).map_err(io::Error::other)
    }
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub data: PathBuf,
    pub settings: PathBuf,
    pub log: PathBuf,
    pub recovery: PathBuf,
}
impl Paths {
    pub fn under(local: &Path) -> Self {
        let data = local.join("MonarchNext");
        Self {
            settings: data.join("settings.json"),
            log: data.join("session.log"),
            recovery: data.join("window-recovery.json"),
            data,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct BarLayout {
    pub left: Rect,
    pub capsule: Rect,
    pub right: Rect,
}

#[derive(Debug, Clone, Copy)]
pub struct HubLayout {
    pub panel: Rect,
    pub compact: bool,
    pub content_height: i32,
    pub scroll_max: i32,
}
impl HubLayout {
    pub fn new(width: i32, height: i32, monitoring: bool) -> Self {
        let compact = width < 820;
        let content_height = height.max(if compact && monitoring { 780 } else { 650 });
        let panel_width = if compact {
            (width - 96).clamp(160, 330)
        } else {
            330
        };
        Self {
            panel: Rect {
                x: 72,
                y: 54,
                width: panel_width,
                height: content_height - 78,
            },
            compact,
            content_height,
            scroll_max: (content_height - height).max(0),
        }
    }
}
impl BarLayout {
    pub fn new(width: i32) -> Self {
        let w = width.max(360);
        let cap = (w / 3).clamp(120, 312);
        let available = (w - cap) / 2 - 16;
        let left = available.min(510);
        let right = available.min(270);
        Self {
            left: Rect {
                x: 8,
                y: 4,
                width: left,
                height: 24,
            },
            capsule: Rect {
                x: (w - cap) / 2,
                y: 4,
                width: cap,
                height: 24,
            },
            right: Rect {
                x: w - right - 8,
                y: 4,
                width: right,
                height: 24,
            },
        }
    }
}

pub fn cpu_percent(before: (u64, u64), after: (u64, u64)) -> Option<f64> {
    let total = after.0.checked_sub(before.0)?;
    let idle = after.1.checked_sub(before.1)?;
    if total == 0 || idle > total {
        return None;
    }
    Some((total - idle) as f64 / total as f64 * 100.0)
}

/// A main column plus a vertical stack. Returns no layout when useful minimums cannot fit.
pub fn tile(area: Rect, count: usize, gap: u32) -> Vec<Rect> {
    if count == 0 || count > 32 {
        return vec![];
    }
    let g = gap.min(40) as i32;
    let usable = Rect {
        x: area.x + g,
        y: area.y + g,
        width: area.width - 2 * g,
        height: area.height - 2 * g,
    };
    if usable.width < 160 || usable.height < 100 {
        return vec![];
    }
    if count == 1 {
        return vec![usable];
    }
    let main_width = ((usable.width - g) * 3) / 5;
    let stack_width = usable.width - main_width - g;
    let n = (count - 1) as i32;
    let total = usable.height - g * (n - 1);
    if main_width < 160 || stack_width < 160 || total / n < 100 {
        return vec![];
    }
    let mut out = vec![Rect {
        width: main_width,
        ..usable
    }];
    let mut y = usable.y;
    for i in 0..n {
        let height = total / n + i32::from(i < total % n);
        out.push(Rect {
            x: usable.x + main_width + g,
            y,
            width: stack_width,
            height,
        });
        y += height + g;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_handles_negative_monitor_coordinates_without_overlap() {
        let a = Rect {
            x: -1920,
            y: -200,
            width: 1920,
            height: 1080,
        };
        let tiles = tile(a, 5, 10);
        assert_eq!(tiles.len(), 5);
        for (i, r) in tiles.iter().enumerate() {
            assert!(
                r.x >= a.x
                    && r.y >= a.y
                    && r.x + r.width <= a.x + a.width
                    && r.y + r.height <= a.y + a.height
            );
            for other in &tiles[i + 1..] {
                assert!(
                    r.x + r.width <= other.x
                        || other.x + other.width <= r.x
                        || r.y + r.height <= other.y
                        || other.y + other.height <= r.y
                );
            }
        }
        assert_eq!(
            tiles.last().unwrap().y + tiles.last().unwrap().height,
            a.y + a.height - 10
        );
    }
    #[test]
    fn refuses_impossible_layout_instead_of_creating_offscreen_windows() {
        assert!(tile(
            Rect {
                x: 0,
                y: 0,
                width: 320,
                height: 200
            },
            9,
            10
        )
        .is_empty());
        assert!(tile(
            Rect {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080
            },
            0,
            10
        )
        .is_empty());
    }
    #[test]
    fn future_schema_is_rejected_and_partial_settings_use_defaults() {
        let future: Settings = serde_json::from_str(r#"{"schema":2}"#).unwrap();
        assert!(future.validate().is_err());
        let partial: Settings = serde_json::from_str(r#"{"palette":"Ocean"}"#).unwrap();
        assert_eq!(partial.bar_height, 32);
        assert!(partial.validate().is_ok());
    }
    #[test]
    fn compact_bar_segments_never_overlap() {
        for width in [360, 480, 640, 943, 1280, 1920, 3840] {
            let b = BarLayout::new(width);
            assert!(b.left.x + b.left.width + 8 <= b.capsule.x);
            assert!(b.capsule.x + b.capsule.width + 8 <= b.right.x);
            assert!(b.right.x + b.right.width <= width);
        }
    }
    #[test]
    fn cpu_sampling_distinguishes_idle_busy_and_reset_counters() {
        assert_eq!(cpu_percent((100, 60), (200, 120)), Some(40.0));
        assert_eq!(cpu_percent((100, 60), (200, 160)), Some(0.0));
        assert_eq!(cpu_percent((100, 60), (100, 60)), None);
        assert_eq!(cpu_percent((100, 60), (50, 30)), None);
    }
    #[test]
    fn narrow_hub_stays_inside_viewport_and_exposes_overflow_by_scrolling() {
        for width in [360, 446, 463, 640, 820, 943, 1920] {
            for height in [360, 493, 650, 1028] {
                let layout = HubLayout::new(width, height, true);
                assert!(layout.panel.x + layout.panel.width <= width - 24);
                assert!(layout.panel.height > 0);
                assert_eq!(height + layout.scroll_max, layout.content_height);
            }
        }
    }
}
