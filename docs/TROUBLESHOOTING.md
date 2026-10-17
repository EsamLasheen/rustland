# Troubleshooting

## Shortcuts (Win+…) do nothing, and Win alone does not open Start

**Cause (ASUS TUF / ROG laptops):** Armoury Crate's *Win Key* lock is on. The lock lives in the keyboard
controller, so the Windows key produces no input at all — not to Windows, not to Raw Input, not to any
hook. No software (Solo Leveling Shell, komorebi, whkd, AutoHotkey) can see a key that is never sent.

How it was diagnosed (2026-10-17):

1. Solo Leveling Shell's keyboard hook received every other key (letters, Alt+Shift…) but never `VK_LWIN`.
2. `scripts/diagnostics/rawkeys.ps1` (Raw Input, below all hooks) also never saw the Win key.
3. `HKCU\Software\ASUS\Armoury Crate Service\SystemFeature\WinKeyStatus` was `0`, and Armoury Crate's
   live state (`SystemFeature_WinKey` in `ScenarioProfile\Config\CurrentStatusCache.json`) was `0`.
4. Writing the registry value alone does nothing: only Armoury Crate (or Fn+Win) talks to the controller.

**Fix:** any of

- Press **Fn + Win** once.
- Armoury Crate → Home → **System Settings** → System Configuration → **Win Key** (tile turns orange).
- Run `powershell -ExecutionPolicy Bypass -File scripts\asus-winkey.ps1` (drives that same tile).

Solo Leveling Shell watches `WinKeyStatus` and shows a capsule warning with these steps whenever the lock turns on.

## Verifying the keyboard hook

Create an empty file `%LOCALAPPDATA%\MonarchNext\debug` and restart the shell: every Win-chord the hook
sees is written to `session.log` (`key vk=… mods=… match=…`, then `cmd …`). Delete the file when done —
it is checked once per run.

## Going back to Windows' own taskbar

Settings → Apps → *Solo Leveling Shell* → Uninstall. The taskbar returns as it was before installation.
