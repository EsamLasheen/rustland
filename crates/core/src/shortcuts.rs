//! The legacy shortcut contract. Unimplemented bindings never capture input.
pub const WIN: u8 = 1;
pub const SHIFT: u8 = 2;
pub const ALT: u8 = 4;
pub const CTRL: u8 = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Close,
    Maximize,
    Minimize,
    Restore,
    Launch(&'static str),
    Launcher,
    Dashboard,
    Monitoring,
    Help,
    Clipboard,
    Todo,
    Reload,
    Focus(u8),
    Move(u8),
    Resize(i32),
}
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    pub modifiers: u8,
    pub key: u32,
    pub label: &'static str,
    pub purpose: &'static str,
    pub action: Option<Action>,
}
macro_rules! b {
    ($mods:expr,$key:expr,$label:expr,$purpose:expr,$action:expr) => {
        Binding {
            modifiers: $mods,
            key: $key,
            label: $label,
            purpose: $purpose,
            action: $action,
        }
    };
}
use Action::*;
pub const BINDINGS: &[Binding] = &[
    b!(WIN, 81, "Win+Q", "Close window", Some(Close)),
    b!(WIN, 77, "Win+M", "Toggle maximize", Some(Maximize)),
    b!(WIN, 70, "Win+F", "Toggle floating", None),
    b!(WIN, 74, "Win+J", "Flip tile layout", None),
    b!(WIN, 78, "Win+N", "Minimize window", Some(Minimize)),
    b!(
        WIN | SHIFT,
        78,
        "Win+Shift+N",
        "Restore last minimized window",
        Some(Restore)
    ),
    b!(WIN, 13, "Win+Enter", "Terminal", Some(Launch("wt.exe"))),
    b!(WIN, 84, "Win+T", "Terminal", Some(Launch("wt.exe"))),
    b!(WIN, 82, "Win+R", "Application search", Some(Launcher)),
    b!(
        WIN,
        79,
        "Win+O",
        "Obsidian vault",
        Some(Launch("obsidian://open?vault=obsidian"))
    ),
    b!(
        WIN | SHIFT,
        67,
        "Win+Shift+C",
        "VS Code",
        Some(Launch("code.exe"))
    ),
    b!(
        WIN | SHIFT,
        69,
        "Win+Shift+E",
        "File Explorer",
        Some(Launch("explorer.exe"))
    ),
    b!(
        WIN | SHIFT,
        66,
        "Win+Shift+B",
        "Brave",
        Some(Launch("brave.exe"))
    ),
    b!(
        WIN | SHIFT,
        75,
        "Win+Shift+K",
        "Task Manager",
        Some(Launch("taskmgr.exe"))
    ),
    b!(
        WIN | SHIFT,
        77,
        "Win+Shift+M",
        "Task Manager",
        Some(Launch("taskmgr.exe"))
    ),
    b!(WIN, 112, "Win+F1", "Dashboard", Some(Dashboard)),
    b!(WIN, 113, "Win+F2", "Notifications", None),
    b!(WIN, 114, "Win+F3", "Weather", None),
    b!(WIN, 115, "Win+F4", "System monitoring", Some(Monitoring)),
    b!(WIN, 116, "Win+F5", "Network panel", None),
    b!(WIN, 117, "Win+F6", "Clipboard history", Some(Clipboard)),
    b!(WIN, 118, "Win+F7", "Todo", Some(Todo)),
    b!(
        WIN | SHIFT,
        82,
        "Win+Shift+R",
        "Clipboard history",
        Some(Clipboard)
    ),
    b!(WIN | SHIFT, 76, "Win+Shift+L", "Power menu", None),
    b!(WIN, 83, "Win+S", "Dashboard", Some(Dashboard)),
    b!(WIN, 191, "Win+/", "Shortcut reference", Some(Help)),
    b!(WIN, 80, "Win+P", "Media capsule", None),
    b!(WIN, 87, "Win+W", "Weather capsule", None),
    b!(WIN, 37, "Win+Left", "Focus left", Some(Focus(0))),
    b!(WIN, 39, "Win+Right", "Focus right", Some(Focus(1))),
    b!(WIN, 38, "Win+Up", "Focus up", Some(Focus(2))),
    b!(WIN, 40, "Win+Down", "Focus down", Some(Focus(3))),
    b!(
        WIN | SHIFT,
        37,
        "Win+Shift+Left",
        "Move window left",
        Some(Move(0))
    ),
    b!(
        WIN | SHIFT,
        39,
        "Win+Shift+Right",
        "Move window right",
        Some(Move(1))
    ),
    b!(
        WIN | SHIFT,
        38,
        "Win+Shift+Up",
        "Move window up",
        Some(Move(2))
    ),
    b!(
        WIN | SHIFT,
        40,
        "Win+Shift+Down",
        "Move window down",
        Some(Move(3))
    ),
    b!(
        WIN | ALT,
        37,
        "Win+Alt+Left",
        "Decrease window width",
        Some(Resize(-40))
    ),
    b!(
        WIN | ALT,
        39,
        "Win+Alt+Right",
        "Increase window width",
        Some(Resize(40))
    ),
    b!(WIN, 27, "Win+Escape", "Previous workspace", None),
    b!(WIN, 72, "Win+H", "Special workspace", None),
    b!(WIN, 49, "Win+1", "Workspace 1", None),
    b!(WIN, 50, "Win+2", "Workspace 2", None),
    b!(WIN, 51, "Win+3", "Workspace 3", None),
    b!(WIN, 52, "Win+4", "Workspace 4", None),
    b!(WIN, 53, "Win+5", "Workspace 5", None),
    b!(WIN, 54, "Win+6", "Workspace 6", None),
    b!(WIN, 55, "Win+7", "Workspace 7", None),
    b!(WIN, 56, "Win+8", "Workspace 8", None),
    b!(WIN, 57, "Win+9", "Workspace 9", None),
    b!(WIN, 48, "Win+0", "Workspace 10", None),
    b!(WIN | SHIFT, 49, "Win+Shift+1", "Send to workspace 1", None),
    b!(WIN | SHIFT, 50, "Win+Shift+2", "Send to workspace 2", None),
    b!(WIN | SHIFT, 51, "Win+Shift+3", "Send to workspace 3", None),
    b!(WIN | SHIFT, 52, "Win+Shift+4", "Send to workspace 4", None),
    b!(WIN | SHIFT, 53, "Win+Shift+5", "Send to workspace 5", None),
    b!(WIN | SHIFT, 54, "Win+Shift+6", "Send to workspace 6", None),
    b!(WIN | SHIFT, 55, "Win+Shift+7", "Send to workspace 7", None),
    b!(WIN | SHIFT, 56, "Win+Shift+8", "Send to workspace 8", None),
    b!(WIN | SHIFT, 57, "Win+Shift+9", "Send to workspace 9", None),
    b!(WIN | SHIFT, 48, "Win+Shift+0", "Send to workspace 10", None),
    b!(
        WIN | ALT,
        82,
        "Win+Alt+R",
        "Reload saved colors/settings",
        Some(Reload)
    ),
    b!(WIN | ALT, 83, "Win+Alt+S", "Sleep", None),
    b!(WIN | SHIFT, 65, "Win+Shift+A", "AI capture preview", None),
];
pub fn find(modifiers: u8, key: u32) -> Option<usize> {
    BINDINGS
        .iter()
        .position(|b| b.modifiers == modifiers && b.key == key && b.action.is_some())
}
pub fn reference() -> String {
    let ready = BINDINGS.iter().filter(|b| b.action.is_some()).count();
    let mut text = format!("{ready}/{} legacy bindings implemented. Enable explicitly in the control hub.\r\nMove uses 40px steps; workspace tiling is still pending.\r\nOptional app shortcuts require that app to be installed.\r\n\r\n", BINDINGS.len());
    for b in BINDINGS {
        text.push_str(&format!(
            "{}  |  {}  |  {}\r\n",
            b.label,
            b.purpose,
            if b.action.is_some() {
                "Available"
            } else {
                "Pending - input untouched"
            }
        ));
    }
    text
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_and_unique_contract() {
        assert_eq!(BINDINGS.len(), 63);
        for (i, a) in BINDINGS.iter().enumerate() {
            assert!(!BINDINGS[i + 1..]
                .iter()
                .any(|b| a.key == b.key && a.modifiers == b.modifiers));
        }
    }
    #[test]
    fn pending_and_reserved_input_passes_through() {
        for (m, k) in [
            (WIN, 76),
            (WIN, 86),
            (WIN, 73),
            (WIN, 32),
            (WIN | SHIFT, 83),
            (WIN, 49),
            (WIN, 116),
            (WIN | CTRL, 81),
        ] {
            assert_eq!(find(m, k), None);
        }
        assert!(find(WIN, 81).is_some());
        assert!(find(WIN | SHIFT, 78).is_some());
    }
}
