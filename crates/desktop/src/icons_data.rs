//! Lucide icons (ISC licence) as used by the shell, rendered as Direct2D geometry.
//! The SVG subset parser covers path / circle / rect / ellipse with M L H V C S Q T A Z commands.
pub static ICONS: &[(&str, &str)] = &[
    (
        "athar",
        r##"<path d="M12 3l3 6 6 3-6 3-3 6-3-6-6-3 6-3z"/><circle cx="12" cy="12" r="2"/>"##,
    ),
    (
        "crown",
        r##"<path d="M2 7l4.5 4L12 4l5.5 7L22 7l-2 12H4L2 7z"/><path d="M4 21h16"/>"##,
    ),
    (
        "grid",
        r##"<rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/>"##,
    ),
    (
        "bell",
        r##"<path d="M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9"/><path d="M10.3 21a1.94 1.94 0 0 0 3.4 0"/>"##,
    ),
    (
        "cloud",
        r##"<path d="M17.5 19H9a7 7 0 1 1 6.71-9h1.79a4.5 4.5 0 1 1 0 9z"/>"##,
    ),
    (
        "sun",
        r##"<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M6.34 17.66l-1.41 1.41M19.07 4.93l-1.41 1.41"/>"##,
    ),
    (
        "moon",
        r##"<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9z"/>"##,
    ),
    (
        "hibernate",
        r##"<path d="M12 2v20M4.9 6.5l14.2 11M19.1 6.5L4.9 17.5"/><path d="M9.5 3.5L12 6l2.5-2.5M9.5 20.5L12 18l2.5 2.5"/>"##,
    ),
    (
        "rain",
        r##"<path d="M4 14.9A7 7 0 1 1 15.7 8h1.8a4.5 4.5 0 0 1 2.5 8.2"/><path d="M16 14v6M8 14v6M12 16v6"/>"##,
    ),
    (
        "snow",
        r##"<path d="M2 12h20M12 2v20M20 16l-4-4 4-4M4 8l4 4-4 4M16 4l-4 4-4-4M8 20l4-4 4 4"/>"##,
    ),
    ("bolt", r##"<path d="M13 2L3 14h9l-1 8 10-12h-9l1-8z"/>"##),
    ("activity", r##"<path d="M22 12h-4l-3 9L9 3l-3 9H2"/>"##),
    (
        "wifi",
        r##"<path d="M5 12.55a11 11 0 0 1 14.08 0"/><path d="M1.42 9a16 16 0 0 1 21.16 0"/><path d="M8.53 16.11a6 6 0 0 1 6.95 0"/><circle cx="12" cy="20" r="0.6" fill="currentColor"/>"##,
    ),
    (
        "wifiOff",
        r##"<path d="M2 2l20 20"/><path d="M8.5 16.5a5 5 0 0 1 7 0"/><path d="M5 12.86a10 10 0 0 1 5.17-2.69"/><path d="M19 12.86a10 10 0 0 0-2.07-1.41"/><circle cx="12" cy="20" r="0.6" fill="currentColor"/>"##,
    ),
    ("bt", r##"<path d="M7 7l10 10-5 5V2l5 5L7 17"/>"##),
    (
        "clipboard",
        r##"<rect x="8" y="2" width="8" height="4" rx="1"/><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"/>"##,
    ),
    (
        "todo",
        r##"<path d="M3 17l2 2 4-4M3 7l2 2 4-4M13 6h8M13 12h8M13 18h8"/>"##,
    ),
    (
        "power",
        r##"<path d="M18.36 6.64a9 9 0 1 1-12.73 0"/><path d="M12 2v10"/>"##,
    ),
    (
        "cpu",
        r##"<rect x="5" y="5" width="14" height="14" rx="2"/><rect x="9" y="9" width="6" height="6"/><path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3"/>"##,
    ),
    (
        "ram",
        r##"<path d="M6 19v-3M10 19v-3M14 19v-3M18 19v-3"/><rect x="2" y="7" width="20" height="9" rx="1.5"/>"##,
    ),
    (
        "battery",
        r##"<rect x="2" y="7" width="17" height="10" rx="2"/><path d="M22 11v2"/>"##,
    ),
    (
        "plug",
        r##"<path d="M12 22v-5M9 8V2M15 8V2M18 8v5a6 6 0 0 1-12 0V8z"/>"##,
    ),
    (
        "vol",
        r##"<path d="M11 5L6 9H2v6h4l5 4V5z"/><path d="M15.5 8.5a5 5 0 0 1 0 7M19 5a10 10 0 0 1 0 14"/>"##,
    ),
    (
        "mute",
        r##"<path d="M11 5L6 9H2v6h4l5 4V5z"/><path d="M22 9l-6 6M16 9l6 6"/>"##,
    ),
    (
        "play",
        r##"<path d="M6 4l14 8-14 8V4z" fill="currentColor"/>"##,
    ),
    (
        "pause",
        r##"<rect x="6" y="4" width="4" height="16" rx="1" fill="currentColor"/><rect x="14" y="4" width="4" height="16" rx="1" fill="currentColor"/>"##,
    ),
    (
        "next",
        r##"<path d="M5 4l10 8-10 8V4z" fill="currentColor"/><path d="M19 5v14"/>"##,
    ),
    (
        "prev",
        r##"<path d="M19 20L9 12l10-8v16z" fill="currentColor"/><path d="M5 19V5"/>"##,
    ),
    (
        "music",
        r##"<path d="M9 18V5l12-2v13"/><circle cx="6" cy="18" r="3"/><circle cx="18" cy="16" r="3"/>"##,
    ),
    (
        "timer",
        r##"<circle cx="12" cy="13" r="8"/><path d="M12 9v4l2 2M9 2h6"/>"##,
    ),
    (
        "lock",
        r##"<rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>"##,
    ),
    (
        "logout",
        r##"<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="M16 17l5-5-5-5M21 12H9"/>"##,
    ),
    (
        "restart",
        r##"<path d="M3 12a9 9 0 1 0 3-6.7L3 8"/><path d="M3 3v5h5"/>"##,
    ),
    ("x", r##"<path d="M18 6L6 18M6 6l12 12"/>"##),
    ("plus", r##"<path d="M12 5v14M5 12h14"/>"##),
    ("check", r##"<path d="M20 6L9 17l-5-5"/>"##),
    (
        "trash",
        r##"<path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6"/>"##,
    ),
    (
        "image",
        r##"<rect x="3" y="3" width="18" height="18" rx="2"/><circle cx="9" cy="9" r="2"/><path d="M21 15l-5-5L5 21"/>"##,
    ),
    (
        "settings",
        r##"<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>"##,
    ),
    ("down", r##"<path d="M12 5v14M19 12l-7 7-7-7"/>"##),
    ("up", r##"<path d="M12 19V5M5 12l7-7 7 7"/>"##),
    (
        "disk",
        r##"<ellipse cx="12" cy="5" rx="9" ry="3"/><path d="M3 5v14c0 1.66 4 3 9 3s9-1.34 9-3V5"/><path d="M3 12c0 1.66 4 3 9 3s9-1.34 9-3"/>"##,
    ),
    (
        "wind",
        r##"<path d="M17.7 7.7a2.5 2.5 0 1 1 1.8 4.3H2M9.6 4.6A2 2 0 1 1 11 8H2M12.6 19.4A2 2 0 1 0 14 16H2"/>"##,
    ),
    (
        "search",
        r##"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>"##,
    ),
    (
        "calculator",
        r##"<rect width="16" height="20" x="4" y="2" rx="2"/><path d="M8 6h8"/><path d="M16 14v4"/><path d="M8 10h.01"/><path d="M12 10h.01"/><path d="M16 10h.01"/><path d="M8 14h.01"/><path d="M12 14h.01"/><path d="M8 18h.01"/><path d="M12 18h.01"/>"##,
    ),
    (
        "globe",
        r##"<circle cx="12" cy="12" r="10"/><path d="M12 2a14.5 14.5 0 0 0 0 20 14.5 14.5 0 0 0 0-20"/><path d="M2 12h20"/>"##,
    ),
    (
        "app",
        r##"<rect width="18" height="18" x="3" y="3" rx="4"/><path d="M9 9h6v6H9z"/>"##,
    ),
];
