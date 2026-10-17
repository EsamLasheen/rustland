# Performance

Measured on the same laptop (i5-12500H, 16 GB, Windows 11), 60 s idle window, every process of
the stack including child processes, with `scripts/compare-stacks.ps1`.

Solo Leveling Shell replaced an earlier setup of the same theme built from several tools
(komorebi + whkd + Zebar with WebView2 widgets + Flow Launcher + a small Rust hub).

| | Previous setup | Solo Leveling Shell |
|---|---|---|
| Processes | 20 | 2 |
| CPU (share of all cores) | 0.31 % | 0.026 % |
| Private memory | 954 MB | 34 MB |
| Working set | 514 MB | 73 MB |
| Install size | 456 MB + WebView2 | 2.6 MB single executable |
| First paint after start | — | 110–140 ms |

Reproduce:

```powershell
.\scripts\compare-stacks.ps1 -Roots MonarchNext -Seconds 60
```

Targets used during development: first paint < 150 ms, idle CPU < 0.5 %, private memory < 30 MiB
(currently ~34 MiB with the wallpaper engine on).
