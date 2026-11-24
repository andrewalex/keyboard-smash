# Keyboard Smash

## Overview
A fullscreen Windows app for toddlers to bang on a keyboard. Displays pressed keys large and colorful with synthesized sounds and simple effects, with a low-level keyboard hook that locks the keyboard down.

## Tech Stack
- Rust (edition 2024), MSVC toolchain
- `windows-sys` only: Win32 for window/input, GDI for drawing, `waveOut` for audio
- Static CRT (`.cargo/config.toml`), so `target/release/smash.exe` is a single standalone exe (~290 KB)

## Building
**Windows-only.** Build from Windows, not WSL.
```
cargo build --release
```
May require Administrator for the keyboard hook.

## Architecture
Single file, `src/main.rs`, in sections:
1. **Keyboard hook** - `WH_KEYBOARD_LL` on its own thread with a message loop. Swallows **every** key (returns 1) and posts it to the main window as `WM_APP_KEY` (with a repeat flag) / `WM_APP_KEYUP`. Key state is tracked in the hook (`KEY_DOWN` array), not via `GetAsyncKeyState`, because swallowed keys never reach Windows' key state. Refreshed every 2s (new hook installed before the old one is removed).
2. **Sounds** - clips synthesized at startup (pentatonic chimes, firework whistle/pops), no asset files. An audio thread keeps one `waveOut` device open and mixes up to 12 overlapping voices into 3×23ms buffers; the main thread sends clip ids over an mpsc channel.
3. **Main loop** - fullscreen topmost popup window, ~60fps, GDI into an off-screen bitmap. `App` holds all state: current key, background color fade, particles, rainbow, counting dots, menu, night/sleep fade.

## Behavior
- Letters/keys: big letter + sparkle ring + note. Numbers: count out dots. Space: rainbow (holding grows it). Enter: fireworks (holding keeps launching). Clicks: sparkles at the click.
- Key auto-repeat never re-triggers effects; a held key just stays on screen.
- After 2 min with no input, fades to a "Bye bye!" night sky; any input wakes it. Never auto-quits, so a toddler isn't left at the desktop.
- Grown-up controls: **hold Alt+F1 for 2s** opens a menu (sound on/off, quit; arrows + Enter, Esc closes instantly, auto-closes after 20s). **Hold Alt+F4 for 2s** quits. Both show on-screen progress.

## Key Decisions
- Child-proofing relies entirely on the OS-level hook, not window key events.
- Sticky/Filter/Toggle Keys hotkeys (Shift ×5 etc.) are disabled at start (in-memory SPI, no ini write, so even a crash is undone at next logon) and restored on exit.
- Focus, topmost and `ClipCursor` are re-asserted every 500ms; `SetThreadExecutionState` keeps the display awake.
- Single instance via a named mutex.
- `PlaySoundW` was tried first and dropped: one sound at a time, cut-offs, unreliable.
- Cannot be blocked from user mode: Ctrl+Alt+Del, Win+L, the power button. Win+L can be disabled by policy: `HKCU\Software\Microsoft\Windows\CurrentVersion\Policies\System` → `DisableLockWorkstation`=1 (DWORD).
- History: originally a Python + Pygame app (`smash.py`), replaced by this Rust port.
