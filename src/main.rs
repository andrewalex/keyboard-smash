//! Keyboard Smash - a fullscreen app for toddlers (Rust port of smash.py).
//!
//! Displays pressed keys big and colorful, with soft chime sounds.
//!   - Letters / keys: big letter, a small sparkle ring, a note.
//!   - Number keys: count out that many dots.
//!   - Space: a rainbow. Hold it and the rainbow keeps growing.
//!   - Enter: fireworks.
//!   - No presses for a while: fades to a sleepy "Bye bye!" night sky.
//!
//! Grown-ups:
//!   - Hold Alt+F1 (2s) to open the menu: sound on/off, quit. Esc closes it.
//!   - Or hold Alt+F4 (2s) to quit.
//!
//! Lockdown strategy:
//! - A low-level keyboard hook swallows EVERY key. Nothing reaches Windows or
//!   any other app, even if our window loses focus. We only see the key.
//! - The hook is refreshed every 2s (new one installed before the old one is
//!   removed), in case Windows silently drops it.
//! - Sticky/Filter/Toggle Keys hotkeys (Shift x5, hold Shift, hold NumLock)
//!   are disabled while running and restored on exit.
//! - The window is topmost, re-asserts focus, hides the cursor and confines
//!   the mouse to itself. Display sleep / screen saver are suppressed.
//!
//! Not blockable from user mode: Ctrl+Alt+Del, Win+L, the power button.
//!
//! Pure Win32 + GDI, no other dependencies, so the exe is small and standalone.

#![windows_subsystem = "windows"]

use std::collections::VecDeque;
use std::f64::consts::{FRAC_PI_2, PI, TAU};
use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Media::Audio::*;
use windows_sys::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{CreateEventW, CreateMutexW, WaitForSingleObject};
use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};
use windows_sys::Win32::UI::Accessibility::*;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// =============================================================================
// Constants
// =============================================================================

const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;
const VK_SPACE: u32 = 0x20;
const VK_UP: u32 = 0x26;
const VK_DOWN: u32 = 0x28;
const VK_F1: u32 = 0x70;
const VK_F4: u32 = 0x73;
const VK_MENU: u32 = 0x12;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;

const MODIFIER_VKS: [u32; 15] = [
    0x11, 0xA2, 0xA3, // Ctrl
    0x10, 0xA0, 0xA1, // Shift
    0x12, 0xA4, 0xA5, // Alt
    0x5B, 0x5C, 0x5D, // Win, Win, Menu
    0x14, 0x90, 0x91, // Caps, Num, Scroll lock
];

/// How long Alt+F4 must be held to quit.
const EXIT_HOLD: Duration = Duration::from_secs(2);
/// How long Alt+F1 must be held to open the menu.
const MENU_HOLD: Duration = Duration::from_secs(2);
/// The menu closes by itself after this long without a menu key.
const MENU_TIMEOUT: Duration = Duration::from_secs(20);
/// No presses for this long -> fade to the "bye bye" night screen.
const SLEEP_AFTER: Duration = Duration::from_secs(120);

/// Hook thread -> main window. wparam = virtual key, lparam = 1 if auto-repeat.
const WM_APP_KEY: u32 = WM_APP + 1;
/// Hook thread -> main window. wparam = virtual key.
const WM_APP_KEYUP: u32 = WM_APP + 2;
/// Hook thread -> main window if the hook failed to install. wparam = error.
const WM_APP_HOOK_FAILED: u32 = WM_APP + 3;
/// Hook thread -> main window when Alt+F4 starts/stops being held. wparam = 1/0.
const WM_APP_EXIT_HOLD: u32 = WM_APP + 4;

const DISPLAY_SECONDS: f64 = 10.0;
const MAX_RECENT: usize = 30;
const DEFAULT_BG: [f64; 3] = [30.0, 30.0, 40.0];
const NIGHT_BG: Rgb = [12, 14, 40];
const GOLDEN_RATIO: f64 = 0.618033988749895;
const FRAME: Duration = Duration::from_micros(16_667);
const MAX_PARTICLES: usize = 800;

const RAINBOW: [Rgb; 7] = [
    [230, 30, 30],
    [255, 140, 0],
    [255, 225, 0],
    [40, 180, 70],
    [30, 110, 255],
    [90, 40, 180],
    [170, 60, 210],
];
/// Number keys: delay before the first dot, then between dots.
const DOT_START: f64 = 0.25;
const DOT_STEP: f64 = 0.3;

static MAIN_HWND: AtomicPtr<c_void> = AtomicPtr::new(null_mut());

// =============================================================================
// Virtual key -> display text
// =============================================================================

fn vk_display(vk: u32) -> Option<String> {
    let s = match vk {
        0x41..=0x5A | 0x30..=0x39 => return Some(char::from(vk as u8).to_string()),
        0x60..=0x69 => return Some((vk - 0x60).to_string()),
        0x70..=0x7B => return Some(format!("F{}", vk - 0x70 + 1)),
        VK_SPACE => "SPACE",
        VK_RETURN => "ENTER",
        0x08 => "BACK",
        0x09 => "TAB",
        VK_ESCAPE => "ESC",
        0x2E => "DEL",
        0x2D => "INS",
        0x24 => "HOME",
        0x23 => "END",
        0x21 => "PGUP",
        0x22 => "PGDN",
        0x25 => "\u{2190}",
        0x26 => "\u{2191}",
        0x27 => "\u{2192}",
        0x28 => "\u{2193}",
        0x6A => "*",
        0x6B => "+",
        0x6D => "-",
        0x6E => ".",
        0x6F => "/",
        0xBA => ";",
        0xBB => "=",
        0xBC => ",",
        0xBD => "-",
        0xBE => ".",
        0xBF => "/",
        0xC0 => "`",
        0xDB => "[",
        0xDC => "\\",
        0xDD => "]",
        0xDE => "'",
        _ => return None,
    };
    Some(s.to_string())
}

// =============================================================================
// Colors
// =============================================================================

type Rgb = [u8; 3];

fn hls_to_rgb(h: f64, l: f64, s: f64) -> Rgb {
    // Same algorithm as Python's colorsys.hls_to_rgb.
    let m2 = if l <= 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let m1 = 2.0 * l - m2;
    let v = |hue: f64| {
        let hue = hue.rem_euclid(1.0);
        let x = if hue < 1.0 / 6.0 {
            m1 + (m2 - m1) * hue * 6.0
        } else if hue < 0.5 {
            m2
        } else if hue < 2.0 / 3.0 {
            m1 + (m2 - m1) * (2.0 / 3.0 - hue) * 6.0
        } else {
            m1
        };
        (x * 255.0) as u8
    };
    [v(h + 1.0 / 3.0), v(h), v(h - 1.0 / 3.0)]
}

/// Stable number for a key label: its code point if one character, else a hash.
fn key_index(text: &str) -> u64 {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c as u64,
        _ => {
            // FNV-1a: stable across runs (Python's hash() was randomized).
            let mut h: u64 = 0xcbf29ce484222325;
            for b in text.bytes() {
                h = (h ^ b as u64).wrapping_mul(0x100000001b3);
            }
            h % 1_000_003
        }
    }
}

/// Vibrant, deterministic color for a key label.
fn char_to_color(text: &str) -> Rgb {
    hls_to_rgb((key_index(text) as f64 * GOLDEN_RATIO) % 1.0, 0.55, 0.85)
}

fn vivid(hue: f64) -> Rgb {
    hls_to_rgb(hue, 0.6, 0.95)
}

fn text_color_for_bg(bg: Rgb) -> Rgb {
    let brightness = (bg[0] as u32 * 299 + bg[1] as u32 * 587 + bg[2] as u32 * 114) / 1000;
    if brightness < 140 { [255, 255, 255] } else { [20, 20, 20] }
}

fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f64 + (b[i] as f64 - a[i] as f64) * t) as u8)
}

fn ease_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

fn colorref(c: Rgb) -> COLORREF {
    c[0] as u32 | (c[1] as u32) << 8 | (c[2] as u32) << 16
}

// =============================================================================
// Sounds: synthesized at startup (no sound files), played by a small mixer
// =============================================================================
//
// A dedicated audio thread keeps one waveOut device open and continuously
// streams short buffers. Each buffer is the sum of every sound currently
// playing, so sounds overlap instead of cutting each other off, and a new
// sound starts within ~50ms.

const SAMPLE_RATE: u32 = 44100;
const BUF_SAMPLES: usize = 1024; // ~23ms per buffer
const NUM_BUFS: usize = 3;
const MAX_VOICES: usize = 12;

// Clip indexes (see `make_clips`).
const CLIP_NOTES: usize = 0; // 10 notes: CLIP_NOTES..CLIP_NOTES + 10
const CLIP_SPACE: usize = 10;
const CLIP_LAUNCH: usize = 11;
const CLIP_CLICK: usize = 12;
const CLIP_POPS: usize = 13; // 5 pops: CLIP_POPS..CLIP_POPS + 5
const NUM_POPS: usize = 5;

/// Major pentatonic scale from C4: any combination of notes sounds pleasant.
fn pentatonic(step: usize) -> f64 {
    let semis = [0, 2, 4, 7, 9][step % 5] + 12 * (step / 5);
    261.63 * 2f64.powf(semis as f64 / 12.0)
}

fn fade_out(buf: &mut [f32]) {
    let fade = (0.02 * SAMPLE_RATE as f64) as usize;
    let n = buf.len();
    for i in 0..fade.min(n) {
        buf[n - 1 - i] *= i as f32 / fade as f32;
    }
}

/// Bell-like notes: (start seconds, frequency Hz), mixed into one clip.
fn chime(notes: &[(f64, f64)], seconds: f64, decay: f64, volume: f64) -> Vec<f32> {
    let rate = SAMPLE_RATE as f64;
    let mut buf = vec![0f32; (seconds * rate) as usize];
    for &(t0, freq) in notes {
        for (i, sample) in buf.iter_mut().enumerate().skip((t0 * rate) as usize) {
            let t = i as f64 / rate - t0;
            let env = (t / 0.004).min(1.0) * (-t * decay).exp();
            let tone = (TAU * freq * t).sin() + 0.3 * (TAU * 2.0 * freq * t).sin() * (-t * decay).exp();
            *sample += (tone * env * volume) as f32;
        }
    }
    fade_out(&mut buf);
    buf
}

/// Soft rising whistle for a firework launch.
fn whistle(seconds: f64, from: f64, to: f64, volume: f64) -> Vec<f32> {
    let rate = SAMPLE_RATE as f64;
    let n = (seconds * rate) as usize;
    let mut phase = 0.0;
    let mut buf = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / n as f64;
        phase += TAU * (from + (to - from) * t * t) / rate;
        let env = (t * 8.0).min(1.0) * (1.0 - t);
        buf.push((phase.sin() * env * volume) as f32);
    }
    fade_out(&mut buf);
    buf
}

fn make_clips() -> Vec<Vec<f32>> {
    let p = pentatonic;
    let mut clips = Vec::new();
    for i in 0..10 {
        clips.push(chime(&[(0.0, p(i))], 0.7, 6.0, 0.25)); // CLIP_NOTES + i
    }
    clips.push(chime(&[(0.0, p(0)), (0.08, p(2)), (0.16, p(4)), (0.24, p(5)), (0.32, p(7))], 1.2, 4.0, 0.2)); // SPACE
    clips.push(whistle(0.55, 500.0, 1300.0, 0.06)); // LAUNCH
    clips.push(chime(&[(0.0, p(10))], 0.25, 18.0, 0.25)); // CLICK
    for i in 0..NUM_POPS {
        let f = p(7 + i);
        clips.push(chime(&[(0.0, f), (0.05, f * 1.5), (0.1, f * 2.0)], 0.9, 7.0, 0.13)); // POPS
    }
    clips
}

struct Sounds {
    enabled: bool,
    tx: std::sync::mpsc::Sender<usize>,
}

impl Sounds {
    fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let clips = make_clips();
        std::thread::spawn(move || unsafe { audio_thread(clips, rx) });
        Sounds { enabled: true, tx }
    }

    fn play(&self, clip: usize) {
        if self.enabled {
            let _ = self.tx.send(clip); // fails only if no audio device; then silent
        }
    }

    fn for_key(text: &str) -> Option<usize> {
        match text {
            "SPACE" => Some(CLIP_SPACE),
            "ENTER" => None, // the fireworks make their own sounds
            "CLICK" | "MIDDLE" | "RIGHT" => Some(CLIP_CLICK),
            _ => Some(CLIP_NOTES + (key_index(text) % 10) as usize),
        }
    }
}

unsafe fn audio_thread(clips: Vec<Vec<f32>>, rx: std::sync::mpsc::Receiver<usize>) {
    use std::sync::mpsc::TryRecvError;
    unsafe {
        let event = CreateEventW(null(), 0, 0, null());
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 1,
            nSamplesPerSec: SAMPLE_RATE,
            nAvgBytesPerSec: SAMPLE_RATE * 2,
            nBlockAlign: 2,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut hwo: HWAVEOUT = zeroed();
        if waveOutOpen(&mut hwo, WAVE_MAPPER, &fmt, event as usize, 0, CALLBACK_EVENT) != 0 {
            return; // no audio device: run silently
        }

        let hdr_size = size_of::<WAVEHDR>() as u32;
        let mut data: Vec<Vec<i16>> = (0..NUM_BUFS).map(|_| vec![0i16; BUF_SAMPLES]).collect();
        let mut hdrs: Vec<WAVEHDR> = (0..NUM_BUFS).map(|_| zeroed()).collect();
        for (hdr, buf) in hdrs.iter_mut().zip(data.iter_mut()) {
            hdr.lpData = buf.as_mut_ptr() as *mut u8;
            hdr.dwBufferLength = (BUF_SAMPLES * 2) as u32;
            waveOutPrepareHeader(hwo, hdr, hdr_size);
            hdr.dwFlags |= WHDR_DONE; // "free" so the loop fills it right away
        }

        let mut voices: Vec<(usize, usize)> = Vec::new(); // (clip, position)
        let mut mix = vec![0f32; BUF_SAMPLES];
        loop {
            for i in 0..NUM_BUFS {
                if hdrs[i].dwFlags & WHDR_DONE == 0 {
                    continue; // still playing
                }
                loop {
                    match rx.try_recv() {
                        Ok(clip) if clip < clips.len() => {
                            if voices.len() >= MAX_VOICES {
                                voices.remove(0);
                            }
                            voices.push((clip, 0));
                        }
                        Ok(_) => {}
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            waveOutReset(hwo);
                            waveOutClose(hwo);
                            return;
                        }
                    }
                }

                mix.fill(0.0);
                for (clip, pos) in &mut voices {
                    let samples = &clips[*clip][*pos..];
                    let n = samples.len().min(BUF_SAMPLES);
                    for (m, s) in mix.iter_mut().zip(&samples[..n]) {
                        *m += s;
                    }
                    *pos += n;
                }
                voices.retain(|&(clip, pos)| pos < clips[clip].len());

                for (out, &x) in data[i].iter_mut().zip(&mix) {
                    // Soft limiter: linear when quiet, never harshly clips.
                    let x = x.clamp(-3.0, 3.0);
                    let y = x * (27.0 + x * x) / (27.0 + 9.0 * x * x);
                    *out = (y * 32000.0) as i16;
                }
                hdrs[i].dwFlags &= !WHDR_DONE;
                waveOutWrite(hwo, &mut hdrs[i], hdr_size);
            }
            WaitForSingleObject(event, 50);
        }
    }
}

// =============================================================================
// Keyboard hook (runs on its own thread with its own message loop)
// =============================================================================

// Key state is tracked here rather than via GetAsyncKeyState, because we
// swallow every key, so Windows' own key state never sees them go down.
static KEY_DOWN: [AtomicBool; 256] = [const { AtomicBool::new(false) }; 256];
static EXIT_HOLDING: AtomicBool = AtomicBool::new(false);

fn is_down(vk: u32) -> bool {
    KEY_DOWN[(vk & 0xFF) as usize].load(Ordering::Relaxed)
}

fn alt_down() -> bool {
    is_down(VK_MENU) || is_down(VK_LMENU) || is_down(VK_RMENU)
}

unsafe extern "system" fn kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if code != HC_ACTION as i32 {
            return CallNextHookEx(null_mut(), code, wparam, lparam);
        }
        let kb = &*(lparam as *const KBDLLHOOKSTRUCT);
        let vk = kb.vkCode;
        let msg = wparam as u32;
        let hwnd = MAIN_HWND.load(Ordering::Relaxed);
        let modifier = MODIFIER_VKS.contains(&vk);

        if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
            let repeat = KEY_DOWN[(vk & 0xFF) as usize].swap(true, Ordering::Relaxed);
            if !modifier {
                PostMessageW(hwnd, WM_APP_KEY, vk as usize, repeat as isize);
            }
        } else if msg == WM_KEYUP || msg == WM_SYSKEYUP {
            KEY_DOWN[(vk & 0xFF) as usize].store(false, Ordering::Relaxed);
            if !modifier {
                PostMessageW(hwnd, WM_APP_KEYUP, vk as usize, 0);
            }
        }

        let holding = alt_down() && is_down(VK_F4);
        if holding != EXIT_HOLDING.swap(holding, Ordering::Relaxed) {
            PostMessageW(hwnd, WM_APP_EXIT_HOLD, holding as usize, 0);
        }

        // Swallow everything: Win key, Alt+Tab, Ctrl+Esc, Ctrl+Shift+Esc,
        // media/browser/mail/calculator/sleep keys, Sticky Keys triggers...
        1
    }
}

unsafe fn install_hook() -> HHOOK {
    unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), GetModuleHandleW(null()), 0) }
}

fn spawn_hook_thread() {
    std::thread::spawn(|| unsafe {
        let mut hook = install_hook();
        if hook.is_null() {
            let err = GetLastError();
            PostMessageW(MAIN_HWND.load(Ordering::Relaxed), WM_APP_HOOK_FAILED, err as usize, 0);
            return;
        }
        // Windows silently removes a low-level hook that ever responds too
        // slowly. Periodically install a fresh one *before* removing the old
        // one, so there is never a gap.
        SetTimer(null_mut(), 0, 2000, None);
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if msg.message == WM_TIMER {
                let fresh = install_hook();
                if !fresh.is_null() {
                    UnhookWindowsHookEx(hook);
                    hook = fresh;
                }
                continue;
            }
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        UnhookWindowsHookEx(hook);
    });
}

// =============================================================================
// Accessibility hotkeys (Shift x5 = Sticky Keys dialog, etc.)
// =============================================================================

struct AccessibilitySaved {
    sticky: STICKYKEYS,
    toggle: TOGGLEKEYS,
    filter: FILTERKEYS,
}

unsafe fn spi<T>(action: u32, value: &mut T) {
    unsafe {
        SystemParametersInfoW(action, size_of::<T>() as u32, value as *mut T as *mut c_void, 0);
    }
}

/// Turn off the accessibility *shortcut* hotkeys, unless the feature is
/// already on (then leave the user's setting alone). Changes are in-memory
/// only (not saved to the profile), so even a crash is undone at next logon.
unsafe fn disable_accessibility_hotkeys() -> AccessibilitySaved {
    unsafe {
        let mut sticky = STICKYKEYS { cbSize: size_of::<STICKYKEYS>() as u32, dwFlags: 0 };
        let mut toggle = TOGGLEKEYS { cbSize: size_of::<TOGGLEKEYS>() as u32, dwFlags: 0 };
        let mut filter: FILTERKEYS = zeroed();
        filter.cbSize = size_of::<FILTERKEYS>() as u32;
        spi(SPI_GETSTICKYKEYS, &mut sticky);
        spi(SPI_GETTOGGLEKEYS, &mut toggle);
        spi(SPI_GETFILTERKEYS, &mut filter);
        let saved = AccessibilitySaved { sticky, toggle, filter };

        if sticky.dwFlags & SKF_STICKYKEYSON == 0 {
            sticky.dwFlags &= !(SKF_HOTKEYACTIVE | SKF_CONFIRMHOTKEY);
            spi(SPI_SETSTICKYKEYS, &mut sticky);
        }
        if toggle.dwFlags & TKF_TOGGLEKEYSON == 0 {
            toggle.dwFlags &= !(TKF_HOTKEYACTIVE | TKF_CONFIRMHOTKEY);
            spi(SPI_SETTOGGLEKEYS, &mut toggle);
        }
        if filter.dwFlags & FKF_FILTERKEYSON == 0 {
            filter.dwFlags &= !(FKF_HOTKEYACTIVE | FKF_CONFIRMHOTKEY);
            spi(SPI_SETFILTERKEYS, &mut filter);
        }
        saved
    }
}

unsafe fn restore_accessibility_hotkeys(saved: &AccessibilitySaved) {
    unsafe {
        let (mut s, mut t, mut f) = (saved.sticky, saved.toggle, saved.filter);
        spi(SPI_SETSTICKYKEYS, &mut s);
        spi(SPI_SETTOGGLEKEYS, &mut t);
        spi(SPI_SETFILTERKEYS, &mut f);
    }
}

// =============================================================================
// Window
// =============================================================================

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe fn clip_cursor_to(hwnd: HWND) {
    unsafe {
        let mut rc: RECT = zeroed();
        GetWindowRect(hwnd, &mut rc);
        ClipCursor(&rc);
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SETCURSOR => {
                SetCursor(null_mut());
                1
            }
            WM_ERASEBKGND => 1,
            WM_PAINT => {
                // Frames are drawn by the main loop; just validate.
                let mut ps: PAINTSTRUCT = zeroed();
                BeginPaint(hwnd, &mut ps);
                EndPaint(hwnd, &ps);
                0
            }
            WM_ACTIVATE => {
                clip_cursor_to(hwnd);
                0
            }
            WM_CLOSE => {
                DestroyWindow(hwnd);
                0
            }
            WM_DESTROY => {
                ClipCursor(null());
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe fn create_window(sw: i32, sh: i32) -> HWND {
    unsafe {
        let hinst = GetModuleHandleW(null());
        let class_name = wide("SmashWindow");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinst,
            lpszClassName: class_name.as_ptr(),
            ..zeroed()
        };
        RegisterClassW(&wc);
        let title = wide("Smash!");
        CreateWindowExW(
            WS_EX_TOPMOST,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_POPUP | WS_VISIBLE,
            0,
            0,
            sw,
            sh,
            null_mut(),
            null_mut(),
            hinst,
            null(),
        )
    }
}

// =============================================================================
// Drawing helpers (GDI into an off-screen bitmap)
// =============================================================================

unsafe fn make_font(px: i32) -> HFONT {
    unsafe {
        let face = wide("Arial");
        CreateFontW(
            -px, // negative = em height in pixels
            0,
            0,
            0,
            FW_BOLD as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_TT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            ANTIALIASED_QUALITY as u32,
            (DEFAULT_PITCH | FF_SWISS) as u32,
            face.as_ptr(),
        )
    }
}

unsafe fn fill_rect(dc: HDC, x: i32, y: i32, w: i32, h: i32, color: Rgb) {
    unsafe {
        let brush = CreateSolidBrush(colorref(color));
        let rc = RECT { left: x, top: y, right: x + w, bottom: y + h };
        FillRect(dc, &rc, brush);
        DeleteObject(brush);
    }
}

/// Run `f` with a solid brush of `color` and no outline selected.
unsafe fn with_brush(dc: HDC, color: Rgb, f: impl FnOnce()) {
    unsafe {
        let brush = CreateSolidBrush(colorref(color));
        let old_brush = SelectObject(dc, brush);
        let old_pen = SelectObject(dc, GetStockObject(NULL_PEN));
        f();
        SelectObject(dc, old_pen);
        SelectObject(dc, old_brush);
        DeleteObject(brush);
    }
}

unsafe fn round_rect(dc: HDC, x: i32, y: i32, w: i32, h: i32, radius: i32, color: Rgb) {
    unsafe {
        // With a null pen the shape is one pixel smaller, so add 1.
        with_brush(dc, color, || {
            RoundRect(dc, x, y, x + w + 1, y + h + 1, radius * 2, radius * 2);
        });
    }
}

unsafe fn circle(dc: HDC, cx: f64, cy: f64, r: f64, color: Rgb) {
    if r < 0.5 {
        return;
    }
    unsafe {
        with_brush(dc, color, || {
            Ellipse(dc, (cx - r) as i32, (cy - r) as i32, (cx + r) as i32 + 1, (cy + r) as i32 + 1);
        });
    }
}

/// Filled upper half of a circle (a rainbow band).
unsafe fn half_disc(dc: HDC, cx: f64, cy: f64, r: f64, color: Rgb) {
    if r < 1.0 {
        return;
    }
    let (l, t, rr, b) = ((cx - r) as i32, (cy - r) as i32, (cx + r) as i32, (cy + r) as i32);
    unsafe {
        with_brush(dc, color, || {
            // Counter-clockwise from the right edge to the left edge = over the top.
            Pie(dc, l, t, rr + 1, b + 1, rr, cy as i32, l, cy as i32);
        });
    }
}

unsafe fn star(dc: HDC, cx: f64, cy: f64, r: f64, rot: f64, color: Rgb) {
    if r < 0.5 {
        return;
    }
    let mut pts = [POINT { x: 0, y: 0 }; 10];
    for (k, p) in pts.iter_mut().enumerate() {
        let rr = if k % 2 == 0 { r } else { r * 0.45 };
        let a = rot + k as f64 * PI / 5.0 - FRAC_PI_2;
        *p = POINT { x: (cx + rr * a.cos()) as i32, y: (cy + rr * a.sin()) as i32 };
    }
    unsafe {
        with_brush(dc, color, || {
            Polygon(dc, pts.as_ptr(), 10);
        });
    }
}

unsafe fn text_centered(dc: HDC, text: &str, font: HFONT, color: Rgb, cx: i32, cy: i32) {
    unsafe {
        let mut w: Vec<u16> = text.encode_utf16().collect();
        let old = SelectObject(dc, font);
        SetTextColor(dc, colorref(color));
        let mut rc = RECT { left: cx - 20000, top: cy - 20000, right: cx + 20000, bottom: cy + 20000 };
        DrawTextW(
            dc,
            w.as_mut_ptr(),
            w.len() as i32,
            &mut rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_NOCLIP,
        );
        SelectObject(dc, old);
    }
}

// =============================================================================
// Particles
// =============================================================================

struct Rng(u64);

impl Rng {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x1234_5678);
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        // xorshift64
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f()
    }
    fn chance(&mut self, p: f64) -> bool {
        self.f() < p
    }
}

#[derive(Clone, Copy)]
struct Particle {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    /// Negative age = waiting to launch (used to stagger fireworks).
    age: f64,
    life: f64,
    size: f64,
    color: Rgb,
    star: bool,
    spin: f64,
    /// Multiplier on gravity (0 = floats).
    gravity: f64,
    /// Rockets fly up and explode into a burst when their life runs out.
    rocket: bool,
}

// =============================================================================
// App state
// =============================================================================

struct Menu {
    open: bool,
    selected: usize,
    last_used: Instant,
}

const MENU_ITEMS: usize = 2; // 0 = sound toggle, 1 = quit

struct App {
    sw: i32,
    sh: i32,
    current: Option<String>,
    char_time: Instant,
    bg: [f64; 3],
    target_bg: [f64; 3],
    recent: VecDeque<(String, Rgb)>,
    scale: f64,
    rng: Rng,
    particles: Vec<Particle>,
    sounds: Sounds,
    menu: Menu,
    quit: bool,

    /// Number keys: how many counting dots have popped in so far.
    dots_shown: usize,
    /// Rainbow (SPACE): current radius and the radius it eases toward.
    rainbow_r: f64,
    rainbow_target: f64,
    space_held: bool,
    /// Fireworks (ENTER): holding it keeps launching rockets.
    enter_held: bool,
    next_rocket: Instant,

    /// When Alt+F1 started being held (open menu), if it is.
    f1_hold: Option<Instant>,
    /// When Alt+F4 started being held (exit), if it is.
    exit_hold: Option<Instant>,

    last_input: Instant,
    /// 0 = awake, 1 = fully faded into the night screen.
    night: f64,
    start: Instant,
    /// Night sky stars: (x fraction, y fraction, size fraction, twinkle phase).
    sky: Vec<[f64; 4]>,

    idle_font: HFONT,
    ticker_font: HFONT,
    small_font: HFONT,
}

impl App {
    fn new(sw: i32, sh: i32) -> Self {
        let mut rng = Rng::new();
        let mut sky = Vec::new();
        while sky.len() < 70 {
            let (x, y) = (rng.f(), rng.range(0.03, 0.85));
            let near_text = (x - 0.5).abs() < 0.3 && (y - 0.5).abs() < 0.12;
            let near_moon = (x - 0.78).abs() < 0.12 && (y - 0.22).abs() < 0.15;
            if !near_text && !near_moon {
                sky.push([x, y, rng.range(0.004, 0.012), rng.range(0.0, TAU)]);
            }
        }
        let now = Instant::now();
        unsafe {
            App {
                sw,
                sh,
                current: None,
                char_time: now,
                bg: DEFAULT_BG,
                target_bg: DEFAULT_BG,
                recent: VecDeque::new(),
                scale: 1.0,
                rng,
                particles: Vec::new(),
                sounds: Sounds::new(),
                menu: Menu { open: false, selected: 0, last_used: now },
                quit: false,
                dots_shown: 0,
                rainbow_r: 0.0,
                rainbow_target: 0.0,
                space_held: false,
                enter_held: false,
                next_rocket: now,
                f1_hold: None,
                exit_hold: None,
                last_input: now,
                night: 0.0,
                start: now,
                sky,
                idle_font: make_font(sh / 12),
                ticker_font: make_font(sh / 22),
                small_font: make_font(sh / 34),
            }
        }
    }

    fn sh(&self) -> f64 {
        self.sh as f64
    }

    fn text_center(&self) -> (f64, f64) {
        (self.sw as f64 / 2.0, (self.sh / 2 - self.sh / 12) as f64)
    }

    /// If the current key is a single digit, its value.
    fn digit(&self) -> Option<usize> {
        let mut chars = self.current.as_deref()?.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => c.to_digit(10).map(|d| d as usize),
            _ => None,
        }
    }

    fn dot_pos(&self, i: usize, n: usize) -> (f64, f64, f64) {
        let d = self.sh() / 13.0;
        let gap = d * 0.45;
        let total = n as f64 * d + (n as f64 - 1.0) * gap;
        let x = (self.sw as f64 - total) / 2.0 + i as f64 * (d + gap) + d / 2.0;
        (x, self.sh() * 0.80, d / 2.0)
    }

    // ---- Input -------------------------------------------------------------

    fn key_down(&mut self, vk: u32, repeat: bool) {
        self.last_input = Instant::now();
        if vk == VK_F1 && !repeat && alt_down() && !self.menu.open {
            self.f1_hold = Some(Instant::now());
        }
        if self.menu.open {
            self.menu_key(vk, repeat);
            return;
        }
        if vk == VK_SPACE && !repeat {
            self.space_held = true;
        }
        if vk == VK_RETURN && !repeat {
            self.enter_held = true;
        }
        let Some(text) = vk_display(vk) else { return };
        if repeat {
            // Holding a key keeps it on screen instead of re-triggering it.
            if self.current.as_deref() == Some(text.as_str()) {
                self.char_time = Instant::now();
            }
            return;
        }
        self.smash(text, None);
    }

    fn key_up(&mut self, vk: u32) {
        match vk {
            VK_F1 => self.f1_hold = None,
            VK_SPACE => self.space_held = false,
            VK_RETURN => self.enter_held = false,
            _ => {}
        }
    }

    fn menu_key(&mut self, vk: u32, repeat: bool) {
        self.menu.last_used = Instant::now();
        match vk {
            VK_UP => self.menu.selected = (self.menu.selected + MENU_ITEMS - 1) % MENU_ITEMS,
            VK_DOWN => self.menu.selected = (self.menu.selected + 1) % MENU_ITEMS,
            VK_RETURN | VK_SPACE if !repeat => match self.menu.selected {
                0 => {
                    self.sounds.enabled = !self.sounds.enabled;
                    self.sounds.play(CLIP_NOTES + 4);
                }
                _ => self.quit = true,
            },
            VK_ESCAPE => self.menu.open = false,
            _ => {}
        }
    }

    fn click(&mut self, name: &str, x: f64, y: f64) {
        self.last_input = Instant::now();
        if !self.menu.open {
            self.smash(name.to_string(), Some((x, y)));
        }
    }

    /// A fresh key press or click. `at`: where it happened (clicks).
    fn smash(&mut self, text: String, at: Option<(f64, f64)>) {
        let color = char_to_color(&text);
        self.target_bg = color.map(|c| c as f64);
        self.scale = 1.4; // pop-in effect
        self.char_time = Instant::now();
        self.dots_shown = 0;
        if let Some(clip) = Sounds::for_key(&text) {
            self.sounds.play(clip);
        }
        self.recent.push_back((text.clone(), color));
        if self.recent.len() > MAX_RECENT {
            self.recent.pop_front();
        }

        match text.as_str() {
            "ENTER" => {
                self.fireworks(3);
                self.next_rocket = Instant::now() + Duration::from_millis(800);
            }
            "SPACE" => {
                self.rainbow_r = 0.0;
                self.rainbow_target = self.sh() * 0.3;
            }
            _ if self.digit_of(&text).is_some() => {} // the dots are the show
            _ => {
                let (x, y) = at.unwrap_or_else(|| self.text_center());
                self.sparkle_ring(x, y, color);
            }
        }
        self.current = Some(text);
    }

    fn digit_of(&self, text: &str) -> Option<u32> {
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => c.to_digit(10),
            _ => None,
        }
    }

    // ---- Effects -----------------------------------------------------------

    /// A small, tidy ring of stars that floats outward behind the letter.
    fn sparkle_ring(&mut self, x: f64, y: f64, base: Rgb) {
        let sh = self.sh();
        let count = 12;
        let light = mix(base, [255, 255, 255], 0.6);
        let offset = self.rng.range(0.0, TAU);
        for i in 0..count {
            let a = offset + i as f64 * TAU / count as f64;
            let speed = sh * 0.9;
            self.push(Particle {
                x: x + a.cos() * sh * 0.12,
                y: y + a.sin() * sh * 0.12,
                vx: a.cos() * speed,
                vy: a.sin() * speed,
                age: 0.0,
                life: 0.7,
                size: sh / 55.0,
                color: light,
                star: true,
                spin: 3.0,
                gravity: 0.0,
                rocket: false,
            });
        }
    }

    /// `n` rockets spread across the screen, launched one after another.
    fn fireworks(&mut self, n: usize) {
        for i in 0..n {
            let x = (0.15 + 0.7 * (i as f64 + self.rng.range(0.2, 0.8)) / n as f64) * self.sw as f64;
            self.rocket(x, i as f64 * 0.25);
        }
    }

    fn rocket(&mut self, x: f64, delay: f64) {
        let sh = self.sh();
        let r = &mut self.rng;
        let p = Particle {
            x,
            y: sh,
            vx: sh * r.range(-0.05, 0.05),
            vy: -sh * r.range(1.3, 1.6),
            age: -delay - 0.001, // negative: waiting; launch sound plays at 0
            life: r.range(0.75, 0.95),
            size: sh / 110.0,
            color: [255, 240, 200],
            star: false,
            spin: 0.0,
            gravity: 1.0,
            rocket: true,
        };
        self.push(p);
    }

    fn explode(&mut self, x: f64, y: f64) {
        let sh = self.sh();
        let hue = self.rng.f();
        let pop = CLIP_POPS + (self.rng.next() % NUM_POPS as u64) as usize;
        self.sounds.play(pop);
        for _ in 0..60 {
            let r = &mut self.rng;
            let a = r.range(0.0, TAU);
            let s = sh * 0.8 * r.range(0.4, 1.0);
            let color = if r.chance(0.2) { [255, 255, 255] } else { vivid(hue + r.range(-0.05, 0.05)) };
            let p = Particle {
                x,
                y,
                vx: a.cos() * s,
                vy: a.sin() * s,
                age: 0.0,
                life: r.range(1.0, 1.6),
                size: sh / 90.0 * r.range(0.7, 1.3),
                color,
                star: false,
                spin: 0.0,
                gravity: 0.35,
                rocket: false,
            };
            self.push(p);
        }
    }

    fn push(&mut self, p: Particle) {
        if self.particles.len() < MAX_PARTICLES {
            self.particles.push(p);
        }
    }

    // ---- Update ------------------------------------------------------------

    fn update(&mut self, dt: f64) {
        let sh = self.sh();

        // Grown-up holds: Alt+F1 opens the menu, Alt+F4 quits.
        if !alt_down() {
            self.f1_hold = None;
        }
        if self.f1_hold.is_some_and(|t| t.elapsed() >= MENU_HOLD) {
            self.f1_hold = None;
            self.menu.open = true;
            self.menu.selected = 0;
            self.menu.last_used = Instant::now();
        }
        if self.menu.open && self.menu.last_used.elapsed() >= MENU_TIMEOUT {
            self.menu.open = false;
        }
        if self.exit_hold.is_some_and(|t| t.elapsed() >= EXIT_HOLD) {
            self.quit = true;
        }

        if self.current.is_some() && self.char_time.elapsed().as_secs_f64() > DISPLAY_SECONDS {
            self.current = None;
            self.target_bg = DEFAULT_BG;
        }
        let t = (dt * 6.0).min(1.0);
        for i in 0..3 {
            self.bg[i] += (self.target_bg[i] - self.bg[i]) * t;
        }
        if self.scale > 1.0 {
            self.scale = (self.scale - dt * 2.5).max(1.0);
        }

        // Rainbow: holding SPACE keeps it growing (and on screen).
        if self.current.as_deref() == Some("SPACE") {
            if self.space_held {
                self.rainbow_target = (self.rainbow_target + sh * 0.22 * dt).min(sh * 0.7);
                self.char_time = Instant::now();
            }
            self.rainbow_r += (self.rainbow_target - self.rainbow_r) * (dt * 5.0).min(1.0);
        }

        // Fireworks: holding ENTER keeps launching rockets.
        if self.current.as_deref() == Some("ENTER") && self.enter_held {
            self.char_time = Instant::now();
            if Instant::now() >= self.next_rocket {
                self.next_rocket = Instant::now() + Duration::from_millis(350);
                let x = self.rng.range(0.12, 0.88) * self.sw as f64;
                self.rocket(x, 0.0);
            }
        }

        // Number keys: pop in one counting dot at a time.
        if let Some(n) = self.digit() {
            let age = self.char_time.elapsed().as_secs_f64();
            self.dots_shown = (0..n).filter(|&i| age >= DOT_START + i as f64 * DOT_STEP).count();
        }

        // Particles.
        let gravity = sh * 1.2;
        let drag = (1.0 - 2.5 * dt).max(0.0);
        let mut explode = Vec::new();
        let mut launches = 0;
        for p in &mut self.particles {
            let waiting = p.age < 0.0;
            p.age += dt;
            if p.age < 0.0 {
                continue;
            }
            if waiting && p.rocket {
                launches += 1;
            }
            p.vy += gravity * p.gravity * dt;
            if !p.rocket {
                p.vx *= drag;
                p.vy *= drag;
            }
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            if p.rocket && p.age >= p.life {
                explode.push((p.x, p.y));
            }
        }
        self.particles.retain(|p| p.age < p.life);
        for _ in 0..launches {
            self.sounds.play(CLIP_LAUNCH);
        }
        for (x, y) in explode {
            self.explode(x, y);
        }

        // Fade into / out of the night screen.
        if self.last_input.elapsed() >= SLEEP_AFTER {
            self.night = (self.night + dt / 5.0).min(1.0);
        } else {
            self.night = (self.night - dt * 3.0).max(0.0);
        }
    }

    // ---- Drawing -----------------------------------------------------------

    unsafe fn draw(&self, dc: HDC) {
        unsafe {
            let (sw, sh) = (self.sw, self.sh);
            let bg = mix(self.bg.map(|c| c as u8), NIGHT_BG, self.night);
            let fg = text_color_for_bg(bg);
            fill_rect(dc, 0, 0, sw, sh, bg);
            SetBkMode(dc, TRANSPARENT as i32);

            if self.current.as_deref() == Some("SPACE") {
                self.draw_rainbow(dc, bg);
            }

            // Particles go behind the letter so it always reads clearly.
            self.draw_particles(dc, bg);

            if let Some(text) = &self.current {
                let age = self.char_time.elapsed().as_secs_f64();
                if text != "SPACE" {
                    let size = ((sh / 2) as f64 * self.scale) as i32;
                    let font = make_font(size.clamp(10, sh));
                    text_centered(dc, text, font, fg, sw / 2, sh / 2 - sh / 12);
                    DeleteObject(font);
                }

                if let Some(n) = self.digit() {
                    for i in 0..self.dots_shown {
                        let (x, y, r) = self.dot_pos(i, n);
                        let since = age - (DOT_START + i as f64 * DOT_STEP);
                        let pop = 1.0 + 0.5 * (1.0 - since / 0.25).max(0.0);
                        circle(dc, x, y, r * pop, fg);
                    }
                }

                // Timer bar at top.
                let progress = (1.0 - age / DISPLAY_SECONDS).max(0.0);
                let bar = bg.map(|c| c.saturating_add(40));
                fill_rect(dc, 0, 0, (sw as f64 * progress) as i32, 6, bar);
            } else {
                let idle = mix([100, 100, 120], bg, self.night);
                text_centered(dc, "SMASH THE KEYBOARD!", self.idle_font, idle, sw / 2, sh / 2);
            }

            if self.night > 0.0 {
                self.draw_night(dc, bg);
            }
            if self.night < 0.3 {
                self.draw_ticker(dc);
            }
            if self.menu.open {
                self.draw_menu(dc);
            }

            // Hold-to-activate hints (only after a moment, so taps don't flash them).
            if let Some(t) = self.exit_hold {
                self.draw_hold_hint(dc, "Keep holding Alt+F4 to quit", t.elapsed().as_secs_f64() / EXIT_HOLD.as_secs_f64());
            } else if let Some(t) = self.f1_hold.filter(|t| t.elapsed().as_secs_f64() > 0.25) {
                self.draw_hold_hint(dc, "Keep holding Alt+F1 for menu", t.elapsed().as_secs_f64() / MENU_HOLD.as_secs_f64());
            }
        }
    }

    unsafe fn draw_particles(&self, dc: HDC, bg: Rgb) {
        unsafe {
            for p in &self.particles {
                if p.age < 0.0 {
                    continue;
                }
                if p.rocket {
                    // Bright head with a fading trail.
                    for k in 0..4 {
                        let k = k as f64;
                        let c = mix(p.color, bg, k * 0.25);
                        circle(dc, p.x - p.vx * 0.015 * k, p.y - p.vy * 0.015 * k, p.size * (1.0 - k * 0.2), c);
                    }
                    continue;
                }
                let t = p.age / p.life;
                let r = p.size * (1.0 - t * t);
                let c = mix(p.color, bg, t * 0.7);
                if p.star {
                    star(dc, p.x, p.y, r * 1.6, p.age * p.spin, c);
                } else {
                    circle(dc, p.x, p.y, r, c);
                }
            }
        }
    }

    /// SPACE: a rainbow rising out of two clouds.
    unsafe fn draw_rainbow(&self, dc: HDC, bg: Rgb) {
        unsafe {
            let (cx, cy) = (self.sw as f64 / 2.0, self.sh() * 0.72);
            let big = self.rainbow_r;
            let band = big / 11.0;
            for (i, &c) in RAINBOW.iter().enumerate() {
                half_disc(dc, cx, cy, big - i as f64 * band, c);
            }
            half_disc(dc, cx, cy, big - RAINBOW.len() as f64 * band, bg);
            let puff = band * 1.6;
            let foot = big - RAINBOW.len() as f64 * band / 2.0;
            let white = [255, 255, 255];
            for side in [-1.0, 1.0] {
                let fx = cx + side * foot;
                circle(dc, fx - puff, cy, puff, white);
                circle(dc, fx + puff, cy, puff, white);
                circle(dc, fx, cy - puff * 0.7, puff * 1.2, white);
                circle(dc, fx, cy + puff * 0.3, puff, white);
            }
        }
    }

    /// Sleepy screen: twinkling stars, a crescent moon, "Bye bye!".
    unsafe fn draw_night(&self, dc: HDC, bg: Rgb) {
        unsafe {
            let (swf, shf) = (self.sw as f64, self.sh());
            let n = ease_out(self.night);
            let t = self.start.elapsed().as_secs_f64();

            for &[x, y, size, phase] in &self.sky {
                let tw = 0.55 + 0.45 * (t * 1.7 + phase).sin();
                let c = mix(bg, [255, 250, 225], n * tw);
                star(dc, x * swf, y * shf, size * shf * (0.7 + 0.3 * tw), 0.0, c);
            }

            let (mx, my, mr) = (swf * 0.78, shf * 0.22, shf * 0.09);
            circle(dc, mx, my, mr, mix(bg, [255, 236, 170], n));
            circle(dc, mx + mr * 0.45, my - mr * 0.2, mr * 0.85, bg);

            let bob = (t * 1.2).sin() * shf * 0.01;
            let c = mix(bg, [190, 200, 255], n);
            text_centered(dc, "Bye bye!", self.idle_font, c, self.sw / 2, (shf / 2.0 + bob) as i32);
        }
    }

    unsafe fn draw_ticker(&self, dc: HDC) {
        if self.recent.is_empty() {
            return;
        }
        unsafe {
            let (sw, sh) = (self.sw, self.sh);
            let ticker_y = sh - sh / 10;
            let box_size = sh / 18;
            let gap = 6;
            let fit = (sw / (box_size + gap)) as usize;
            let skip = self.recent.len().saturating_sub(fit);
            let count = (self.recent.len() - skip) as i32;
            let start_x = (sw - (count * (box_size + gap) - gap)) / 2;

            for (i, (text, color)) in self.recent.iter().skip(skip).enumerate() {
                let x = start_x + i as i32 * (box_size + gap);
                round_rect(dc, x, ticker_y, box_size, box_size, 6, *color);
                let label: String = if text.chars().count() <= 2 { text.clone() } else { text.chars().take(1).collect() };
                let c = text_color_for_bg(*color);
                text_centered(dc, &label, self.ticker_font, c, x + box_size / 2, ticker_y + box_size / 2);
            }
        }
    }

    unsafe fn draw_menu(&self, dc: HDC) {
        unsafe {
            let (sw, sh) = (self.sw, self.sh);
            let (w, h) = (sh * 3 / 4, sh / 2);
            let (x, y) = ((sw - w) / 2, (sh - h) / 2);
            round_rect(dc, x - 4, y - 4, w + 8, h + 8, 24, [230, 230, 240]);
            round_rect(dc, x, y, w, h, 20, [28, 28, 40]);
            text_centered(dc, "Menu", self.ticker_font, [255, 255, 255], sw / 2, y + h / 7);

            let sound = if self.sounds.enabled { "Sound: On" } else { "Sound: Off" };
            for (i, label) in [sound, "Quit"].into_iter().enumerate() {
                let iy = y + h * (3 + 2 * i as i32) / 9;
                let ih = h / 7;
                if i == self.menu.selected {
                    round_rect(dc, x + w / 8, iy - ih / 2, w * 3 / 4, ih, 12, [70, 100, 220]);
                }
                text_centered(dc, label, self.ticker_font, [255, 255, 255], sw / 2, iy);
            }

            let help = "\u{2191} \u{2193} choose   \u{2022}   Enter select   \u{2022}   Esc close";
            text_centered(dc, help, self.small_font, [150, 150, 170], sw / 2, y + h * 8 / 9);
        }
    }

    unsafe fn draw_hold_hint(&self, dc: HDC, label: &str, progress: f64) {
        unsafe {
            let (sw, sh) = (self.sw, self.sh);
            let (y, h) = (sh / 24, sh / 14);
            fill_rect(dc, 0, y, sw, h, [0, 0, 0]);
            fill_rect(dc, 0, y + h - 5, (sw as f64 * progress.min(1.0)) as i32, 5, [255, 80, 80]);
            text_centered(dc, label, self.small_font, [255, 255, 255], sw / 2, y + h / 2 - 2);
        }
    }
}

// =============================================================================
// Main
// =============================================================================

fn main() {
    unsafe {
        // Only one copy at a time (a double double-click shouldn't stack two
        // windows and two keyboard hooks). The mutex lives until the process exits.
        let mutex_name = wide(r"Local\KeyboardSmashSingleInstance");
        let _mutex = CreateMutexW(null(), 1, mutex_name.as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }

        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        timeBeginPeriod(1);
        // Keep the display on and the screen saver / lock screen away.
        SetThreadExecutionState(ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
        let saved_accessibility = disable_accessibility_hotkeys();

        let sw = GetSystemMetrics(SM_CXSCREEN);
        let sh = GetSystemMetrics(SM_CYSCREEN);

        let hwnd = create_window(sw, sh);
        MAIN_HWND.store(hwnd, Ordering::Relaxed);
        SetForegroundWindow(hwnd);
        ShowCursor(0);
        clip_cursor_to(hwnd);

        spawn_hook_thread();

        // Off-screen buffer for flicker-free drawing.
        let wdc = GetDC(hwnd);
        let mem_dc = CreateCompatibleDC(wdc);
        let bitmap = CreateCompatibleBitmap(wdc, sw, sh);
        let old_bitmap = SelectObject(mem_dc, bitmap);

        let mut app = App::new(sw, sh);
        let mut last_guard = Instant::now();
        let mut last = Instant::now();
        let mut next_frame = Instant::now();
        let mut msg: MSG = zeroed();

        'main: loop {
            while PeekMessageW(&mut msg, null_mut(), 0, 0, PM_REMOVE) != 0 {
                match msg.message {
                    WM_QUIT => break 'main,
                    WM_APP_KEY => app.key_down(msg.wParam as u32, msg.lParam != 0),
                    WM_APP_KEYUP => app.key_up(msg.wParam as u32),
                    WM_APP_EXIT_HOLD => app.exit_hold = (msg.wParam != 0).then(Instant::now),
                    WM_APP_HOOK_FAILED => {
                        let text = wide(&format!(
                            "Could not install the keyboard hook (error {}).\n\
                             Try running as Administrator.",
                            msg.wParam
                        ));
                        let caption = wide("Smash!");
                        ClipCursor(null());
                        ShowCursor(1);
                        ShowWindow(hwnd, SW_HIDE);
                        MessageBoxW(null_mut(), text.as_ptr(), caption.as_ptr(), MB_ICONERROR);
                        break 'main;
                    }
                    WM_LBUTTONDOWN | WM_MBUTTONDOWN | WM_RBUTTONDOWN => {
                        let name = match msg.message {
                            WM_MBUTTONDOWN => "MIDDLE",
                            WM_RBUTTONDOWN => "RIGHT",
                            _ => "CLICK",
                        };
                        let x = (msg.lParam & 0xFFFF) as i16 as f64;
                        let y = ((msg.lParam >> 16) & 0xFFFF) as i16 as f64;
                        app.click(name, x, y);
                    }
                    _ => {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }

            if app.quit {
                app.quit = false;
                DestroyWindow(hwnd); // posts WM_QUIT
                continue;
            }

            // Twice a second: stay on top, grab focus back, keep the mouse caged.
            if last_guard.elapsed() >= Duration::from_millis(500) {
                last_guard = Instant::now();
                if GetForegroundWindow() != hwnd {
                    SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
                    SetForegroundWindow(hwnd);
                }
                clip_cursor_to(hwnd);
            }

            let now = Instant::now();
            if now >= next_frame {
                let dt = (now - last).as_secs_f64().min(0.1);
                last = now;
                app.update(dt);
                app.draw(mem_dc);
                BitBlt(wdc, 0, 0, sw, sh, mem_dc, 0, 0, SRCCOPY);
                next_frame = (next_frame + FRAME).max(now);
            } else {
                // Sleep until the next frame, waking early for input.
                let ms = (next_frame - now).as_millis() as u32;
                MsgWaitForMultipleObjects(0, null(), 0, ms, QS_ALLINPUT);
            }
        }

        SelectObject(mem_dc, old_bitmap);
        DeleteObject(bitmap);
        DeleteDC(mem_dc);
        ReleaseDC(hwnd, wdc);
        DeleteObject(app.idle_font);
        DeleteObject(app.ticker_font);
        DeleteObject(app.small_font);
        restore_accessibility_hotkeys(&saved_accessibility);
        ClipCursor(null());
        SetThreadExecutionState(ES_CONTINUOUS);
        timeEndPeriod(1);
    }
}
