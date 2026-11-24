# Keyboard Smash

A fullscreen app for toddlers to bang on the keyboard. Keys show up big and colorful with soft chime sounds, and the keyboard is locked down so little hands can't escape to the desktop.

- **Letters and keys:** big letter, a sparkle ring, a note
- **Numbers:** count out that many dots
- **Space:** a rainbow (hold it to make it grow)
- **Enter:** fireworks (hold it for more)
- **Mouse clicks:** sparkles where you click
- **No presses for 2 minutes:** fades to a sleepy "Bye bye!" night sky

## Grown-up controls

| Action | How |
|---|---|
| Menu (sound on/off, quit) | Hold **Alt+F1** for 2 seconds. Arrows + Enter to choose, Esc to close. |
| Quit | Hold **Alt+F4** for 2 seconds |

## Build

Requires Rust (MSVC toolchain) on Windows.

```
cargo build --release
```

Output: `target\release\smash.exe`, a single standalone exe with no runtime or DLLs needed. Copy it anywhere and run it.

## Notes

- Windows only.
- If it reports the keyboard hook couldn't be installed, run it as Administrator.
- Windows reserves a few keys that no app can block: **Ctrl+Alt+Del**, **Win+L** (lock), and the power button. Win+L can be disabled by setting the DWORD `DisableLockWorkstation` = 1 under `HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\System`.
