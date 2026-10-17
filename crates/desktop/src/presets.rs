//! Theme presets offered in the dashboard (accent of each palette; the base surfaces stay the legacy ones).
pub const PRESETS: [(&str, &str); 8] = [
    ("DraculaDark", "#bd93f9"),
    ("CatppuccinDark", "#cba6f7"),
    ("NordDark", "#88c0d0"),
    ("TokyoNightDark", "#7aa2f7"),
    ("GruvboxDark", "#fabd2f"),
    ("RosePineDark", "#ebbcba"),
    ("SolarizedDark", "#268bd2"),
    ("OxocarbonDark", "#78a9ff"),
];

pub fn accent(name: &str) -> Option<&'static str> {
    PRESETS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}
pub fn names() -> Vec<String> {
    PRESETS.iter().map(|(n, _)| n.to_string()).collect()
}
