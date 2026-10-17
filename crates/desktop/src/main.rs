#![windows_subsystem = "windows"]
use monarch_platform as platform;
mod ai;
mod appicons;
mod capsule;
mod config;
mod focus_watch;
mod glow;
mod help;
mod icons;
mod input_hooks;
mod launcher;
mod leftbar;
mod osd;
mod panel;
mod presets;
mod render;
mod runtime;
mod setup;
mod setup_wizard;
mod shell;
mod subsystems;
mod supervisor;
mod sysctl;
mod taskbar;
mod theme;
mod topbar;
mod tray;
mod wallpaper;
mod winkey_watch;
mod wm;
mod wm_bridge;

use windows::{
    core::{w, PCWSTR},
    Win32::UI::WindowsAndMessaging::*,
};

fn message(text: &str, error: bool) {
    let t = platform::wide(text);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(t.as_ptr()),
            w!("Monarch Next"),
            MB_OK
                | if error {
                    MB_ICONERROR
                } else {
                    MB_ICONINFORMATION
                },
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("--diagnostics") => diagnostics(args.get(1)),
        Some("--benchmark") => benchmark(args.get(1)),
        Some("--feature-audit") => feature_audit(args.get(1)),
        Some("--shell") => supervisor::run(),
        Some("--shell-child") => runtime::run(),
        Some("--render") => render_snapshot(args.get(1), args.get(2), args.get(3)),
        Some("--preview") if args.get(1).map(String::as_str) == Some("topbar") => {
            shell::preview_topbar(render::Color::hex(
                args.get(2).map(String::as_str).unwrap_or("#62baf8"),
            ))
        }
        Some("--finish-uninstall") => {
            let pid = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
            if pid == 0 {
                Err(std::io::Error::other("Missing uninstaller process"))
            } else {
                setup::finish_uninstall(pid)
            }
        }
        Some("--uninstall") => {
            let answer = unsafe {
                MessageBoxW(
                    None,
                    w!("Remove Monarch Next? The Windows taskbar comes back and your saved preferences are kept."),
                    w!("Monarch Next"),
                    MB_YESNO | MB_ICONQUESTION,
                )
            };
            if answer == IDYES {
                setup::uninstall()
            } else {
                Ok(())
            }
        }
        Some("--setup") => run_setup(),
        None => {
            let is_setup = std::env::current_exe()
                .ok()
                .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().to_lowercase()))
                .is_some_and(|s| s.contains("setup"));
            if is_setup {
                run_setup()
            } else {
                supervisor::run()
            }
        }
        _ => Err(std::io::Error::other(
            "Unknown option. Open the application normally or use --diagnostics <output.json>.",
        )),
    };
    if let Err(e) = result {
        if let Ok(p) = platform::paths() {
            platform::log(&p, &format!("ERROR: {e}"));
        }
        // a shell that cannot start is not a crash: the supervisor reports it once instead of retrying
        if args.first().map(String::as_str) == Some("--shell-child") {
            std::process::exit(supervisor::FATAL_EXIT);
        }
        message(&e.to_string(), true);
        std::process::exit(1);
    }
}

/// Installer flow: questions first, then install/update, settings, autostart and launch.
fn run_setup() -> std::io::Result<()> {
    let update = setup::install_dir()
        .map(|d| d.join("MonarchNext.exe").exists())
        .unwrap_or(false);
    let Some(choices) = setup_wizard::ask(update) else {
        return Ok(()); // cancelled
    };
    // install first: an update closes the running shell, so the new answers cannot be overwritten
    let target = setup::install()?;
    setup_wizard::apply(&choices)?;
    setup::set_autostart(&target, choices.autostart)?;
    std::process::Command::new(target).arg("--shell").spawn()?;
    Ok(())
}

fn diagnostics(output: Option<&String>) -> std::io::Result<()> {
    let output = output.ok_or_else(|| std::io::Error::other("Provide an output JSON path"))?;
    let p = platform::paths()?;
    let config = monarch_core::Settings::load(&p.settings);
    let features = serde_json::json!({"topBar":true,"appbarReservation":true,"tray":true,"workspaces":true,"smartCapsule":true,"mediaSessions":true,"weather":true,"focusTimer":true,"leftBar":true,"panels":10,"wifiManager":true,"radios":true,"brightness":true,"volume":true,"processes":true,"disks":true,"alerts":true,"networkUsage":true,"clipboardHistory":true,"todo":true,"power":true,"launcher":true,"calculator":true,"tilingWindowManager":true,"wallpaper":true,"dynamicAccent":true,"focusGlow":true,"taskbarHidden":true,"gemini":true,"crashRecovery":true,"autostart":true});
    let report = serde_json::json!({
        "product":"Monarch Next", "version":env!("CARGO_PKG_VERSION"), "architecture":std::env::consts::ARCH,
        "settingsPath":p.settings, "settingsValid":config.is_ok(), "settingsError":config.err().map(|e|e.to_string()),
        "externalProgramsRequired":[], "browserRuntimeRequired":false,
        "shortcutCount":63,
        "implementedShortcutCount":input_hooks::BINDINGS.len(),
        "features":features,
        "releaseStatus":"release-candidate", "cleanMachineValidated":false
    });
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&report).map_err(std::io::Error::other)?,
    )
}

fn feature_audit(output: Option<&String>) -> std::io::Result<()> {
    let output = output.ok_or_else(|| std::io::Error::other("Provide an output JSON path"))?;
    let shortcuts: Vec<_> = input_hooks::BINDINGS
        .iter()
        .map(|b| serde_json::json!({"binding":b.label,"action":format!("{:?}",b.act)}))
        .collect();
    let report = serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"bindings":shortcuts.len(),"shortcuts":shortcuts});
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&report).map_err(std::io::Error::other)?,
    )
}

fn benchmark(output: Option<&String>) -> std::io::Result<()> {
    let output = output.ok_or_else(|| std::io::Error::other("Provide an output JSON path"))?;
    let started = std::time::Instant::now();
    let apps = platform::applications();
    let index_us = started.elapsed().as_micros();
    let mut samples = vec![];
    for query in [
        "",
        "edge",
        "studio",
        "excel",
        "windows",
        "photos",
        "إعدادات",
        "برامج",
        "does not exist",
    ] {
        for _ in 0..50 {
            let at = std::time::Instant::now();
            let names: Vec<&str> = apps.iter().map(|a| a.search_key.as_str()).collect();
            std::hint::black_box(monarch_core::launcher::rank(
                std::hint::black_box(&names),
                std::hint::black_box(query),
                8,
            ));
            samples.push(at.elapsed().as_nanos() as u64);
        }
    }
    samples.sort_unstable();
    let report = serde_json::json!({"version":env!("CARGO_PKG_VERSION"),"indexEntries":apps.len(),"indexDurationMs":index_us as f64/1000.0,"cachedFilterMedianMs":samples[samples.len()/2] as f64/1_000_000.0,"cachedFilterP95Ms":samples[samples.len()*95/100] as f64/1_000_000.0,"filterSamples":samples.len(),"note":"Single-machine development measurement; excludes UI presentation and clean-machine validation."});
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&report).map_err(std::io::Error::other)?,
    )
}

/// `--render <surface> <out.png> [accent #rrggbb]`: draw a shell surface into an image with
/// representative data, for pixel comparison with the legacy theme. Never touches the desktop.
fn render_snapshot(
    surface: Option<&String>,
    output: Option<&String>,
    accent: Option<&String>,
) -> std::io::Result<()> {
    let output = output.ok_or_else(|| std::io::Error::other("Provide an output PNG path"))?;
    let accent = render::Color::hex(accent.map(String::as_str).unwrap_or("#62baf8"));
    let tokens = theme::Tokens::new(accent);
    match surface.map(String::as_str) {
        Some("topbar") => {
            let state = topbar::sample();
            let r = render::snapshot(std::path::Path::new(output), 1920, 32, 1.0, |c| {
                c.clear(render::Color::hex("#16233f")); // wallpaper stand-in so translucent surfaces read correctly
                topbar::paint(c, 1920., &tokens, &state);
            })
            .map_err(std::io::Error::other);
            render::shutdown();
            r
        }
        Some(kind @ ("capsule-idle" | "capsule-osd" | "capsule-weather" | "capsule-media" | "capsule-focus")) => {
            let mut s = capsule::sample();
            let (w, h) = if kind == "capsule-idle" || kind == "capsule-osd" { (720, 40) } else { (520, 300) };
            if kind == "capsule-osd" {
                s.request = Some(capsule::Request { priority: 2, timeout_ms: 2000, icon: "sun", text: "39%".into(), progress: Some(0.39), tone: capsule::Tone::Primary });
            }
            if kind != "capsule-idle" && kind != "capsule-osd" {
                s.expanded = true;
                s.tab = Some(match kind { "capsule-media" => capsule::Tab::Media, "capsule-focus" => capsule::Tab::Focus, _ => capsule::Tab::Weather });
                if kind == "capsule-media" {
                    s.media = Some(capsule::Media { player: "Spotify".into(), title: "Solo Leveling OST".into(), artist: "Hiroyuki Sawano".into(), playing: true, progress: 0.42, sessions: 1, index: 0, can_prev: true, can_next: true });
                    s.playing = true;
                }
            }
            let r = render::snapshot(std::path::Path::new(output), w, h, 1.0, |c| {
                c.clear(render::Color::hex("#16233f"));
                if s.expanded {
                    let frame = render::Rect::new((w as f32 - capsule::CARD.0) / 2., capsule::CARD_TOP, capsule::CARD.0, capsule::CARD.1);
                    capsule::paint_card(c, w as f32, &tokens, &s, frame, 1.);
                } else {
                    let iw = capsule::idle_width(c, &s);
                    capsule::paint_idle(c, w as f32, &tokens, &s, iw);
                }
            })
            .map_err(std::io::Error::other);
            render::shutdown();
            r
        }
        Some("leftbar") => {
            let s = leftbar::sample();
            let r = render::snapshot(std::path::Path::new(output), 60, 1048, 1.0, |c| {
                c.clear(render::Color::hex("#16233f"));
                leftbar::paint(c, 1048., &tokens, &s);
            })
            .map_err(std::io::Error::other);
            render::shutdown();
            r
        }
        Some(kind) if kind.starts_with("panel-") => {
            let tab = match &kind[6..] {
                "notifications" => leftbar::Tab::Notifications, "weather" => leftbar::Tab::Weather, "monitoring" => leftbar::Tab::Monitoring,
                "network" => leftbar::Tab::Network, "clipboard" => leftbar::Tab::Clipboard, "todo" => leftbar::Tab::Todo,
                "power" => leftbar::Tab::Power, "settings" => leftbar::Tab::Settings, "ai" => leftbar::Tab::Ai, _ => leftbar::Tab::Dashboard,
            };
            let mut s = panel::sample(tab);
            if tab == leftbar::Tab::Clipboard { s.clipboard = vec!["https://github.com".into(), "نص عربي منسوخ للتجربة".into(), "cargo build --release".into()]; }
            let r = render::snapshot(std::path::Path::new(output), panel::WIDTH as u32 + 20, 1022, 1.0, |c| {
                c.clear(render::Color::hex("#16233f"));
                panel::paint(c, 1022., &tokens, &s, 1.);
            })
            .map_err(std::io::Error::other);
            render::shutdown();
            r
        }
        other => Err(std::io::Error::other(format!(
            "Unknown surface {other:?}. Available: topbar, capsule-idle, capsule-osd, capsule-weather, capsule-media, capsule-focus"
        ))),
    }
}
