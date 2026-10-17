//! Pure tiling window-manager model reproducing the legacy komorebi configuration.
//!
//! Deterministic BSP tree, per-monitor workspaces ("1".."10" plus the special "S"),
//! window rules, window classification and the journal of windows hidden by the manager.
//! Nothing here calls Windows: every operation appends [`Effect`]s which the desktop crate
//! executes. Window ids are opaque `u64` values (HWNDs on Windows).
use crate::Rect;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, io, time::Duration};

pub type WinId = u64;
/// Ten numbered workspaces plus the special workspace.
pub const WORKSPACES: usize = 11;
/// Index of the special workspace "S".
pub const SPECIAL: usize = 10;
pub const NAMES: [&str; WORKSPACES] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10", "S"];
/// Rule-placed new windows are not followed during this period after start.
pub const FOLLOW_GRACE: Duration = Duration::from_secs(90);
/// Upper bound on managed windows; further windows are left alone.
pub const MAX_WINDOWS: usize = 512;
/// Upper bound on journal entries (also bounds the on-disk file).
pub const MAX_JOURNAL: usize = 1024;
const MAX_JOURNAL_BYTES: usize = 128 * 1024;
const MIN_RATIO: f32 = 0.1;
const MAX_RATIO: f32 = 0.9;
const MINIMIZED_HISTORY: usize = 32;
/// Ratio adjustment passes used to give windows their minimum size.
const MIN_FIT_PASSES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}
impl Dir {
    pub const ALL: [Self; 4] = [Self::Left, Self::Right, Self::Up, Self::Down];
    /// Same numbering as `shortcuts::Action::Focus/Move`: 0 left, 1 right, 2 up, 3 down.
    pub fn from_index(i: u8) -> Option<Self> {
        Self::ALL.get(i as usize).copied()
    }
    pub fn index(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
            Self::Up => 2,
            Self::Down => 3,
        }
    }
    fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Vertical,
            Self::Up | Self::Down => Axis::Horizontal,
        }
    }
    /// The edge lies between child `a` and child `b` when moving right/down from `a`.
    fn toward_b(self) -> bool {
        matches!(self, Self::Right | Self::Down)
    }
}

/// Split orientation. `Vertical` places children side by side (a vertical divider),
/// `Horizontal` stacks them (a horizontal divider).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}
impl Axis {
    fn flipped(self) -> Self {
        match self {
            Self::Vertical => Self::Horizontal,
            Self::Horizontal => Self::Vertical,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Leaf(WinId),
    Split {
        axis: Axis,
        ratio: f32,
        a: usize,
        b: usize,
    },
    Free,
}
#[derive(Clone, Copy, Debug)]
struct Node {
    parent: Option<usize>,
    kind: Kind,
}

/// Arena-backed binary space partition. Inserting splits a target leaf; the split
/// orientation alternates with depth starting vertical (right, below, right, ...).
#[derive(Clone, Debug, Default)]
pub struct Bsp {
    nodes: Vec<Node>,
    root: Option<usize>,
    free: Vec<usize>,
}

fn inset(r: Rect, p: i32) -> Rect {
    Rect {
        x: r.x + p,
        y: r.y + p,
        width: (r.width - 2 * p).max(1),
        height: (r.height - 2 * p).max(1),
    }
}
fn split_rect(r: Rect, axis: Axis, ratio: f32) -> (Rect, Rect) {
    match axis {
        Axis::Vertical => {
            let wa = (r.width as f32 * ratio).round() as i32;
            (
                Rect { width: wa, ..r },
                Rect {
                    x: r.x + wa,
                    width: r.width - wa,
                    ..r
                },
            )
        }
        Axis::Horizontal => {
            let ha = (r.height as f32 * ratio).round() as i32;
            (
                Rect { height: ha, ..r },
                Rect {
                    y: r.y + ha,
                    height: r.height - ha,
                    ..r
                },
            )
        }
    }
}
fn contains(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height
}
/// Grow `r` to the minimum size `min` (0 = none, capped at `area`) and shift it left/up
/// so its right/bottom edges stay inside `area` instead of overflowing the screen.
pub fn fit_min(r: Rect, min: (i32, i32), area: Rect) -> Rect {
    let width = r.width.max(min.0.min(area.width));
    let height = r.height.max(min.1.min(area.height));
    Rect {
        x: r.x.min(area.x + area.width - width).max(area.x),
        y: r.y.min(area.y + area.height - height).max(area.y),
        width,
        height,
    }
}

impl Bsp {
    pub fn new() -> Self {
        Self::default()
    }
    fn alloc(&mut self, node: Node) -> usize {
        if let Some(i) = self.free.pop() {
            self.nodes[i] = node;
            i
        } else {
            self.nodes.push(node);
            self.nodes.len() - 1
        }
    }
    fn release(&mut self, i: usize) {
        self.nodes[i] = Node {
            parent: None,
            kind: Kind::Free,
        };
        self.free.push(i);
    }
    fn leaf(&self, id: WinId) -> Option<usize> {
        self.nodes
            .iter()
            .position(|n| matches!(n.kind, Kind::Leaf(x) if x == id))
    }
    fn depth(&self, mut i: usize) -> usize {
        let mut d = 0;
        while let Some(p) = self.nodes[i].parent {
            d += 1;
            i = p;
        }
        d
    }
    fn last_leaf(&self) -> Option<usize> {
        let mut i = self.root?;
        while let Kind::Split { b, .. } = self.nodes[i].kind {
            i = b;
        }
        Some(i)
    }
    pub fn is_empty(&self) -> bool {
        self.root.is_none()
    }
    pub fn len(&self) -> usize {
        self.nodes
            .iter()
            .filter(|n| matches!(n.kind, Kind::Leaf(_)))
            .count()
    }
    /// Arena slots in use, including free ones (for memory-bound tests).
    pub fn capacity_used(&self) -> usize {
        self.nodes.len()
    }
    pub fn contains(&self, id: WinId) -> bool {
        self.leaf(id).is_some()
    }
    /// The bottom/right-most window.
    pub fn last(&self) -> Option<WinId> {
        match self.nodes[self.last_leaf()?].kind {
            Kind::Leaf(id) => Some(id),
            _ => None,
        }
    }
    /// Windows in layout order (a before b, depth first).
    pub fn leaves(&self) -> Vec<WinId> {
        let mut out = Vec::new();
        let mut stack: Vec<usize> = self.root.into_iter().collect();
        while let Some(i) = stack.pop() {
            match self.nodes[i].kind {
                Kind::Leaf(id) => out.push(id),
                Kind::Split { a, b, .. } => {
                    stack.push(b);
                    stack.push(a);
                }
                Kind::Free => {}
            }
        }
        out
    }
    /// Split the leaf holding `at` (or the last leaf) and put `id` in the new second half.
    pub fn insert(&mut self, id: WinId, at: Option<WinId>) -> bool {
        if self.contains(id) {
            return false;
        }
        let Some(root) = self.root else {
            let i = self.alloc(Node {
                parent: None,
                kind: Kind::Leaf(id),
            });
            self.root = Some(i);
            return true;
        };
        let target = at
            .and_then(|w| self.leaf(w))
            .or_else(|| self.last_leaf())
            .unwrap_or(root);
        let Kind::Leaf(old) = self.nodes[target].kind else {
            return false;
        };
        let axis = if self.depth(target).is_multiple_of(2) {
            Axis::Vertical
        } else {
            Axis::Horizontal
        };
        let a = self.alloc(Node {
            parent: Some(target),
            kind: Kind::Leaf(old),
        });
        let b = self.alloc(Node {
            parent: Some(target),
            kind: Kind::Leaf(id),
        });
        self.nodes[target].kind = Kind::Split {
            axis,
            ratio: 0.5,
            a,
            b,
        };
        true
    }
    /// Remove a window; its sibling takes over the parent's space.
    pub fn remove(&mut self, id: WinId) -> bool {
        let Some(i) = self.leaf(id) else {
            return false;
        };
        match self.nodes[i].parent {
            None => {
                self.release(i);
                self.root = None;
            }
            Some(p) => {
                let Kind::Split { a, b, .. } = self.nodes[p].kind else {
                    return false;
                };
                let s = if a == i { b } else { a };
                let kind = self.nodes[s].kind;
                if let Kind::Split { a: ca, b: cb, .. } = kind {
                    self.nodes[ca].parent = Some(p);
                    self.nodes[cb].parent = Some(p);
                }
                self.nodes[p].kind = kind;
                self.release(i);
                self.release(s);
            }
        }
        if self.root.is_none() {
            self.nodes.clear();
            self.free.clear();
        }
        true
    }
    pub fn swap(&mut self, x: WinId, y: WinId) -> bool {
        match (self.leaf(x), self.leaf(y)) {
            (Some(i), Some(j)) if i != j => {
                self.nodes[i].kind = Kind::Leaf(y);
                self.nodes[j].kind = Kind::Leaf(x);
                true
            }
            _ => false,
        }
    }
    /// Toggle the orientation of the split containing `id`.
    pub fn flip(&mut self, id: WinId) -> bool {
        let Some(p) = self.leaf(id).and_then(|i| self.nodes[i].parent) else {
            return false;
        };
        if let Kind::Split { axis, .. } = &mut self.nodes[p].kind {
            *axis = axis.flipped();
            return true;
        }
        false
    }
    /// Nearest ancestor split whose divider is the `dir` edge of `id`; returns the split
    /// node and whether `id` lives in its first child.
    fn edge_owner(&self, id: WinId, dir: Dir) -> Option<(usize, bool)> {
        let mut c = self.leaf(id)?;
        while let Some(p) = self.nodes[c].parent {
            if let Kind::Split { axis, a, .. } = self.nodes[p].kind {
                if axis == dir.axis() && (a == c) == dir.toward_b() {
                    return Some((p, a == c));
                }
            }
            c = p;
        }
        None
    }
    fn adjust(&mut self, p: usize, in_a: bool, delta: f32) {
        if let Kind::Split { ratio, .. } = &mut self.nodes[p].kind {
            let r = if in_a { *ratio + delta } else { *ratio - delta };
            *ratio = r.clamp(MIN_RATIO, MAX_RATIO);
        }
    }
    /// Move the `dir` edge of `id` outward by `delta` (a ratio of the split; negative
    /// shrinks). Returns false when that edge is a workspace border.
    pub fn grow(&mut self, id: WinId, dir: Dir, delta: f32) -> bool {
        match self.edge_owner(id, dir) {
            Some((p, in_a)) => {
                self.adjust(p, in_a, delta);
                true
            }
            None => false,
        }
    }
    /// Like [`Bsp::grow`] with a pixel amount, converted using the split's size in `area`.
    pub fn grow_px(&mut self, id: WinId, dir: Dir, px: i32, area: Rect) -> bool {
        let Some((p, in_a)) = self.edge_owner(id, dir) else {
            return false;
        };
        let rects = self.node_rects(area);
        let size = match dir.axis() {
            Axis::Vertical => rects[p].width,
            Axis::Horizontal => rects[p].height,
        };
        self.adjust(p, in_a, px as f32 / size.max(1) as f32);
        true
    }
    pub fn ratio_of_parent(&self, id: WinId) -> Option<f32> {
        let p = self.nodes[self.leaf(id)?].parent?;
        match self.nodes[p].kind {
            Kind::Split { ratio, .. } => Some(ratio),
            _ => None,
        }
    }
    pub fn axis_of_parent(&self, id: WinId) -> Option<Axis> {
        let p = self.nodes[self.leaf(id)?].parent?;
        match self.nodes[p].kind {
            Kind::Split { axis, .. } => Some(axis),
            _ => None,
        }
    }
    fn node_rects(&self, area: Rect) -> Vec<Rect> {
        let mut rects = vec![
            Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0
            };
            self.nodes.len()
        ];
        let mut stack: Vec<(usize, Rect)> = self.root.map(|r| (r, area)).into_iter().collect();
        while let Some((i, r)) = stack.pop() {
            rects[i] = r;
            if let Kind::Split { axis, ratio, a, b } = self.nodes[i].kind {
                let (ra, rb) = split_rect(r, axis, ratio);
                stack.push((b, rb));
                stack.push((a, ra));
            }
        }
        rects
    }
    /// Window rectangles for `area`, each inset by `pad` on every side.
    pub fn layout(&self, area: Rect, pad: i32, out: &mut Vec<(WinId, Rect)>) {
        out.clear();
        let mut stack: Vec<(usize, Rect)> = self.root.map(|r| (r, area)).into_iter().collect();
        while let Some((i, r)) = stack.pop() {
            match self.nodes[i].kind {
                Kind::Leaf(id) => out.push((id, inset(r, pad))),
                Kind::Split { axis, ratio, a, b } => {
                    let (ra, rb) = split_rect(r, axis, ratio);
                    stack.push((b, rb));
                    stack.push((a, ra));
                }
                Kind::Free => {}
            }
        }
    }
}

/// Geometric neighbour of `from` in `dir`: the closest rectangle entirely on that side
/// that overlaps it on the perpendicular axis (ties: larger overlap, then top/left-most).
pub fn neighbour(rects: &[(WinId, Rect)], from: WinId, dir: Dir) -> Option<WinId> {
    let f = rects.iter().find(|(w, _)| *w == from)?.1;
    let overlap = |a0: i32, al: i32, b0: i32, bl: i32| (a0 + al).min(b0 + bl) - a0.max(b0);
    let mut best: Option<((i32, i32, i32), WinId)> = None;
    for &(w, r) in rects {
        if w == from {
            continue;
        }
        let (gap, over, cross) = match dir {
            Dir::Left => (
                f.x - (r.x + r.width),
                overlap(f.y, f.height, r.y, r.height),
                r.y,
            ),
            Dir::Right => (
                r.x - (f.x + f.width),
                overlap(f.y, f.height, r.y, r.height),
                r.y,
            ),
            Dir::Up => (
                f.y - (r.y + r.height),
                overlap(f.x, f.width, r.x, r.width),
                r.x,
            ),
            Dir::Down => (
                r.y - (f.y + f.height),
                overlap(f.x, f.width, r.x, r.width),
                r.x,
            ),
        };
        if gap < 0 || over <= 0 {
            continue;
        }
        let key = (gap, -over, cross);
        if best.is_none_or(|(k, _)| key < k) {
            best = Some((key, w));
        }
    }
    best.map(|(_, w)| w)
}

// ---------------------------------------------------------------------------------------
// Rules and classification
// ---------------------------------------------------------------------------------------

const IGNORE_CLASSES: &[&str] = &[
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "IME",
    "MSCTFIME UI",
    "CicMarshalWndClass",
    "Windows.UI.Core.CoreWindow",
];
const IGNORE_EXES: &[&str] = &["GoogleDriveFS"];
const IGNORE_TITLE_SUFFIXES: &[&str] = &[
    "WindowsPowerShell\\v1.0\\powershell.exe",
    "\\system32\\cmd.exe",
];
const FLOAT_EXES: &[&str] = &["windhawk"];
const FLOAT_TITLES: &[&str] = &["Picture-in-Picture"];
const FORCE_MANAGE: &[&str] = &["Obsidian", "WhatsApp.Root"];
/// Standard Win32 dialog class; always floats.
const DIALOG_CLASS: &str = "#32770";
/// Minimum visual size of a window worth managing.
pub const MIN_SIZE: i32 = 64;
/// (exe without extension, zero-based workspace index).
const EXE_RULES: &[(&str, usize)] = &[
    ("brave", 1),
    ("chrome", 1),
    ("msedge", 1),
    ("firefox", 1),
    ("opera", 1),
    ("opera_gx", 1),
    ("Code", 2),
    ("Cursor", 2),
    ("devenv", 2),
    ("idea64", 2),
    ("pycharm64", 2),
    ("dbeaver", 4),
    ("studio64", 4),
    ("WhatsApp.Root", 5),
    ("Telegram", 5),
    ("Discord", 5),
    ("thunderbird", 5),
    ("Spotify", 6),
    ("vlc", 6),
    ("mpv", 6),
    ("Music.UI", 6),
    ("steam", 7),
    ("steamwebhelper", 7),
    ("EpicGamesLauncher", 7),
    ("retroarch", 7),
];
const TITLE_RULES: &[(&str, usize)] = &[
    ("Burp Suite Professional", 4),
    ("Burp Suite Community Edition", 4),
    ("LibreOffice", 4),
];

fn strip_exe(exe: &str) -> &str {
    let n = exe.len();
    if n >= 4 && exe.is_char_boundary(n - 4) && exe[n - 4..].eq_ignore_ascii_case(".exe") {
        &exe[..n - 4]
    } else {
        exe
    }
}
/// Case-insensitive executable comparison, with or without ".exe".
pub fn exe_is(exe: &str, name: &str) -> bool {
    strip_exe(exe).eq_ignore_ascii_case(strip_exe(name))
}
fn contains_ci(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}
fn ends_with_ci(hay: &str, suffix: &str) -> bool {
    let (h, s) = (hay.as_bytes(), suffix.as_bytes());
    h.len() >= s.len() && h[h.len() - s.len()..].eq_ignore_ascii_case(s)
}

/// Initial workspace (zero-based) for a newly managed window, if a rule applies.
pub fn rule_workspace(exe: &str, title: &str) -> Option<usize> {
    EXE_RULES
        .iter()
        .find(|(name, _)| exe_is(exe, name))
        .or_else(|| TITLE_RULES.iter().find(|(t, _)| contains_ci(title, t)))
        .map(|(_, ws)| *ws)
}

/// Whether a rule-placed new window should be followed.
pub fn should_follow(since_start: Duration, grace: Duration) -> bool {
    since_start >= grace
}

/// Everything the classifier needs about a top-level window.
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowFacts<'a> {
    pub exe: &'a str,
    pub title: &'a str,
    pub class: &'a str,
    pub own_process: bool,
    pub visible: bool,
    pub cloaked: bool,
    pub child: bool,
    pub tool_window: bool,
    pub no_activate: bool,
    pub app_window: bool,
    pub has_caption: bool,
    pub owned: bool,
    pub dialog_frame: bool,
    /// Has a sizing border (WS_THICKFRAME); fixed-size windows float.
    pub resizable: bool,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Ignore,
    Float,
    Tile,
}

pub fn classify(f: &WindowFacts) -> Disposition {
    if f.own_process || !f.visible || f.cloaked || f.child {
        return Disposition::Ignore;
    }
    if IGNORE_CLASSES.contains(&f.class)
        || IGNORE_EXES.iter().any(|e| exe_is(f.exe, e))
        || f.title.trim().is_empty()
        || IGNORE_TITLE_SUFFIXES
            .iter()
            .any(|s| ends_with_ci(f.title, s))
        || f.tool_window
        || f.no_activate
        || f.width < MIN_SIZE
        || f.height < MIN_SIZE
    {
        return Disposition::Ignore;
    }
    if FLOAT_EXES.iter().any(|e| exe_is(f.exe, e))
        || FLOAT_TITLES.iter().any(|t| contains_ci(f.title, t))
    {
        return Disposition::Float;
    }
    let forced = FORCE_MANAGE.iter().any(|e| exe_is(f.exe, e));
    if !f.has_caption && !forced {
        return Disposition::Ignore;
    }
    if (f.owned && !f.app_window)
        || f.dialog_frame
        || f.class == DIALOG_CLASS
        || (!f.resizable && !forced)
    {
        return Disposition::Float;
    }
    Disposition::Tile
}

// ---------------------------------------------------------------------------------------
// Journal of windows hidden by the manager
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub hwnd: u64,
    pub pid: u32,
}
#[derive(Serialize, Deserialize)]
struct JournalFile {
    schema: u32,
    hidden: Vec<JournalEntry>,
}

/// Every window the manager hid. Restoring all entries undoes the manager's hiding.
#[derive(Clone, Debug, Default)]
pub struct Journal {
    entries: Vec<JournalEntry>,
    dirty: bool,
}
impl Journal {
    pub fn insert(&mut self, hwnd: u64, pid: u32) -> bool {
        if self.contains(hwnd) || self.entries.len() >= MAX_JOURNAL {
            return false;
        }
        self.entries.push(JournalEntry { hwnd, pid });
        self.dirty = true;
        true
    }
    pub fn remove(&mut self, hwnd: u64) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.hwnd != hwnd);
        let removed = self.entries.len() != before;
        self.dirty |= removed;
        removed
    }
    pub fn contains(&self, hwnd: u64) -> bool {
        self.entries.iter().any(|e| e.hwnd == hwnd)
    }
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// True once after each change; used to persist only when needed.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }
    pub fn drain(&mut self) -> Vec<JournalEntry> {
        self.dirty |= !self.entries.is_empty();
        std::mem::take(&mut self.entries)
    }
    pub fn to_bytes(&self) -> io::Result<Vec<u8>> {
        serde_json::to_vec(&JournalFile {
            schema: 1,
            hidden: self.entries.clone(),
        })
        .map_err(io::Error::other)
    }
    pub fn parse(bytes: &[u8]) -> io::Result<Vec<JournalEntry>> {
        if bytes.len() > MAX_JOURNAL_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Window journal exceeds limit",
            ));
        }
        let f: JournalFile = serde_json::from_slice(bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if f.schema != 1 || f.hidden.len() > MAX_JOURNAL {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unsupported window journal",
            ));
        }
        Ok(f.hidden)
    }
}

// ---------------------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------------------

/// Window-manager actions. Encodable into a `usize` so they can travel in a WPARAM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WmAction {
    Focus(Dir),
    Move(Dir),
    /// `vertical == false` is komorebi's `resize-axis horizontal`.
    ResizeAxis {
        vertical: bool,
        grow: bool,
    },
    Close,
    ToggleMaximize,
    ToggleFloat,
    Flip,
    Minimize,
    RestoreLast,
    /// Zero-based; 10 is the special workspace.
    FocusWorkspace(u8),
    SendToWorkspace(u8),
    FocusLastWorkspace,
    ToggleSpecial,
    CycleWorkspace {
        next: bool,
    },
    Retile,
}
impl WmAction {
    pub fn encode(self) -> usize {
        match self {
            Self::Focus(d) => 0x100 | d.index() as usize,
            Self::Move(d) => 0x200 | d.index() as usize,
            Self::ResizeAxis { vertical, grow } => 0x300 | (vertical as usize) << 1 | grow as usize,
            Self::Close => 0x400,
            Self::ToggleMaximize => 0x401,
            Self::ToggleFloat => 0x402,
            Self::Flip => 0x403,
            Self::Minimize => 0x404,
            Self::RestoreLast => 0x405,
            Self::FocusLastWorkspace => 0x406,
            Self::ToggleSpecial => 0x407,
            Self::CycleWorkspace { next: true } => 0x408,
            Self::CycleWorkspace { next: false } => 0x409,
            Self::Retile => 0x40a,
            Self::FocusWorkspace(n) => 0x500 | n as usize,
            Self::SendToWorkspace(n) => 0x600 | n as usize,
        }
    }
    pub fn decode(v: usize) -> Option<Self> {
        let low = (v & 0xff) as u8;
        Some(match v >> 8 {
            1 if low < 4 => Self::Focus(Dir::from_index(low)?),
            2 if low < 4 => Self::Move(Dir::from_index(low)?),
            3 if low < 4 => Self::ResizeAxis {
                vertical: low & 2 != 0,
                grow: low & 1 != 0,
            },
            4 => match low {
                0 => Self::Close,
                1 => Self::ToggleMaximize,
                2 => Self::ToggleFloat,
                3 => Self::Flip,
                4 => Self::Minimize,
                5 => Self::RestoreLast,
                6 => Self::FocusLastWorkspace,
                7 => Self::ToggleSpecial,
                8 => Self::CycleWorkspace { next: true },
                9 => Self::CycleWorkspace { next: false },
                10 => Self::Retile,
                _ => return None,
            },
            5 if (low as usize) < WORKSPACES => Self::FocusWorkspace(low),
            6 if (low as usize) < WORKSPACES => Self::SendToWorkspace(low),
            _ => return None,
        })
    }
    /// Map the existing shortcut actions onto tiling semantics.
    pub fn from_shortcut(a: &crate::shortcuts::Action) -> Option<Self> {
        use crate::shortcuts::Action as A;
        Some(match *a {
            A::Close => Self::Close,
            A::Maximize => Self::ToggleMaximize,
            A::Minimize => Self::Minimize,
            A::Restore => Self::RestoreLast,
            A::Focus(d) => Self::Focus(Dir::from_index(d)?),
            A::Move(d) => Self::Move(Dir::from_index(d)?),
            A::Resize(delta) => Self::ResizeAxis {
                vertical: false,
                grow: delta > 0,
            },
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------------------
// Workspaces and the model
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Move/resize so the window's VISUAL bounds equal the rect.
    Place(WinId, Rect),
    /// Hide because its workspace became invisible (journaled).
    Hide(WinId),
    /// Show after its workspace became visible again (removed from the journal).
    Show(WinId),
    Focus(WinId),
    Close(WinId),
    ToggleMaximize(WinId),
    /// User-requested minimize (not a workspace hide).
    Minimize(WinId),
    /// Restore a user-minimized window.
    Restore(WinId),
    /// Workspace/bar state changed.
    BarChanged,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Config {
    pub workspace_padding: i32,
    pub container_padding: i32,
    pub resize_step: f32,
    pub follow_grace: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            workspace_padding: 5,
            container_padding: 5,
            resize_step: 0.05,
            follow_grace: FOLLOW_GRACE,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub tree: Bsp,
    /// All windows of the workspace (tiled, floating, minimized) in arrival order.
    pub members: Vec<WinId>,
    pub focused: Option<WinId>,
}

#[derive(Clone, Debug)]
pub struct Monitor {
    pub work: Rect,
    pub workspaces: Vec<Workspace>,
    pub active: usize,
    pub last: Option<usize>,
    pub special_return: Option<usize>,
}
impl Monitor {
    fn new(work: Rect) -> Self {
        Self {
            work,
            workspaces: (0..WORKSPACES).map(|_| Workspace::default()).collect(),
            active: 0,
            last: None,
            special_return: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Managed {
    pub pid: u32,
    pub exe: String,
    pub monitor: usize,
    pub workspace: usize,
    pub floating: bool,
    pub minimized: bool,
    pub maximized: bool,
    /// Smallest visual size the window accepts (0 = unknown), learned from placements.
    pub min_size: (i32, i32),
}

#[derive(Clone, Copy, Debug)]
pub struct NewWindow<'a> {
    pub id: WinId,
    pub pid: u32,
    pub exe: &'a str,
    pub title: &'a str,
    pub monitor: usize,
    pub floating: bool,
    /// Explicit workspace (e.g. the owner's for an owned dialog). Takes precedence over
    /// workspace rules; the window is never hidden and its workspace becomes active.
    pub workspace: Option<usize>,
}

/// What the bar shows for one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BarWorkspace {
    pub name: &'static str,
    pub focused: bool,
    /// (window id, executable file name) in arrival order.
    pub windows: Vec<(WinId, String)>,
}

#[derive(Clone, Debug)]
pub struct Model {
    config: Config,
    monitors: Vec<Monitor>,
    windows: HashMap<WinId, Managed>,
    focused_monitor: usize,
    journal: Journal,
    minimized: Vec<WinId>,
    scratch: Vec<(WinId, Rect)>,
}

const EMPTY: Rect = Rect {
    x: 0,
    y: 0,
    width: 1920,
    height: 1080,
};

impl Model {
    /// `works` are monitor work areas; at least one monitor is always present.
    pub fn new(config: Config, works: &[Rect]) -> Self {
        let mut monitors: Vec<Monitor> = works.iter().map(|w| Monitor::new(*w)).collect();
        if monitors.is_empty() {
            monitors.push(Monitor::new(EMPTY));
        }
        Self {
            config,
            monitors,
            windows: HashMap::new(),
            focused_monitor: 0,
            journal: Journal::default(),
            minimized: Vec::new(),
            scratch: Vec::new(),
        }
    }
    pub fn config(&self) -> &Config {
        &self.config
    }
    pub fn journal(&self) -> &Journal {
        &self.journal
    }
    pub fn journal_mut(&mut self) -> &mut Journal {
        &mut self.journal
    }
    pub fn monitors(&self) -> &[Monitor] {
        &self.monitors
    }
    pub fn focused_monitor(&self) -> usize {
        self.focused_monitor
    }
    pub fn window(&self, id: WinId) -> Option<&Managed> {
        self.windows.get(&id)
    }
    pub fn len(&self) -> usize {
        self.windows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
    pub fn active_workspace(&self, mon: usize) -> Option<usize> {
        self.monitors.get(mon).map(|m| m.active)
    }
    pub fn focused_window(&self) -> Option<WinId> {
        let m = &self.monitors[self.focused_monitor];
        m.workspaces[m.active].focused
    }
    pub fn is_tiled(&self, id: WinId) -> bool {
        self.windows
            .get(&id)
            .is_some_and(|w| !w.floating && !w.minimized)
    }
    /// Managed, not minimized and on its monitor's visible workspace.
    pub fn is_visible(&self, id: WinId) -> bool {
        self.windows
            .get(&id)
            .is_some_and(|w| !w.minimized && self.monitors[w.monitor].active == w.workspace)
    }
    fn area(&self, mon: usize) -> Rect {
        inset(self.monitors[mon].work, self.config.workspace_padding)
    }
    /// Layout of the given workspace (window rects including container padding).
    pub fn layout_of(&self, mon: usize, ws: usize) -> Vec<(WinId, Rect)> {
        let mut out = Vec::new();
        if let Some(space) = self.monitors.get(mon).and_then(|m| m.workspaces.get(ws)) {
            space
                .tree
                .layout(self.area(mon), self.config.container_padding, &mut out);
        }
        out
    }
    pub fn tiled_rect(&self, id: WinId) -> Option<Rect> {
        let w = self.windows.get(&id)?;
        self.layout_of(w.monitor, w.workspace)
            .into_iter()
            .find(|(x, _)| *x == id)
            .map(|(_, r)| r)
    }
    pub fn monitor_at(&self, x: i32, y: i32) -> Option<usize> {
        self.monitors.iter().position(|m| contains(m.work, x, y))
    }
    /// Visible tiled window whose tile (including its share of the gap) contains the point.
    pub fn tiled_at(&self, x: i32, y: i32) -> Option<WinId> {
        let mon = self.monitor_at(x, y)?;
        let pad = self.config.container_padding;
        self.layout_of(mon, self.monitors[mon].active)
            .into_iter()
            .find(|(_, r)| contains(inset(*r, -pad), x, y))
            .map(|(id, _)| id)
    }
    pub fn bar(&self, mon: usize) -> Vec<BarWorkspace> {
        let Some(m) = self.monitors.get(mon) else {
            return Vec::new();
        };
        m.workspaces
            .iter()
            .enumerate()
            .map(|(i, space)| BarWorkspace {
                name: NAMES[i],
                focused: i == m.active,
                windows: space
                    .members
                    .iter()
                    .filter_map(|id| self.windows.get(id).map(|w| (*id, w.exe.clone())))
                    .collect(),
            })
            .collect()
    }

    fn hide(&mut self, id: WinId, out: &mut Vec<Effect>) {
        if let Some(w) = self.windows.get(&id) {
            if !w.minimized && self.journal.insert(id, w.pid) {
                out.push(Effect::Hide(id));
            }
        }
    }
    fn show(&mut self, id: WinId, out: &mut Vec<Effect>) {
        if self.journal.remove(id) {
            out.push(Effect::Show(id));
        }
    }
    fn pick_focus(&self, mon: usize, ws: usize) -> Option<WinId> {
        let space = &self.monitors[mon].workspaces[ws];
        space.tree.last().or_else(|| {
            space
                .members
                .iter()
                .rev()
                .find(|id| self.windows.get(id).is_some_and(|w| !w.minimized))
                .copied()
        })
    }
    fn detach(&mut self, id: WinId) -> Option<(usize, usize)> {
        let w = self.windows.get(&id)?;
        let (mon, ws) = (w.monitor, w.workspace);
        let space = &mut self.monitors[mon].workspaces[ws];
        space.tree.remove(id);
        space.members.retain(|x| *x != id);
        if space.focused == Some(id) {
            space.focused = None;
            let f = self.pick_focus(mon, ws);
            self.monitors[mon].workspaces[ws].focused = f;
        }
        Some((mon, ws))
    }
    fn attach(&mut self, id: WinId, mon: usize, ws: usize, at: Option<WinId>) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        w.monitor = mon;
        w.workspace = ws;
        let tile = !w.floating && !w.minimized;
        let space = &mut self.monitors[mon].workspaces[ws];
        if !space.members.contains(&id) {
            space.members.push(id);
        }
        if tile {
            let at = at.or(space.focused).filter(|f| space.tree.contains(*f));
            space.tree.insert(id, at);
        }
        space.focused = Some(id);
    }

    pub fn retile_monitor(&mut self, mon: usize, out: &mut Vec<Effect>) {
        if mon >= self.monitors.len() {
            return;
        }
        let ws = self.monitors[mon].active;
        self.fit_min_sizes(mon, ws);
        let mut scratch = std::mem::take(&mut self.scratch);
        let area = self.area(mon);
        self.monitors[mon].workspaces[ws].tree.layout(
            area,
            self.config.container_padding,
            &mut scratch,
        );
        for &(id, r) in &scratch {
            if let Some(w) = self.windows.get(&id).filter(|w| !w.maximized) {
                out.push(Effect::Place(id, fit_min(r, w.min_size, area)));
            }
        }
        self.scratch = scratch;
    }

    /// Move split dividers so windows with a known minimum size get at least that much
    /// space, taking it from their siblings. Bounded; [`fit_min`] handles any remainder.
    fn fit_min_sizes(&mut self, mon: usize, ws: usize) {
        let has_min = |w: &Managed| !w.maximized && w.min_size != (0, 0);
        let space = &self.monitors[mon].workspaces[ws];
        if !space
            .members
            .iter()
            .any(|id| self.windows.get(id).is_some_and(has_min))
        {
            return;
        }
        let area = self.area(mon);
        let pad = self.config.container_padding;
        let mut rects = Vec::new();
        for _ in 0..MIN_FIT_PASSES {
            self.monitors[mon].workspaces[ws]
                .tree
                .layout(area, pad, &mut rects);
            let mut changed = false;
            for &(id, r) in &rects {
                let Some(min) = self
                    .windows
                    .get(&id)
                    .filter(|w| has_min(w))
                    .map(|w| w.min_size)
                else {
                    continue;
                };
                let tree = &mut self.monitors[mon].workspaces[ws].tree;
                let (dw, dh) = (
                    min.0.min(area.width) - r.width,
                    min.1.min(area.height) - r.height,
                );
                if dw > 0 {
                    changed |= tree.grow_px(id, Dir::Right, dw, area)
                        || tree.grow_px(id, Dir::Left, dw, area);
                }
                if dh > 0 {
                    changed |= tree.grow_px(id, Dir::Down, dh, area)
                        || tree.grow_px(id, Dir::Up, dh, area);
                }
            }
            if !changed {
                break;
            }
        }
    }

    /// The window refused a placement and is at least `width`×`height` (visual px; 0 =
    /// no constraint on that axis). Remembers it and re-lays out the monitor so the window
    /// gets that space. Returns false when the known minimum did not grow.
    pub fn set_min_size(
        &mut self,
        id: WinId,
        width: i32,
        height: i32,
        out: &mut Vec<Effect>,
    ) -> bool {
        let Some(w) = self.windows.get_mut(&id) else {
            return false;
        };
        let min = (w.min_size.0.max(width), w.min_size.1.max(height));
        if min == w.min_size {
            return false;
        }
        w.min_size = min;
        let mon = w.monitor;
        if self.is_visible(id) && self.is_tiled(id) {
            self.retile_monitor(mon, out);
        }
        true
    }
    pub fn retile_all(&mut self, out: &mut Vec<Effect>) {
        for mon in 0..self.monitors.len() {
            self.retile_monitor(mon, out);
        }
    }

    /// Replace monitor work areas. Windows of removed monitors move to monitor 0.
    pub fn set_monitors(&mut self, works: &[Rect], out: &mut Vec<Effect>) {
        if works.is_empty() {
            return;
        }
        for (i, r) in works.iter().enumerate() {
            match self.monitors.get_mut(i) {
                Some(m) => m.work = *r,
                None => self.monitors.push(Monitor::new(*r)),
            }
        }
        if works.len() < self.monitors.len() {
            let removed = self.monitors.split_off(works.len());
            for m in removed {
                for (ws, space) in m.workspaces.into_iter().enumerate() {
                    for id in space.members {
                        let at = self.monitors[0].workspaces[ws].focused;
                        self.attach(id, 0, ws, at);
                        if self.monitors[0].active != ws {
                            self.hide(id, out);
                        } else {
                            self.show(id, out);
                        }
                    }
                }
            }
            self.focused_monitor = self.focused_monitor.min(self.monitors.len() - 1);
        }
        self.retile_all(out);
        out.push(Effect::BarChanged);
    }

    /// Start managing a window. Applies workspace rules once; follows rule-placed windows
    /// after the start grace period.
    pub fn manage(&mut self, w: NewWindow, since_start: Duration, out: &mut Vec<Effect>) -> bool {
        if self.windows.contains_key(&w.id) || self.windows.len() >= MAX_WINDOWS {
            return false;
        }
        let mon = w.monitor.min(self.monitors.len() - 1);
        let active = self.monitors[mon].active;
        let explicit = w.workspace.filter(|ws| *ws < WORKSPACES);
        let ws = explicit
            .or_else(|| rule_workspace(w.exe, w.title))
            .unwrap_or(active);
        self.windows.insert(
            w.id,
            Managed {
                pid: w.pid,
                exe: w.exe.to_string(),
                monitor: mon,
                workspace: ws,
                floating: w.floating,
                minimized: false,
                maximized: false,
                min_size: (0, 0),
            },
        );
        self.attach(w.id, mon, ws, None);
        if ws == active {
            self.focused_monitor = mon;
            self.retile_monitor(mon, out);
        } else if explicit.is_some() || should_follow(since_start, self.config.follow_grace) {
            self.switch_workspace(mon, ws, out);
        } else {
            self.hide(w.id, out);
        }
        out.push(Effect::BarChanged);
        true
    }

    pub fn unmanage(&mut self, id: WinId, out: &mut Vec<Effect>) -> bool {
        let Some((mon, ws)) = self.detach(id) else {
            return false;
        };
        self.windows.remove(&id);
        self.journal.remove(id);
        self.minimized.retain(|x| *x != id);
        if self.monitors[mon].active == ws {
            self.retile_monitor(mon, out);
        }
        out.push(Effect::BarChanged);
        true
    }

    /// Like [`Model::unmanage`] but also focuses the workspace's next window when the
    /// removed one was focused (komorebi behaviour on close).
    pub fn closed(&mut self, id: WinId, out: &mut Vec<Effect>) -> bool {
        let focused_before = self.windows.get(&id).map(|w| {
            let m = &self.monitors[w.monitor];
            (
                m.workspaces[w.workspace].focused == Some(id) && m.active == w.workspace,
                w.monitor,
                w.workspace,
            )
        });
        if !self.unmanage(id, out) {
            return false;
        }
        if let Some((true, mon, ws)) = focused_before {
            if let Some(f) = self.monitors[mon].workspaces[ws].focused {
                out.push(Effect::Focus(f));
            }
        }
        true
    }

    pub fn on_foreground(&mut self, id: WinId, out: &mut Vec<Effect>) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if w.minimized || self.journal.contains(id) {
            return;
        }
        let (mon, ws) = (w.monitor, w.workspace);
        self.focused_monitor = mon;
        self.monitors[mon].workspaces[ws].focused = Some(id);
        if self.monitors[mon].active != ws {
            self.switch_workspace(mon, ws, out);
        }
    }

    pub fn on_minimized(&mut self, id: WinId, out: &mut Vec<Effect>) {
        if self.journal.contains(id) {
            return;
        }
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        if w.minimized {
            return;
        }
        w.minimized = true;
        let (mon, ws) = (w.monitor, w.workspace);
        self.minimized.retain(|x| *x != id);
        if self.minimized.len() >= MINIMIZED_HISTORY {
            self.minimized.remove(0);
        }
        self.minimized.push(id);
        let space = &mut self.monitors[mon].workspaces[ws];
        space.tree.remove(id);
        if space.focused == Some(id) {
            space.focused = None;
            let f = self.pick_focus(mon, ws);
            self.monitors[mon].workspaces[ws].focused = f;
        }
        if self.monitors[mon].active == ws {
            self.retile_monitor(mon, out);
        }
    }

    /// The window left the minimized state without the manager asking.
    pub fn on_restored(&mut self, id: WinId, out: &mut Vec<Effect>) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        let (mon, ws) = (w.monitor, w.workspace);
        let was_hidden = self.journal.remove(id);
        if !was_hidden {
            if !w.minimized {
                return;
            }
            w.minimized = false;
            let tile = !w.floating;
            self.minimized.retain(|x| *x != id);
            let space = &mut self.monitors[mon].workspaces[ws];
            if tile {
                let at = space.focused.filter(|f| space.tree.contains(*f));
                space.tree.insert(id, at);
            }
        }
        self.monitors[mon].workspaces[ws].focused = Some(id);
        self.focused_monitor = mon;
        if self.monitors[mon].active == ws {
            self.retile_monitor(mon, out);
        } else {
            self.switch_workspace(mon, ws, out);
        }
    }

    pub fn set_maximized(&mut self, id: WinId, maximized: bool, out: &mut Vec<Effect>) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        if w.maximized == maximized {
            return;
        }
        w.maximized = maximized;
        let mon = w.monitor;
        if !maximized && self.is_visible(id) && self.is_tiled(id) {
            self.retile_monitor(mon, out);
        }
    }

    pub fn switch_workspace(&mut self, mon: usize, target: usize, out: &mut Vec<Effect>) {
        if mon >= self.monitors.len() || target >= WORKSPACES {
            return;
        }
        let old = self.monitors[mon].active;
        self.focused_monitor = mon;
        if old == target {
            return;
        }
        let leaving = std::mem::take(&mut self.monitors[mon].workspaces[old].members);
        for &id in &leaving {
            self.hide(id, out);
        }
        self.monitors[mon].workspaces[old].members = leaving;
        let entering = std::mem::take(&mut self.monitors[mon].workspaces[target].members);
        for &id in &entering {
            self.show(id, out);
        }
        self.monitors[mon].workspaces[target].members = entering;
        let m = &mut self.monitors[mon];
        m.last = Some(old);
        m.active = target;
        if target == SPECIAL {
            // Every way into "S" (toggle, focus-workspace 10, following a window)
            // remembers where to return to.
            m.special_return = Some(old);
        }
        self.retile_monitor(mon, out);
        if let Some(f) = self.monitors[mon].workspaces[target].focused {
            out.push(Effect::Focus(f));
        }
        out.push(Effect::BarChanged);
    }

    /// Move a window to another workspace of its monitor without following it.
    pub fn send_to_workspace(&mut self, id: WinId, target: usize, out: &mut Vec<Effect>) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let (mon, from) = (w.monitor, w.workspace);
        if target >= WORKSPACES || target == from {
            return;
        }
        let active = self.monitors[mon].active;
        self.detach(id);
        let at = self.monitors[mon].workspaces[target].focused;
        self.attach(id, mon, target, at);
        if from == active {
            self.hide(id, out);
            self.retile_monitor(mon, out);
            if let Some(f) = self.monitors[mon].workspaces[from].focused {
                out.push(Effect::Focus(f));
            }
        } else if target == active {
            self.show(id, out);
            self.retile_monitor(mon, out);
        }
        out.push(Effect::BarChanged);
    }

    pub fn move_to_monitor(
        &mut self,
        id: WinId,
        target: usize,
        at: Option<WinId>,
        out: &mut Vec<Effect>,
    ) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let from = w.monitor;
        if target >= self.monitors.len() || target == from {
            return;
        }
        let was_visible = self.is_visible(id);
        self.detach(id);
        let ws = self.monitors[target].active;
        self.attach(id, target, ws, at);
        if !was_visible {
            self.show(id, out);
        }
        self.focused_monitor = target;
        self.retile_monitor(from, out);
        self.retile_monitor(target, out);
        out.push(Effect::BarChanged);
    }

    pub fn toggle_float(&mut self, id: WinId, out: &mut Vec<Effect>) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        w.floating = !w.floating;
        let (mon, ws, floating, minimized) = (w.monitor, w.workspace, w.floating, w.minimized);
        let space = &mut self.monitors[mon].workspaces[ws];
        if floating {
            space.tree.remove(id);
            let work = self.monitors[mon].work;
            let (width, height) = (work.width * 3 / 5, work.height * 7 / 10);
            out.push(Effect::Place(
                id,
                Rect {
                    x: work.x + (work.width - width) / 2,
                    y: work.y + (work.height - height) / 2,
                    width,
                    height,
                },
            ));
        } else if !minimized {
            space.tree.insert(id, None);
        }
        if self.monitors[mon].active == ws {
            self.retile_monitor(mon, out);
        }
    }

    pub fn toggle_special(&mut self, mon: usize, out: &mut Vec<Effect>) {
        let Some(m) = self.monitors.get(mon) else {
            return;
        };
        if m.active == SPECIAL {
            let back = m.special_return.or(m.last).unwrap_or(0);
            let back = if back == SPECIAL { 0 } else { back };
            self.switch_workspace(mon, back, out);
        } else {
            self.switch_workspace(mon, SPECIAL, out);
        }
    }

    pub fn cycle_workspace(&mut self, mon: usize, next: bool, out: &mut Vec<Effect>) {
        let Some(m) = self.monitors.get(mon) else {
            return;
        };
        let n = SPECIAL;
        let base = if m.active == SPECIAL {
            m.special_return.unwrap_or(0).min(n - 1)
        } else {
            m.active
        };
        let target = if next {
            (base + 1) % n
        } else {
            (base + n - 1) % n
        };
        self.switch_workspace(mon, target, out);
    }

    fn neighbour_of(&self, id: WinId, dir: Dir) -> Option<WinId> {
        let w = self.windows.get(&id)?;
        neighbour(&self.layout_of(w.monitor, w.workspace), id, dir)
    }

    /// Adjacent monitor in `dir` by work-area centres.
    fn monitor_toward(&self, from: usize, dir: Dir) -> Option<usize> {
        let c = |r: Rect| (r.x + r.width / 2, r.y + r.height / 2);
        let (fx, fy) = c(self.monitors.get(from)?.work);
        self.monitors
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != from)
            .filter_map(|(i, m)| {
                let (x, y) = c(m.work);
                let (along, cross) = match dir {
                    Dir::Left => (fx - x, (y - fy).abs()),
                    Dir::Right => (x - fx, (y - fy).abs()),
                    Dir::Up => (fy - y, (x - fx).abs()),
                    Dir::Down => (y - fy, (x - fx).abs()),
                };
                (along > 0).then_some(((along, cross), i))
            })
            .min()
            .map(|(_, i)| i)
    }

    /// Native move/resize finished (or a Win+drag ended) on a tiled window. Resizes
    /// adjust split ratios; moves swap with the tile under the cursor; the layout is
    /// always reapplied so the window snaps back into the grid.
    pub fn user_moved(
        &mut self,
        id: WinId,
        visual: Rect,
        cursor: (i32, i32),
        out: &mut Vec<Effect>,
    ) {
        if !self.is_tiled(id) {
            return;
        }
        let Some(layout) = self.tiled_rect(id) else {
            return;
        };
        let Some(mon) = self.windows.get(&id).map(|w| w.monitor) else {
            return;
        };
        let resized =
            (visual.width - layout.width).abs() > 2 || (visual.height - layout.height).abs() > 2;
        if resized {
            let edges = [
                (Dir::Left, layout.x - visual.x),
                (
                    Dir::Right,
                    visual.x + visual.width - layout.x - layout.width,
                ),
                (Dir::Up, layout.y - visual.y),
                (
                    Dir::Down,
                    visual.y + visual.height - layout.y - layout.height,
                ),
            ];
            for (dir, px) in edges {
                if px.abs() > 2 {
                    self.resize_px(id, dir, px, &mut Vec::new());
                }
            }
        } else if let Some(target_mon) = self.monitor_at(cursor.0, cursor.1) {
            let target = self.tiled_at(cursor.0, cursor.1);
            if target_mon != mon {
                self.move_to_monitor(id, target_mon, target, out);
                return;
            }
            if let Some(t) = target.filter(|t| *t != id) {
                let ws = self.monitors[mon].active;
                self.monitors[mon].workspaces[ws].tree.swap(id, t);
            }
        }
        self.retile_monitor(mon, out);
    }

    /// Live drag resize of a tiled window edge by `px` (positive grows outward).
    pub fn resize_px(&mut self, id: WinId, dir: Dir, px: i32, out: &mut Vec<Effect>) {
        if !self.is_tiled(id) {
            return;
        }
        let Some((mon, ws)) = self.windows.get(&id).map(|w| (w.monitor, w.workspace)) else {
            return;
        };
        let area = self.area(mon);
        if self.monitors[mon].workspaces[ws]
            .tree
            .grow_px(id, dir, px, area)
            && self.monitors[mon].active == ws
        {
            self.retile_monitor(mon, out);
        }
    }

    pub fn perform(&mut self, action: WmAction, out: &mut Vec<Effect>) {
        let mon = self.focused_monitor;
        let focused = self.focused_window();
        let tiled = focused.filter(|f| self.is_tiled(*f));
        match action {
            WmAction::Focus(d) => {
                let next = focused.and_then(|f| self.neighbour_of(f, d)).or_else(|| {
                    let m = self.monitor_toward(mon, d)?;
                    let mm = &self.monitors[m];
                    mm.workspaces[mm.active].focused
                });
                if let Some(n) = next {
                    self.on_foreground(n, out);
                    out.push(Effect::Focus(n));
                }
            }
            WmAction::Move(d) => {
                let Some(f) = tiled else { return };
                if let Some(n) = self.neighbour_of(f, d) {
                    let ws = self.monitors[mon].active;
                    self.monitors[mon].workspaces[ws].tree.swap(f, n);
                    self.retile_monitor(mon, out);
                } else if let Some(m) = self.monitor_toward(mon, d) {
                    self.move_to_monitor(f, m, None, out);
                    out.push(Effect::Focus(f));
                }
            }
            WmAction::ResizeAxis { vertical, grow } => {
                let Some(f) = tiled else { return };
                let step = self.config.resize_step;
                let delta = if grow { step } else { -step };
                let (first, second) = if vertical {
                    (Dir::Down, Dir::Up)
                } else {
                    (Dir::Right, Dir::Left)
                };
                let ws = self.monitors[mon].active;
                let tree = &mut self.monitors[mon].workspaces[ws].tree;
                if tree.grow(f, first, delta) || tree.grow(f, second, delta) {
                    self.retile_monitor(mon, out);
                }
            }
            WmAction::Close => out.extend(focused.map(Effect::Close)),
            WmAction::ToggleMaximize => out.extend(focused.map(Effect::ToggleMaximize)),
            WmAction::Minimize => out.extend(focused.map(Effect::Minimize)),
            WmAction::ToggleFloat => {
                if let Some(f) = focused {
                    self.toggle_float(f, out);
                }
            }
            WmAction::Flip => {
                let Some(f) = tiled else { return };
                let ws = self.monitors[mon].active;
                if self.monitors[mon].workspaces[ws].tree.flip(f) {
                    self.retile_monitor(mon, out);
                }
            }
            WmAction::RestoreLast => {
                while let Some(id) = self.minimized.pop() {
                    if self.windows.get(&id).is_some_and(|w| w.minimized) {
                        out.push(Effect::Restore(id));
                        break;
                    }
                }
            }
            WmAction::FocusWorkspace(n) => self.switch_workspace(mon, n as usize, out),
            WmAction::SendToWorkspace(n) => {
                if let Some(f) = focused {
                    self.send_to_workspace(f, n as usize, out);
                }
            }
            WmAction::FocusLastWorkspace => {
                if let Some(l) = self.monitors[mon].last {
                    self.switch_workspace(mon, l, out);
                }
            }
            WmAction::ToggleSpecial => self.toggle_special(mon, out),
            WmAction::CycleWorkspace { next } => self.cycle_workspace(mon, next, out),
            WmAction::Retile => self.retile_all(out),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect {
        x: 0,
        y: 32,
        width: 1920,
        height: 1048,
    };
    fn r(x: i32, y: i32, width: i32, height: i32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }
    fn model() -> Model {
        Model::new(Config::default(), &[WORK])
    }
    fn nw(id: WinId, exe: &str) -> NewWindow<'_> {
        NewWindow {
            id,
            pid: id as u32 + 1000,
            exe,
            title: "Window",
            monitor: 0,
            floating: false,
            workspace: None,
        }
    }
    const LATE: Duration = Duration::from_secs(120);
    const EARLY: Duration = Duration::from_secs(10);
    fn places(out: &[Effect]) -> Vec<(WinId, Rect)> {
        out.iter()
            .filter_map(|e| match e {
                Effect::Place(id, r) => Some((*id, *r)),
                _ => None,
            })
            .collect()
    }
    fn no_overlap(rects: &[(WinId, Rect)]) -> bool {
        rects.iter().enumerate().all(|(i, (_, a))| {
            rects[i + 1..].iter().all(|(_, b)| {
                a.x + a.width <= b.x
                    || b.x + b.width <= a.x
                    || a.y + a.height <= b.y
                    || b.y + b.height <= a.y
            })
        })
    }

    #[test]
    fn bsp_alternates_right_then_below_starting_vertical() {
        let mut t = Bsp::new();
        let area = r(0, 0, 1000, 800);
        let mut out = vec![];
        for id in 1..=4 {
            assert!(t.insert(id, Some(id - 1)));
        }
        assert!(!t.insert(3, None), "duplicates rejected");
        t.layout(area, 0, &mut out);
        assert_eq!(
            out,
            vec![
                (1, r(0, 0, 500, 800)),
                (2, r(500, 0, 500, 400)),
                (3, r(500, 400, 250, 400)),
                (4, r(750, 400, 250, 400)),
            ]
        );
        assert_eq!(t.leaves(), vec![1, 2, 3, 4]);
        assert_eq!(t.len(), 4);
        assert_eq!(t.last(), Some(4));
    }

    #[test]
    fn bsp_splits_the_focused_container_not_the_last() {
        let mut t = Bsp::new();
        t.insert(1, None);
        t.insert(2, Some(1));
        t.insert(3, Some(1)); // 1 is at depth 1 -> horizontal split of the left half
        let mut out = vec![];
        t.layout(r(0, 0, 1000, 800), 0, &mut out);
        assert_eq!(
            out,
            vec![
                (1, r(0, 0, 500, 400)),
                (3, r(0, 400, 500, 400)),
                (2, r(500, 0, 500, 800)),
            ]
        );
    }

    #[test]
    fn bsp_remove_collapses_and_memory_stays_bounded() {
        let mut t = Bsp::new();
        for id in 1..=3 {
            t.insert(id, Some(id - 1));
        }
        assert!(t.remove(2));
        assert!(!t.remove(2));
        let mut out = vec![];
        t.layout(r(0, 0, 1000, 800), 0, &mut out);
        assert_eq!(out, vec![(1, r(0, 0, 500, 800)), (3, r(500, 0, 500, 800))]);
        assert!(t.remove(1));
        t.layout(r(0, 0, 1000, 800), 0, &mut out);
        assert_eq!(out, vec![(3, r(0, 0, 1000, 800))]);
        for round in 0..1000u64 {
            t.insert(100 + round, None);
            t.remove(100 + round);
        }
        assert!(t.capacity_used() <= 8, "arena reuses slots");
        assert!(t.remove(3));
        assert!(t.is_empty());
        assert_eq!(t.capacity_used(), 0);
    }

    #[test]
    fn gaps_match_komorebi_padding() {
        let mut m = model();
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), LATE, &mut out);
        assert_eq!(places(&out), vec![(1, r(10, 42, 1900, 1028))]);
        out.clear();
        m.manage(nw(2, "notepad.exe"), LATE, &mut out);
        let p = places(&out);
        assert_eq!(p[0], (1, r(10, 42, 945, 1028)));
        assert_eq!(p[1], (2, r(965, 42, 945, 1028)));
        assert_eq!(
            p[1].1.x - (p[0].1.x + p[0].1.width),
            10,
            "10px between windows"
        );
        assert_eq!(WORK.x + WORK.width - (p[1].1.x + p[1].1.width), 10);
        for n in 3..=8 {
            m.manage(nw(n, "notepad.exe"), LATE, &mut out);
        }
        assert!(no_overlap(&m.layout_of(0, 0)));
    }

    #[test]
    fn neighbours_are_geometric() {
        let mut t = Bsp::new();
        for id in 1..=4 {
            t.insert(id, Some(id - 1));
        }
        let mut rects = vec![];
        t.layout(r(0, 0, 1000, 800), 5, &mut rects);
        assert_eq!(neighbour(&rects, 1, Dir::Right), Some(2));
        assert_eq!(neighbour(&rects, 1, Dir::Left), None);
        assert_eq!(neighbour(&rects, 2, Dir::Down), Some(3));
        assert_eq!(neighbour(&rects, 3, Dir::Up), Some(2));
        assert_eq!(neighbour(&rects, 4, Dir::Up), Some(2));
        assert_eq!(neighbour(&rects, 4, Dir::Left), Some(3));
        assert_eq!(neighbour(&rects, 3, Dir::Left), Some(1));
        assert_eq!(neighbour(&rects, 4, Dir::Down), None);
        assert_eq!(neighbour(&rects, 9, Dir::Down), None);
    }

    #[test]
    fn swap_resize_and_flip() {
        let mut t = Bsp::new();
        t.insert(1, None);
        t.insert(2, Some(1));
        t.insert(3, Some(2));
        assert!(t.swap(1, 3));
        assert_eq!(t.leaves(), vec![3, 2, 1]);
        assert!(!t.swap(1, 99));
        // Window 3 now holds the left half: growing its right edge widens the split.
        assert!(t.grow(3, Dir::Right, 0.05));
        let mut out = vec![];
        t.layout(r(0, 0, 1000, 800), 0, &mut out);
        assert_eq!(out[0].1.width, 550);
        assert!(!t.grow(3, Dir::Left, 0.05), "workspace edge");
        // Window 1 (right column, bottom): left edge growth shrinks the left column.
        assert!(t.grow(1, Dir::Left, 0.05));
        assert_eq!(t.ratio_of_parent(3), Some(0.5));
        for _ in 0..30 {
            t.grow(3, Dir::Right, 0.05);
        }
        assert_eq!(t.ratio_of_parent(3), Some(MAX_RATIO));
        assert_eq!(t.axis_of_parent(2), Some(Axis::Horizontal));
        assert!(t.flip(2));
        assert_eq!(t.axis_of_parent(2), Some(Axis::Vertical));
        assert!(!Bsp::new().flip(1));
        let area = r(0, 0, 1000, 800);
        let mut t = Bsp::new();
        t.insert(1, None);
        t.insert(2, Some(1));
        assert!(t.grow_px(1, Dir::Right, 100, area));
        t.layout(area, 0, &mut out);
        assert_eq!(out[0].1.width, 600);
    }

    #[test]
    fn rules_match_legacy_configuration() {
        assert_eq!(rule_workspace("brave.exe", ""), Some(1));
        assert_eq!(rule_workspace("BRAVE.EXE", ""), Some(1));
        assert_eq!(rule_workspace("opera_gx.exe", ""), Some(1));
        assert_eq!(rule_workspace("Code.exe", ""), Some(2));
        assert_eq!(rule_workspace("pycharm64.exe", ""), Some(2));
        assert_eq!(rule_workspace("studio64.exe", ""), Some(4));
        assert_eq!(
            rule_workspace("java.exe", "Burp Suite Professional v2024.1"),
            Some(4)
        );
        assert_eq!(
            rule_workspace("soffice.bin", "Doc1 - LibreOffice Writer"),
            Some(4)
        );
        assert_eq!(rule_workspace("WhatsApp.Root.exe", ""), Some(5));
        assert_eq!(rule_workspace("Discord.exe", ""), Some(5));
        assert_eq!(rule_workspace("Music.UI.exe", ""), Some(6));
        assert_eq!(rule_workspace("steamwebhelper.exe", ""), Some(7));
        assert_eq!(rule_workspace("notepad.exe", "Untitled"), None);
        assert_eq!(
            rule_workspace("brave-helper.exe", ""),
            None,
            "exact names only"
        );
        assert!(exe_is("Obsidian.exe", "obsidian"));
        assert!(!exe_is("é.exe", "x"));
    }

    #[test]
    fn classification_follows_ignore_float_and_force_rules() {
        let base = WindowFacts {
            exe: "notepad.exe",
            title: "Untitled - Notepad",
            class: "Notepad",
            visible: true,
            has_caption: true,
            resizable: true,
            width: 800,
            height: 600,
            ..Default::default()
        };
        assert_eq!(classify(&base), Disposition::Tile);
        let ignore = [
            WindowFacts {
                own_process: true,
                ..base
            },
            WindowFacts {
                visible: false,
                ..base
            },
            WindowFacts {
                cloaked: true,
                ..base
            },
            WindowFacts {
                class: "Shell_TrayWnd",
                ..base
            },
            WindowFacts {
                class: "Windows.UI.Core.CoreWindow",
                ..base
            },
            WindowFacts {
                exe: "GoogleDriveFS.exe",
                ..base
            },
            WindowFacts {
                title: "C:\\WINDOWS\\System32\\cmd.exe",
                ..base
            },
            WindowFacts {
                title: "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
                ..base
            },
            WindowFacts { title: "", ..base },
            WindowFacts {
                tool_window: true,
                ..base
            },
            WindowFacts {
                has_caption: false,
                ..base
            },
            WindowFacts { width: 20, ..base },
            WindowFacts {
                owned: true,
                has_caption: false,
                ..base
            },
        ];
        for f in ignore {
            assert_eq!(classify(&f), Disposition::Ignore, "{f:?}");
        }
        for exe in ["Obsidian.exe", "WhatsApp.Root.exe"] {
            let f = WindowFacts {
                exe,
                has_caption: false,
                resizable: false,
                ..base
            };
            assert_eq!(classify(&f), Disposition::Tile, "{exe}");
        }
        let float = [
            WindowFacts {
                exe: "windhawk.exe",
                ..base
            },
            WindowFacts {
                title: "Picture-in-Picture",
                has_caption: false,
                ..base
            },
            WindowFacts {
                owned: true,
                ..base
            },
            WindowFacts {
                dialog_frame: true,
                ..base
            },
            WindowFacts {
                resizable: false,
                ..base
            },
            WindowFacts {
                class: "#32770",
                ..base
            },
        ];
        for f in float {
            assert_eq!(classify(&f), Disposition::Float, "{f:?}");
        }
        assert_eq!(
            classify(&WindowFacts {
                owned: true,
                app_window: true,
                ..base
            }),
            Disposition::Tile
        );
    }

    #[test]
    fn new_rule_window_is_hidden_during_grace_and_followed_later() {
        let mut m = model();
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), EARLY, &mut out);
        out.clear();
        m.manage(nw(2, "brave.exe"), EARLY, &mut out);
        assert_eq!(m.active_workspace(0), Some(0));
        assert!(out.contains(&Effect::Hide(2)));
        assert!(m.journal().contains(2));
        assert_eq!(m.window(2).map(|w| w.workspace), Some(1));

        out.clear();
        m.manage(nw(3, "Code.exe"), LATE, &mut out);
        assert_eq!(m.active_workspace(0), Some(2));
        assert!(out.contains(&Effect::Hide(1)));
        assert!(out.contains(&Effect::Focus(3)));
        assert!(!out.contains(&Effect::Hide(3)));
        assert_eq!(places(&out), vec![(3, r(10, 42, 1900, 1028))]);
        assert!(m.journal().contains(1) && m.journal().contains(2));

        // A window without a rule lands on the active workspace and is not hidden.
        out.clear();
        m.manage(nw(4, "notepad.exe"), LATE, &mut out);
        assert_eq!(m.window(4).map(|w| w.workspace), Some(2));
        assert!(!out.iter().any(|e| matches!(e, Effect::Hide(_))));
    }

    #[test]
    fn workspace_switch_hides_shows_and_journals() {
        let mut m = model();
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), LATE, &mut out);
        m.manage(nw(2, "notepad.exe"), LATE, &mut out);
        out.clear();
        m.perform(WmAction::FocusWorkspace(3), &mut out);
        assert_eq!(&out[..2], &[Effect::Hide(1), Effect::Hide(2)]);
        assert_eq!(m.journal().len(), 2);
        assert!(m.journal_mut().take_dirty());
        assert!(!m.journal_mut().take_dirty());
        out.clear();
        m.perform(WmAction::FocusWorkspace(3), &mut out);
        assert!(out.is_empty(), "no-op switch");
        m.perform(WmAction::FocusLastWorkspace, &mut out);
        assert_eq!(m.active_workspace(0), Some(0));
        assert!(out.contains(&Effect::Show(1)) && out.contains(&Effect::Show(2)));
        assert!(out.contains(&Effect::Focus(2)));
        assert!(m.journal().is_empty());
        let bar = m.bar(0);
        assert_eq!(bar.len(), WORKSPACES);
        assert_eq!(bar[10].name, "S");
        assert!(bar[0].focused && !bar[3].focused);
        assert_eq!(
            bar[0].windows,
            vec![(1, "notepad.exe".into()), (2, "notepad.exe".into())]
        );
    }

    #[test]
    fn special_workspace_toggles_back_and_cycle_skips_it() {
        let mut m = model();
        let mut out = vec![];
        m.perform(WmAction::FocusWorkspace(4), &mut out);
        m.perform(WmAction::ToggleSpecial, &mut out);
        assert_eq!(m.active_workspace(0), Some(SPECIAL));
        m.perform(WmAction::FocusLastWorkspace, &mut out);
        assert_eq!(m.active_workspace(0), Some(4));
        m.perform(WmAction::ToggleSpecial, &mut out);
        m.perform(WmAction::ToggleSpecial, &mut out);
        assert_eq!(m.active_workspace(0), Some(4));
        m.perform(WmAction::ToggleSpecial, &mut out);
        m.perform(WmAction::CycleWorkspace { next: true }, &mut out);
        assert_eq!(m.active_workspace(0), Some(5));
        m.perform(WmAction::FocusWorkspace(9), &mut out);
        m.perform(WmAction::CycleWorkspace { next: true }, &mut out);
        assert_eq!(m.active_workspace(0), Some(0));
        m.perform(WmAction::CycleWorkspace { next: false }, &mut out);
        assert_eq!(m.active_workspace(0), Some(9));
    }

    #[test]
    fn send_to_workspace_does_not_follow() {
        let mut m = model();
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), LATE, &mut out);
        m.manage(nw(2, "notepad.exe"), LATE, &mut out);
        out.clear();
        m.perform(WmAction::SendToWorkspace(6), &mut out);
        assert_eq!(m.active_workspace(0), Some(0));
        assert!(out.contains(&Effect::Hide(2)));
        assert!(out.contains(&Effect::Focus(1)));
        assert_eq!(places(&out), vec![(1, r(10, 42, 1900, 1028))]);
        assert_eq!(m.window(2).map(|w| w.workspace), Some(6));
        out.clear();
        m.perform(WmAction::FocusWorkspace(6), &mut out);
        assert!(out.contains(&Effect::Show(2)));
    }

    #[test]
    fn focus_move_resize_flip_actions() {
        let mut m = model();
        let mut out = vec![];
        for id in 1..=3 {
            m.manage(nw(id, "notepad.exe"), LATE, &mut out);
        }
        out.clear();
        assert_eq!(m.focused_window(), Some(3));
        m.perform(WmAction::Focus(Dir::Left), &mut out);
        assert_eq!(out, vec![Effect::Focus(1)]);
        assert_eq!(m.focused_window(), Some(1));
        out.clear();
        m.perform(WmAction::Move(Dir::Right), &mut out);
        assert_eq!(m.monitors()[0].workspaces[0].tree.leaves(), vec![2, 1, 3]);
        out.clear();
        m.perform(
            WmAction::ResizeAxis {
                vertical: false,
                grow: true,
            },
            &mut out,
        );
        // 1 sits in the right column: growing horizontally moves its left edge.
        assert_eq!(m.tiled_rect(2).map(|r| r.width), Some(850));
        out.clear();
        m.perform(WmAction::Flip, &mut out);
        assert_eq!(
            m.monitors()[0].workspaces[0].tree.axis_of_parent(1),
            Some(Axis::Vertical)
        );
        assert!(!places(&out).is_empty());
        out.clear();
        m.perform(WmAction::Close, &mut out);
        m.perform(WmAction::ToggleMaximize, &mut out);
        assert_eq!(out, vec![Effect::Close(1), Effect::ToggleMaximize(1)]);
    }

    #[test]
    fn minimize_and_restore_last() {
        let mut m = model();
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), LATE, &mut out);
        m.manage(nw(2, "notepad.exe"), LATE, &mut out);
        out.clear();
        m.perform(WmAction::Minimize, &mut out);
        assert_eq!(out, vec![Effect::Minimize(2)]);
        out.clear();
        m.on_minimized(2, &mut out);
        assert_eq!(places(&out), vec![(1, r(10, 42, 1900, 1028))]);
        assert_eq!(m.focused_window(), Some(1));
        out.clear();
        m.perform(WmAction::RestoreLast, &mut out);
        assert_eq!(out, vec![Effect::Restore(2)]);
        out.clear();
        m.on_restored(2, &mut out);
        assert_eq!(places(&out).len(), 2);
        assert_eq!(m.focused_window(), Some(2));
        out.clear();
        m.perform(WmAction::RestoreLast, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn hidden_window_restored_by_user_switches_workspace() {
        let mut m = model();
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), LATE, &mut out);
        m.perform(WmAction::FocusWorkspace(1), &mut out);
        // The manager's own minimize must not be treated as a user minimize.
        m.on_minimized(1, &mut out);
        assert!(!m.window(1).is_some_and(|w| w.minimized));
        out.clear();
        m.on_restored(1, &mut out);
        assert_eq!(m.active_workspace(0), Some(0));
        assert!(!m.journal().contains(1));
        assert!(out.contains(&Effect::Focus(1)));
        assert!(!out.contains(&Effect::Show(1)));
    }

    #[test]
    fn foreground_on_other_workspace_switches_and_closing_refocuses() {
        let mut m = model();
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), LATE, &mut out);
        m.manage(nw(2, "notepad.exe"), LATE, &mut out);
        m.send_to_workspace(2, 4, &mut out);
        m.on_foreground(2, &mut out);
        assert_eq!(m.active_workspace(0), Some(0), "hidden windows are ignored");
        m.journal_mut().remove(2);
        m.on_foreground(2, &mut out);
        assert_eq!(m.active_workspace(0), Some(4));
        m.manage(nw(3, "notepad.exe"), LATE, &mut out);
        out.clear();
        assert!(m.closed(3, &mut out));
        assert!(out.contains(&Effect::Focus(2)));
        assert!(!m.closed(3, &mut out));
        assert!(m.unmanage(1, &mut out));
        assert!(!m.journal().contains(1));
        assert_eq!(m.len(), 1);
    }

    #[test]
    fn floating_windows_are_not_tiled_and_maximized_are_not_placed() {
        let mut m = model();
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), LATE, &mut out);
        m.manage(
            NewWindow {
                floating: true,
                ..nw(2, "windhawk.exe")
            },
            LATE,
            &mut out,
        );
        assert_eq!(m.layout_of(0, 0).len(), 1);
        out.clear();
        m.on_foreground(1, &mut out);
        m.perform(WmAction::ToggleFloat, &mut out);
        assert!(m.layout_of(0, 0).is_empty());
        assert_eq!(
            places(&out),
            vec![(1, r(384, 189, 1152, 733))],
            "centred floating rect"
        );
        out.clear();
        m.perform(WmAction::ToggleFloat, &mut out);
        assert_eq!(places(&out), vec![(1, r(10, 42, 1900, 1028))]);
        m.manage(nw(3, "notepad.exe"), LATE, &mut out);
        m.set_maximized(1, true, &mut out);
        out.clear();
        m.perform(WmAction::Retile, &mut out);
        assert_eq!(
            places(&out).iter().map(|p| p.0).collect::<Vec<_>>(),
            vec![3]
        );
        out.clear();
        m.set_maximized(1, false, &mut out);
        assert_eq!(places(&out).len(), 2);
    }

    #[test]
    fn user_drag_swaps_and_native_resize_adjusts_ratio() {
        let mut m = model();
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), LATE, &mut out);
        m.manage(nw(2, "notepad.exe"), LATE, &mut out);
        let r1 = m.tiled_rect(1).unwrap_or(EMPTY);
        out.clear();
        m.user_moved(1, Rect { x: 1200, ..r1 }, (1500, 500), &mut out);
        assert_eq!(m.monitors()[0].workspaces[0].tree.leaves(), vec![2, 1]);
        assert_eq!(places(&out).len(), 2, "snaps back into the grid");
        let r2 = m.tiled_rect(2).unwrap_or(EMPTY);
        m.user_moved(
            2,
            Rect {
                width: r2.width + 191,
                ..r2
            },
            (0, 0),
            &mut out,
        );
        assert_eq!(m.tiled_rect(2).map(|r| r.width), Some(1136));
        assert_eq!(m.tiled_at(1700, 500), Some(1));
        assert_eq!(m.tiled_at(1145, 500), Some(2), "gap belongs to a tile");
        assert_eq!(m.tiled_at(-5, 500), None);
    }

    #[test]
    fn monitor_removal_rehomes_windows() {
        let second = r(1920, 0, 1920, 1080);
        let mut m = Model::new(Config::default(), &[WORK, second]);
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), LATE, &mut out);
        m.manage(
            NewWindow {
                monitor: 1,
                ..nw(2, "notepad.exe")
            },
            LATE,
            &mut out,
        );
        assert_eq!(m.window(2).map(|w| w.monitor), Some(1));
        m.on_foreground(1, &mut out);
        out.clear();
        m.perform(WmAction::Focus(Dir::Right), &mut out);
        assert!(out.contains(&Effect::Focus(2)));
        m.set_monitors(&[WORK], &mut out);
        assert_eq!(m.monitors().len(), 1);
        assert_eq!(m.window(2).map(|w| w.monitor), Some(0));
        assert_eq!(m.layout_of(0, 0).len(), 2);
    }

    #[test]
    fn journal_roundtrip_and_limits() {
        let mut j = Journal::default();
        assert!(j.insert(10, 1));
        assert!(!j.insert(10, 1));
        assert!(j.insert(11, 2));
        let bytes = j.to_bytes().unwrap_or_default();
        assert_eq!(Journal::parse(&bytes).ok(), Some(j.entries().to_vec()));
        assert!(Journal::parse(b"garbage").is_err());
        assert!(Journal::parse(br#"{"schema":2,"hidden":[]}"#).is_err());
        assert!(Journal::parse(&vec![b' '; MAX_JOURNAL_BYTES + 1]).is_err());
        assert_eq!(j.drain().len(), 2);
        assert!(j.is_empty() && j.take_dirty());
        for i in 0..(MAX_JOURNAL as u64 + 10) {
            j.insert(i, 0);
        }
        assert_eq!(j.len(), MAX_JOURNAL);
    }

    #[test]
    fn registry_is_bounded() {
        let mut m = model();
        let mut out = vec![];
        for id in 0..(MAX_WINDOWS as u64 + 5) {
            m.manage(
                NewWindow {
                    floating: true,
                    ..nw(id, "x.exe")
                },
                LATE,
                &mut out,
            );
        }
        assert_eq!(m.len(), MAX_WINDOWS);
    }

    #[test]
    fn actions_encode_roundtrip_and_shortcuts_map() {
        use crate::shortcuts::Action;
        let mut all = vec![
            WmAction::Close,
            WmAction::ToggleMaximize,
            WmAction::ToggleFloat,
            WmAction::Flip,
            WmAction::Minimize,
            WmAction::RestoreLast,
            WmAction::FocusLastWorkspace,
            WmAction::ToggleSpecial,
            WmAction::CycleWorkspace { next: true },
            WmAction::CycleWorkspace { next: false },
            WmAction::Retile,
        ];
        for d in Dir::ALL {
            all.push(WmAction::Focus(d));
            all.push(WmAction::Move(d));
        }
        for (vertical, grow) in [(false, false), (false, true), (true, false), (true, true)] {
            all.push(WmAction::ResizeAxis { vertical, grow });
        }
        for n in 0..WORKSPACES as u8 {
            all.push(WmAction::FocusWorkspace(n));
            all.push(WmAction::SendToWorkspace(n));
        }
        for a in &all {
            assert_eq!(WmAction::decode(a.encode()), Some(*a));
        }
        for bad in [0, 0x104, 0x40b, 0x50b, 0x700, usize::MAX] {
            assert_eq!(WmAction::decode(bad), None);
        }
        assert_eq!(
            WmAction::from_shortcut(&Action::Focus(2)),
            Some(WmAction::Focus(Dir::Up))
        );
        assert_eq!(
            WmAction::from_shortcut(&Action::Resize(-40)),
            Some(WmAction::ResizeAxis {
                vertical: false,
                grow: false
            })
        );
        assert_eq!(WmAction::from_shortcut(&Action::Launcher), None);
        assert!(should_follow(FOLLOW_GRACE, FOLLOW_GRACE));
        assert!(!should_follow(EARLY, FOLLOW_GRACE));
    }

    #[test]
    fn owned_window_joins_owner_workspace_and_is_never_hidden() {
        let mut m = model();
        let mut out = vec![];
        m.manage(nw(1, "notepad.exe"), EARLY, &mut out);
        m.manage(nw(2, "brave.exe"), EARLY, &mut out);
        assert!(m.journal().contains(2), "rule window hidden during grace");
        out.clear();
        // Brave's dialog: explicit owner workspace wins over grace and rules.
        m.manage(
            NewWindow {
                workspace: Some(1),
                floating: true,
                ..nw(3, "notepad.exe")
            },
            EARLY,
            &mut out,
        );
        assert_eq!(m.window(3).map(|w| w.workspace), Some(1));
        assert_eq!(m.active_workspace(0), Some(1));
        assert!(!out.contains(&Effect::Hide(3)));
        assert!(out.contains(&Effect::Show(2)), "owner becomes visible");
        assert!(!m.journal().contains(3) && !m.journal().contains(2));
        // Already-active owner workspace: no switch.
        out.clear();
        m.manage(
            NewWindow {
                workspace: Some(1),
                ..nw(4, "notepad.exe")
            },
            EARLY,
            &mut out,
        );
        assert!(!out.iter().any(|e| matches!(e, Effect::Hide(_))));
        assert_eq!(m.active_workspace(0), Some(1));
    }

    #[test]
    fn special_return_is_set_on_every_entry_path() {
        let mut m = model();
        let mut out = vec![];
        m.perform(WmAction::FocusWorkspace(3), &mut out);
        m.perform(WmAction::FocusWorkspace(4), &mut out);
        m.perform(WmAction::FocusWorkspace(SPECIAL as u8), &mut out);
        assert_eq!(m.monitors()[0].special_return, Some(4));
        m.perform(WmAction::CycleWorkspace { next: true }, &mut out);
        assert_eq!(m.active_workspace(0), Some(5));
        // A stale value from an earlier toggle is replaced on the next direct entry.
        m.perform(WmAction::ToggleSpecial, &mut out);
        m.perform(WmAction::ToggleSpecial, &mut out);
        assert_eq!(m.active_workspace(0), Some(5));
        m.perform(WmAction::FocusWorkspace(7), &mut out);
        m.perform(WmAction::FocusWorkspace(SPECIAL as u8), &mut out);
        m.perform(WmAction::FocusWorkspace(2), &mut out);
        m.perform(WmAction::FocusWorkspace(SPECIAL as u8), &mut out);
        m.perform(WmAction::ToggleSpecial, &mut out);
        assert_eq!(m.active_workspace(0), Some(2));
    }

    #[test]
    fn fit_min_keeps_rect_inside_area() {
        let area = r(0, 0, 1920, 1080);
        assert_eq!(
            fit_min(r(100, 100, 300, 300), (0, 0), area),
            r(100, 100, 300, 300)
        );
        assert_eq!(
            fit_min(r(1500, 100, 300, 300), (600, 0), area),
            r(1320, 100, 600, 300)
        );
        assert_eq!(
            fit_min(r(10, 900, 300, 100), (0, 400), area),
            r(10, 680, 300, 400)
        );
        assert_eq!(
            fit_min(r(10, 10, 300, 100), (5000, 0), area),
            r(0, 10, 1920, 100),
            "capped at the area"
        );
    }

    #[test]
    fn min_size_grows_tile_toward_sibling_then_clamps() {
        let mut m = model();
        let mut out = vec![];
        for id in 1..=3 {
            m.manage(nw(id, "notepad.exe"), LATE, &mut out);
        }
        // 1 | (2 over 3): the stacked windows get 509 px each.
        assert_eq!(m.tiled_rect(3).map(|r| r.height), Some(509));
        out.clear();
        assert!(m.set_min_size(3, 0, 700, &mut out));
        assert!(!m.set_min_size(3, 0, 650, &mut out), "minimum only grows");
        let p = places(&out);
        assert_eq!(p.len(), 3);
        let r3 = p
            .iter()
            .find(|(id, _)| *id == 3)
            .map(|x| x.1)
            .unwrap_or(EMPTY);
        assert!(r3.height >= 699, "{r3:?}");
        assert!(no_overlap(&p), "space taken from the sibling");
        let bottom = WORK.y + WORK.height;
        assert!(p.iter().all(|(_, r)| r.y + r.height <= bottom));

        // More than the split can give: the window keeps its size but stays on screen.
        out.clear();
        assert!(m.set_min_size(3, 0, 1000, &mut out));
        let r3 = places(&out)
            .into_iter()
            .find(|(id, _)| *id == 3)
            .map(|x| x.1)
            .unwrap_or(EMPTY);
        assert_eq!(r3.height, 1000);
        assert!(r3.y + r3.height <= bottom - 5, "{r3:?}");
        assert!(r3.y >= WORK.y);

        // Not visible: recorded without placing.
        m.perform(WmAction::FocusWorkspace(1), &mut out);
        out.clear();
        assert!(m.set_min_size(1, 1200, 0, &mut out));
        assert!(places(&out).is_empty());
    }
}
