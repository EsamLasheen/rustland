# Architecture

Solo Leveling Shell is one Windows executable written in Rust against the Win32/WinRT APIs (the `windows`
crate). It has no runtime dependencies beyond what ships with Windows 10/11: no browser engine, no
scripting runtime, no local web server, no helper programs.

## Principles

- **Event-driven.** Surfaces repaint when something changes (WinEvent hooks, Core Audio callbacks,
  WLAN/IP notifications, registry and directory change notifications). Timers exist only where the
  data is inherently periodic (3 s metrics, minute clock) or while an animation/caret is visible.
- **Bounded work.** Every queue, cache and history has a size limit; background threads never block
  the UI thread, and the UI thread never waits on the network, the WLAN service or disk scans.
- **Give the desktop back.** Screen reservations, the hidden taskbar and windows hidden on other
  workspaces are restored on exit, on sign-out and after a crash (supervisor + on-disk journal).
- **Preserve user data.** Settings and data files are written atomically with a `.previous` copy; a
  file that does not parse is moved aside as `.corrupt` instead of being overwritten.
- **Measure before claiming.** `scripts/compare-stacks.ps1` measures CPU and memory of a process tree.

## Processes

```
MonarchNext.exe --shell          supervisor (≈2 MB): restarts the shell after a crash, stops on sign-out
└── MonarchNext.exe --shell-child   the shell: all surfaces, hooks and subsystems
```

## Shell threads

| Thread | Owns |
|---|---|
| UI (`runtime.rs`) | bar, capsule, left bar, panel, tooltip, launcher, help; all surface state |
| `input-hooks` | low-level keyboard/mouse hooks (63 bindings, edge reveal, Win+drag) |
| `monarch-wm` | tiling window manager (`wm.rs`, pure model in `core::wm_layout`) |
| `metrics` | CPU/RAM/battery, interface counters, Wi-Fi name every 3 s |
| sysctl workers | radios, brightness, Wi-Fi manager, media sessions, processes, disks, weather |
| wallpaper + glow | desktop wallpaper window, minute clock layer, active-window glow |
| tray | `Shell_TrayWnd` host forwarding to Explorer, icon table |
| ai | Gemini requests over WinHTTP, DPAPI-protected key |
| watchers | Obsidian focus-timer file, ASUS Win-key lock state |

Workers talk to the UI thread with `PostMessage` plus a bounded mailbox; nothing is shared mutably.

## Rendering

`render.rs` draws every surface with Direct2D (software target: no GPU driver loaded for small
surfaces) and DirectWrite, into layered windows (`UpdateLayeredWindow`, per-pixel alpha) or into PNG
files (`--render <surface> <out.png>` for screenshots and visual checks). Fonts are embedded and
loaded as a private collection (SIL OFL, see `crates/desktop/assets/fonts/OFL-NOTICE.txt`); Arabic
text is right-to-left aware. Icons are Lucide SVG paths converted to Direct2D geometry.

## Crates

- `crates/core`: platform-independent logic with unit tests: tiling layout tree and workspaces,
  launcher ranking and calculator, network usage and alerts, focus timer, local data (tasks, clipboard).
- `crates/desktop`: the application (surfaces, runtime, subsystems, installer, setup wizard).

## Data

Everything lives under `%LOCALAPPDATA%\MonarchNext`: `shell.json` (settings), `todo.json`,
`alerts.json`, `network-usage.json`, `gemini.key` (DPAPI), `wm-hidden.json` (crash journal),
`session.log`. Nothing is sent anywhere except weather lookups (Open-Meteo, optional IP location)
and Gemini requests that the user explicitly sends.

## Design reference

The visual language follows the *Shadow Monarch* theme and the layout of
[NibrasShell](https://github.com/AhmedSaadi0/NibrasShell) (top bar, smart capsule, left bar, floating
panels). This is an independent implementation, not a port or an endorsed distribution of that project.
