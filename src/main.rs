#![windows_subsystem = "windows"] // Hides the console window in both debug and release builds

use eframe::egui;
use eframe::epaint::Color32;
use raw_window_handle::HasWindowHandle;
use rfd::FileDialog;
use rodio::{OutputStream, Sink};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use winreg::enums::*;
use winreg::RegKey;

static WAKE_UP_UI: AtomicBool = AtomicBool::new(false);
static TRAY_HWND: AtomicUsize = AtomicUsize::new(0);
static TRAY_APP_ICON: AtomicUsize = AtomicUsize::new(0); // fallback / window icon
static TRAY_ICON_DIS: AtomicUsize = AtomicUsize::new(0);
static TRAY_ICON_READY: AtomicUsize = AtomicUsize::new(0);
static TRAY_ICON_PRESSING: AtomicUsize = AtomicUsize::new(0);
static MAIN_HWND: AtomicUsize = AtomicUsize::new(0);

// --- MASTER CONTROLLER LAYOUT ---
// This guarantees a logical display order and ensures no buttons are ever missing.
const ALL_INPUTS: &[&str] = &[
    "A",
    "B",
    "X",
    "Y",
    "DPAD_UP",
    "DPAD_DOWN",
    "DPAD_LEFT",
    "DPAD_RIGHT",
    "LB",
    "RB",
    "LT",
    "RT",
    "LS_CLICK",
    "RS_CLICK",
    "START",
    "BACK",
    "GUIDE",
    "LS_UP",
    "LS_DOWN",
    "LS_LEFT",
    "LS_RIGHT",
    "RS_UP",
    "RS_DOWN",
    "RS_LEFT",
    "RS_RIGHT",
];

// --- 1. KEYBOARD MACRO & VIRTUAL KEY ENGINE ---
fn parse_key_string(k: &str) -> Option<u16> {
    let k = k.to_lowercase();
    match k.as_str() {
        "ctrl" => Some(0x11),
        "shift" => Some(0x10),
        "alt" => Some(0x12),
        "win" | "command" | "meta" => Some(0x5B),
        "space" => Some(0x20),
        "enter" => Some(0x0D),
        "escape" => Some(0x1B),
        "tab" => Some(0x09),
        "backspace" => Some(0x08),
        "caps" => Some(0x14),

        "arrowup" => Some(0x26),
        "arrowdown" => Some(0x28),
        "arrowleft" => Some(0x25),
        "arrowright" => Some(0x27),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pgup" => Some(0x21),
        "pgdown" => Some(0x22),
        "insert" => Some(0x2D),
        "delete" => Some(0x2E),

        "lctrl" => Some(0xA2),
        "rctrl" => Some(0xA3),
        "lshift" => Some(0xA0),
        "rshift" => Some(0xA1),
        "lalt" => Some(0xA4),
        "ralt" => Some(0xA5),

        // OEM symbol keys — these require specific VK_OEM codes, not ASCII
        "semicolon" | ";" => Some(0xBA),
        "slash" | "/" => Some(0xBF),
        "backtick" | "`" => Some(0xC0),
        "lbracket" | "[" => Some(0xDB),
        "backslash" | "\\" => Some(0xDC),
        "rbracket" | "]" => Some(0xDD),
        "quote" | "'" => Some(0xDE),
        "comma" | "," => Some(0xBC),
        "period" | "." => Some(0xBE),
        "minus" | "-" => Some(0xBD),
        "plus" | "=" => Some(0xBB),

        // Numpad operator keys — distinct VK codes so they fire differently from row keys
        "num+" | "numadd" => Some(0x6B),        // VK_ADD
        "num-" | "numsub" => Some(0x6D),        // VK_SUBTRACT
        "num*" | "nummul" => Some(0x6A),        // VK_MULTIPLY
        "num/" | "numdiv" => Some(0x6F),        // VK_DIVIDE  (extended key)
        "num." | "numdec" => Some(0x6E),        // VK_DECIMAL
        "num_enter" | "numenter" => Some(0x0D), // VK_RETURN (scan code will differ via wScan)

        "vol_mute" => Some(0xAD),
        "vol_down" => Some(0xAE),
        "vol_up" => Some(0xAF),
        "media_next" => Some(0xB0),
        "media_prev" => Some(0xB1),
        "media_stop" => Some(0xB2),
        "media_play_pause" => Some(0xB3),

        "launch_mail" => Some(0xB4),
        "launch_media" => Some(0xB5),
        "launch_pc" => Some(0xB6),
        "launch_calc" => Some(0xB7),

        "browser_back" => Some(0xA6),
        "browser_forward" => Some(0xA7),
        "browser_refresh" => Some(0xA8),
        "browser_stop" => Some(0xA9),
        "browser_search" => Some(0xAA),
        "browser_fav" => Some(0xAB),
        "browser_home" => Some(0xAC),

        _ => {
            if k.len() == 1 {
                let c = k.chars().next().unwrap();
                if c >= 'a' && c <= 'z' {
                    Some(c.to_ascii_uppercase() as u16)
                } else if c >= '0' && c <= '9' {
                    Some(c as u16)
                }
                // VK_0..VK_9 = 0x30..0x39
                else {
                    None
                }
            } else if k.starts_with('f') && k.len() <= 3 {
                if let Ok(num) = k[1..].parse::<u16>() {
                    if (1..=24).contains(&num) {
                        return Some(0x6F + num);
                    }
                }
                None
            } else if k.starts_with("num") && k.len() == 4 {
                // "num0".."num9" → VK_NUMPAD0..VK_NUMPAD9 = 0x60..0x69
                // (numpad operator tokens like "num+" are already matched above)
                if let Ok(digit) = k[3..].parse::<u16>() {
                    if digit <= 9 {
                        return Some(0x60 + digit);
                    }
                }
                None
            } else {
                None
            }
        }
    }
}

fn get_display_name(raw: &str) -> String {
    // Numpad 0-9: "num0".."num9" → "Num 0".."Num 9"
    if raw.len() == 4 && raw.starts_with("num") {
        if let Ok(d) = raw[3..].parse::<u8>() {
            if d <= 9 {
                return format!("Num {}", d);
            }
        }
    }
    match raw {
        // Numpad operators
        "num+" => "Num +".to_string(),
        "num-" => "Num -".to_string(),
        "num*" => "Num *".to_string(),
        "num/" => "Num /".to_string(),
        "num." => "Num .".to_string(),
        "num_enter" => "Num Enter".to_string(),
        // OEM symbols
        ";" => ";".to_string(),
        "/" => "/".to_string(),
        "`" => "`".to_string(),
        "[" => "[".to_string(),
        "\\" => "\\".to_string(),
        "]" => "]".to_string(),
        "'" => "'".to_string(),
        "," => ",".to_string(),
        "." => ".".to_string(),
        "-" => "-".to_string(),
        "=" => "=".to_string(),
        _ => raw.to_uppercase(),
    }
}

fn simulate_macro(macro_str: &str, key_up: bool) {
    if macro_str.is_empty() {
        return;
    }
    let parts: Vec<&str> = macro_str.split('+').collect();

    unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
        let mut inputs: Vec<INPUT> = Vec::new();

        let parts_iter: Box<dyn Iterator<Item = &&str>> = if key_up {
            Box::new(parts.iter().rev())
        } else {
            Box::new(parts.iter())
        };

        for part in parts_iter {
            // For single non-alphanumeric characters, prefer the VK code path (handles
            // [, ], ;, / etc. reliably — Unicode injection skips WM_KEYDOWN so games
            // and system shortcuts never see it). Only fall back to Unicode for chars
            // that have no OEM VK mapping (e.g. exotic Unicode symbols).
            if part.len() == 1 {
                let c = part.chars().next().unwrap();
                if !c.is_alphanumeric() {
                    if let Some(vk) = parse_key_string(part) {
                        // Use the proper OEM virtual-key code so WM_KEYDOWN fires correctly.
                        let mut input: INPUT = std::mem::zeroed();
                        input.r#type = INPUT_KEYBOARD;
                        input.Anonymous.ki.wVk = vk;
                        input.Anonymous.ki.wScan =
                            windows_sys::Win32::UI::Input::KeyboardAndMouse::MapVirtualKeyW(
                                vk as u32, 0,
                            ) as u16;
                        if key_up {
                            input.Anonymous.ki.dwFlags = KEYEVENTF_KEYUP;
                        }
                        inputs.push(input);
                    } else {
                        // Fallback: Unicode injection for chars with no known OEM VK.
                        let mut input: INPUT = std::mem::zeroed();
                        input.r#type = INPUT_KEYBOARD;
                        input.Anonymous.ki.wVk = 0;
                        input.Anonymous.ki.wScan = c as u16;
                        input.Anonymous.ki.dwFlags = KEYEVENTF_UNICODE;
                        if key_up {
                            input.Anonymous.ki.dwFlags |= KEYEVENTF_KEYUP;
                        }
                        inputs.push(input);
                    }
                    continue;
                }
            }
            if let Some(vk) = parse_key_string(part) {
                let mut input: INPUT = std::mem::zeroed();
                input.r#type = INPUT_KEYBOARD;
                input.Anonymous.ki.wVk = vk;
                // Always populate the hardware scan code. Programs that distinguish
                // numpad keys from top-row keys (games, DAWs, etc.) check wScan
                // alongside wVk. MapVirtualKeyW(VK_NUMPAD5, MAPVK_VK_TO_VSC) returns
                // 0x4C, which is different from the row-5 scan code 0x06 — the
                // receiving application can then see the correct physical origin.
                input.Anonymous.ki.wScan =
                    windows_sys::Win32::UI::Input::KeyboardAndMouse::MapVirtualKeyW(vk as u32, 0)
                        as u16;
                // VK_DIVIDE (numpad /) and the navigation cluster keys are extended keys;
                // flag them so the scan code is correctly interpreted as E0-prefixed.
                let extended_vks: &[u16] = &[
                    0x6F, // VK_DIVIDE  (Numpad /)
                    0x25, 0x26, 0x27, 0x28, // arrow keys
                    0x24, 0x23, 0x21, 0x22, // Home/End/PgUp/PgDn
                    0x2D, 0x2E, // Insert/Delete
                    0xA3, 0xA5, // RCtrl, RAlt
                ];
                if extended_vks.contains(&vk) {
                    input.Anonymous.ki.dwFlags |= KEYEVENTF_EXTENDEDKEY;
                }
                if key_up {
                    input.Anonymous.ki.dwFlags |= KEYEVENTF_KEYUP;
                }
                inputs.push(input);
            }
        }
        if !inputs.is_empty() {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            );
        }
    }
}

// Extract the raw bitmap data (BITMAPINFOHEADER + XOR mask + AND mask) for
// the best-matching entry in an embedded .ico blob, then create an HICON.
fn extract_icon_data(ico_bytes: &[u8]) -> Option<&[u8]> {
    if ico_bytes.len() < 6 {
        return None;
    }
    let count = u16::from_le_bytes([ico_bytes[4], ico_bytes[5]]) as usize;
    if count == 0 || ico_bytes.len() < 6 + count * 16 {
        return None;
    }

    let mut best = 0usize;
    let mut best_bpp = 0u16;
    for i in 0..count {
        let e = 6 + i * 16;
        let w = ico_bytes[e] as u16;
        let h = ico_bytes[e + 1] as u16;
        let bpp = u16::from_le_bytes([ico_bytes[e + 6], ico_bytes[e + 7]]);
        if (w == 32 || w == 0) && (h == 32 || h == 0) && bpp >= best_bpp {
            best = i;
            best_bpp = bpp;
        }
    }

    let e = 6 + best * 16;
    let size = u32::from_le_bytes([
        ico_bytes[e + 8],
        ico_bytes[e + 9],
        ico_bytes[e + 10],
        ico_bytes[e + 11],
    ]) as usize;
    let off = u32::from_le_bytes([
        ico_bytes[e + 12],
        ico_bytes[e + 13],
        ico_bytes[e + 14],
        ico_bytes[e + 15],
    ]) as usize;
    if off + size <= ico_bytes.len() {
        Some(&ico_bytes[off..off + size])
    } else {
        None
    }
}

unsafe fn load_icon_from_bytes(data: &[u8]) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    if let Some(img) = extract_icon_data(data) {
        CreateIconFromResourceEx(img.as_ptr(), img.len() as u32, 1, 0x00030000, 0, 0, 0) as isize
    } else {
        0
    }
}

fn check_controller_full(index: u32) -> (bool, u16, i16, i16, i16, i16, u8, u8) {
    unsafe {
        use windows_sys::Win32::UI::Input::XboxController::*;
        let mut state: XINPUT_STATE = std::mem::zeroed();
        let result = XInputGetState(index, &mut state);
        if result == 0 {
            let gp = state.Gamepad;
            (
                true,
                gp.wButtons,
                gp.sThumbLX,
                gp.sThumbLY,
                gp.sThumbRX,
                gp.sThumbRY,
                gp.bLeftTrigger,
                gp.bRightTrigger,
            )
        } else {
            (false, 0, 0, 0, 0, 0, 0, 0)
        }
    }
}

fn is_windows_dark_mode() -> bool {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(key) =
        hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
    {
        if let Ok(val) = key.get_value::<u32, _>("AppsUseLightTheme") {
            return val == 0;
        }
    }
    true
}

// --- 2. CONFIGURATION & STATE MANAGERS ---
#[derive(Serialize, Deserialize, Clone, PartialEq, Default)]
enum AppTheme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Profile {
    #[serde(default)]
    last_used: bool,
    mappings: HashMap<String, String>,
}

#[derive(PartialEq)]
enum Tab {
    Mappings,
    Settings,
}

fn profiles_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

struct AppState {
    profiles: HashMap<String, Profile>,
    current_profile_name: String,
    active_tab: Tab,
    theme: AppTheme,
    close_to_tray: bool,
    run_at_startup: bool,
    is_paused: bool,
    connected_device: Arc<Mutex<bool>>,
    pressed_inputs: Arc<Mutex<HashMap<String, bool>>>,
    active_mapping: Arc<Mutex<HashMap<String, String>>>,
    recording_key: Option<String>,
    window_configured: bool,
    sound_enabled: bool,
    sound_enabled_atomic: Arc<AtomicBool>,
    rename_target: Option<String>,
    rename_buffer: String,
}

impl AppState {
    fn profile_path(&self) -> std::path::PathBuf {
        profiles_dir().join(format!("{}.json", self.current_profile_name))
    }

    fn save_to_disk(&self) {
        let dir = profiles_dir();
        let current_name = &self.current_profile_name;

        // Save every *other* profile with last_used = false
        for (name, profile) in &self.profiles {
            if name.as_str() != current_name.as_str() && profile.last_used {
                let mut clean = profile.clone();
                clean.last_used = false;
                if let Ok(json) = serde_json::to_string_pretty(&clean) {
                    let _ = std::fs::write(dir.join(format!("{}.json", name)), json);
                }
            }
        }

        // Save the current profile with last_used = true
        if let Some(profile) = self.profiles.get(current_name) {
            let mut profile = profile.clone();
            profile.last_used = true;
            if let Ok(json) = serde_json::to_string_pretty(&profile) {
                let _ = std::fs::write(self.profile_path(), json);
            }
            *self.active_mapping.lock().unwrap() = profile.mappings.clone();
        }
    }
}

// --- 3. PURE WIN32 TRAY ICON SETUP ---
unsafe extern "system" fn tray_wnd_proc(
    hwnd: windows_sys::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows_sys::Win32::Foundation::WPARAM,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::Win32::Foundation::LRESULT {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    if msg == WM_USER + 1 {
        let event = lparam as u32 & 0xFFFF;
        match event {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                let main_hwnd = MAIN_HWND.load(Ordering::SeqCst) as *mut std::ffi::c_void;
                if !main_hwnd.is_null() {
                    ShowWindow(main_hwnd, SW_RESTORE);
                    SetForegroundWindow(main_hwnd);
                }
                WAKE_UP_UI.store(true, Ordering::SeqCst);
            }
            WM_RBUTTONUP => {
                let hmenu = CreatePopupMenu();
                AppendMenuW(hmenu, MF_STRING, 101, windows_sys::w!("Show Mapper"));
                AppendMenuW(hmenu, MF_STRING, 102, windows_sys::w!("Exit Program"));
                let mut pt: POINT = std::mem::zeroed();
                GetCursorPos(&mut pt);
                SetForegroundWindow(hwnd);
                TrackPopupMenu(
                    hmenu,
                    TPM_LEFTALIGN | TPM_RIGHTBUTTON,
                    pt.x,
                    pt.y,
                    0,
                    hwnd,
                    std::ptr::null(),
                );
                DestroyMenu(hmenu);
            }
            _ => {}
        }
        return 0;
    }

    if msg == WM_USER + 2 {
        use windows_sys::Win32::UI::Shell::*;
        let (hicon, tooltip) = match wparam {
            0 => (
                TRAY_ICON_DIS.load(Ordering::SeqCst) as HICON,
                "JoyMapper Pro — Disconnected",
            ),
            1 => (
                TRAY_ICON_READY.load(Ordering::SeqCst) as HICON,
                "JoyMapper Pro — Ready",
            ),
            _ => (
                TRAY_ICON_PRESSING.load(Ordering::SeqCst) as HICON,
                "JoyMapper Pro — Active",
            ),
        };
        // Fall back to the embedded app icon if a tray icon didn't load
        let hicon = if hicon.is_null() {
            TRAY_APP_ICON.load(Ordering::SeqCst) as HICON
        } else {
            hicon
        };
        let mut tip_chars = [0u16; 128];
        for (i, c) in tooltip.encode_utf16().enumerate() {
            tip_chars[i] = c;
        }
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = 1;
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = hicon;
        nid.szTip = tip_chars;
        Shell_NotifyIconW(NIM_MODIFY, &nid);
        return 0;
    }

    if msg == WM_COMMAND {
        if wparam == 101 {
            let main_hwnd = MAIN_HWND.load(Ordering::SeqCst) as *mut std::ffi::c_void;
            if !main_hwnd.is_null() {
                ShowWindow(main_hwnd, SW_RESTORE);
                SetForegroundWindow(main_hwnd);
                WAKE_UP_UI.store(true, Ordering::SeqCst);
            }
        } else if wparam == 102 {
            std::process::exit(0);
        }
        return 0;
    }

    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn init_tray_icon() -> isize {
    unsafe {
        use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
        use windows_sys::Win32::UI::Shell::*;
        use windows_sys::Win32::UI::WindowsAndMessaging::*;

        // 1. Get the path to the current running executable
        let mut exe_path = [0u16; 260];
        GetModuleFileNameW(std::ptr::null_mut(), exe_path.as_mut_ptr(), 260);

        // 2. Extract the primary icon (index 0) embedded in this .exe
        let mut hicon: HICON = std::ptr::null_mut();
        ExtractIconExW(exe_path.as_ptr(), 0, std::ptr::null_mut(), &mut hicon, 1);

        // Fallback to a system icon just in case the .exe has no embedded icon yet
        if hicon.is_null() {
            ExtractIconExW(
                windows_sys::w!("shell32.dll"),
                176,
                std::ptr::null_mut(),
                &mut hicon,
                1,
            );
        }

        let hicon_raw = hicon as isize;
        TRAY_APP_ICON.store(hicon_raw as usize, Ordering::SeqCst);

        // Load tray-state icons embedded directly in the binary
        TRAY_ICON_DIS.store(
            load_icon_from_bytes(include_bytes!("../app_icon_dis.ico")) as usize,
            Ordering::SeqCst,
        );
        TRAY_ICON_READY.store(
            load_icon_from_bytes(include_bytes!("../app_icon_ready.ico")) as usize,
            Ordering::SeqCst,
        );
        TRAY_ICON_PRESSING.store(
            load_icon_from_bytes(include_bytes!("../app_icon_pressing.ico")) as usize,
            Ordering::SeqCst,
        );

        std::thread::spawn(move || {
            let hinstance = GetModuleHandleW(std::ptr::null());
            let class_name = windows_sys::w!("JoyMapperTrayClass");
            let mut wc: WNDCLASSEXW = std::mem::zeroed();
            wc.cbSize = std::mem::size_of::<WNDCLASSEXW>() as u32;
            wc.lpfnWndProc = Some(tray_wnd_proc);
            wc.hInstance = hinstance;
            wc.lpszClassName = class_name;
            RegisterClassExW(&wc);

            let hwnd = CreateWindowExW(
                0,
                class_name,
                windows_sys::w!("TrayWindow"),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                hinstance,
                std::ptr::null(),
            );
            TRAY_HWND.store(hwnd as usize, Ordering::SeqCst);

            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = 1;
            nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            nid.uCallbackMessage = WM_USER + 1;
            nid.hIcon = hicon_raw as HICON;
            let tooltip = "JoyMapper Pro";
            let mut tip_chars = [0u16; 128];
            for (i, c) in tooltip.encode_utf16().enumerate() {
                tip_chars[i] = c;
            }
            nid.szTip = tip_chars;
            Shell_NotifyIconW(NIM_ADD, &nid);

            let mut msg = std::mem::zeroed();
            while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        });
        hicon_raw
    }
}

fn update_tray_icon(status: &str) {
    let code: usize = match status {
        "disconnected" => 0,
        "ready" => 1,
        _ => 2,
    };
    let hwnd = TRAY_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        unsafe {
            use windows_sys::Win32::UI::WindowsAndMessaging::*;
            PostMessageW(hwnd as *mut std::ffi::c_void, WM_USER + 2, code, 0);
        }
    }
}

// --- 5. SOUND FEEDBACK ENGINE ---
// SoundEngine runs on its own thread so clicks play even when the
// egui window is hidden (trayed).  The polling thread sends () on
// an mpsc channel; a dedicated receiver thread decodes and plays.
struct SoundEngine {
    _stream: OutputStream,
    sender: std::sync::mpsc::Sender<()>,
}

impl SoundEngine {
    fn new() -> Self {
        let (stream, stream_handle) = OutputStream::try_default().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();

        std::thread::spawn(move || {
            let sink = Sink::try_new(&stream_handle).unwrap();
            sink.set_volume(0.1);
            while let Ok(()) = rx.recv() {
                use rodio::Source;
                let source = rodio::source::SineWave::new(1000.0)
                    .take_duration(std::time::Duration::from_millis(30))
                    .amplify(0.2);
                sink.append(source);
            }
        });

        Self {
            _stream: stream,
            sender: tx,
        }
    }

    fn sender(&self) -> std::sync::mpsc::Sender<()> {
        self.sender.clone()
    }
}

// --- 4. MAIN APPLICATION LOGIC ---
struct JoyMapperApp {
    state: AppState,
    tray_hicon: isize,
    sound_engine: SoundEngine,
}

impl JoyMapperApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let tray_hicon = init_tray_icon();

        // Scan the app's folder for .json profile files. Each file = one profile.
        let dir = profiles_dir();
        let mut profiles = HashMap::new();

        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(data) = std::fs::read_to_string(&path) {
                        if let Ok(profile) = serde_json::from_str::<Profile>(&data) {
                            let name = path.file_stem().unwrap().to_string_lossy().to_string();
                            profiles.insert(name, profile);
                        }
                    }
                }
            }
        }

        if profiles.is_empty() {
            let mut default_mappings = HashMap::new();
            for name in ALL_INPUTS {
                default_mappings.insert(name.to_string(), "".to_string());
            }
            let default = Profile {
                mappings: default_mappings,
                ..Default::default()
            };
            if let Ok(json) = serde_json::to_string_pretty(&default) {
                let _ = std::fs::write(dir.join("Default.json"), json);
            }
            profiles.insert("Default".to_string(), default);
        }

        // Repair missing inputs for any new controller buttons added later
        for profile in profiles.values_mut() {
            for name in ALL_INPUTS {
                profile
                    .mappings
                    .entry(name.to_string())
                    .or_insert_with(|| "".to_string());
            }
        }

        let current_profile_name = {
            let mut flagged: Vec<String> = profiles
                .iter()
                .filter(|(_, p)| p.last_used)
                .map(|(n, _)| n.clone())
                .collect();

            if flagged.len() > 1 {
                // Multiple flags — pick alphabetically, clear the rest
                flagged.sort();
                for name in flagged.drain(1..) {
                    if let Some(p) = profiles.get_mut(&name) {
                        p.last_used = false;
                        if let Ok(json) = serde_json::to_string_pretty(p) {
                            let _ = std::fs::write(dir.join(format!("{}.json", &name)), json);
                        }
                    }
                }
            }

            let chosen = if flagged.len() == 1 {
                flagged[0].clone()
            } else if profiles.contains_key("Default") {
                "Default".to_string()
            } else {
                profiles.keys().next().unwrap().clone()
            };

            if let Some(p) = profiles.get_mut(&chosen) {
                p.last_used = true;
            }
            chosen
        };

        let initial_mapping = profiles
            .get(&current_profile_name)
            .unwrap()
            .mappings
            .clone();
        let active_mapping = Arc::new(Mutex::new(initial_mapping));
        let mapping_clone = active_mapping.clone();

        let pressed_inputs = Arc::new(Mutex::new(HashMap::new()));
        let pressed_clone = pressed_inputs.clone();
        let connected_device = Arc::new(Mutex::new(false));
        let connected_clone = connected_device.clone();
        let ctx_clone = cc.egui_ctx.clone();

        let sound_engine = SoundEngine::new();
        let click_tx = sound_engine.sender();
        let sound_enabled = Arc::new(AtomicBool::new(true));
        let sound_enabled_poll = sound_enabled.clone();

        // Hardware Polling Thread (Full XInput coverage)
        std::thread::spawn(move || {
            let mut was_connected = false;
            let mut last_tray_update = std::time::Instant::now();
            let click_tx = click_tx;
            let sound_enabled = sound_enabled_poll;
            loop {
                let (connected, buttons, lx, ly, rx, ry, lt, rt) = check_controller_full(0);

                if connected != was_connected {
                    *connected_clone.lock().unwrap() = connected;
                    was_connected = connected;
                    update_tray_icon(if connected { "ready" } else { "disconnected" });
                    last_tray_update = std::time::Instant::now();
                    ctx_clone.request_repaint();
                }

                if connected {
                    let mut current_pressed: HashMap<String, bool> = HashMap::new();

                    let digital_map = vec![
                        ("A", 0x1000),
                        ("B", 0x2000),
                        ("X", 0x4000),
                        ("Y", 0x8000),
                        ("DPAD_UP", 0x0001),
                        ("DPAD_DOWN", 0x0002),
                        ("DPAD_LEFT", 0x0004),
                        ("DPAD_RIGHT", 0x0008),
                        ("LB", 0x0100),
                        ("RB", 0x0200),
                        ("LS_CLICK", 0x0040),
                        ("RS_CLICK", 0x0080),
                        ("START", 0x0010),
                        ("BACK", 0x0020),
                        ("GUIDE", 0x0400),
                    ];

                    for (name, flag) in digital_map {
                        current_pressed.insert(name.to_string(), (buttons & flag) != 0);
                    }

                    current_pressed.insert("LT".to_string(), lt > 128);
                    current_pressed.insert("RT".to_string(), rt > 128);
                    current_pressed.insert("LS_UP".to_string(), ly > 16000);
                    current_pressed.insert("LS_DOWN".to_string(), ly < -16000);
                    current_pressed.insert("LS_LEFT".to_string(), lx < -16000);
                    current_pressed.insert("LS_RIGHT".to_string(), lx > 16000);
                    current_pressed.insert("RS_UP".to_string(), ry > 16000);
                    current_pressed.insert("RS_DOWN".to_string(), ry < -16000);
                    current_pressed.insert("RS_LEFT".to_string(), rx < -16000);
                    current_pressed.insert("RS_RIGHT".to_string(), rx > 16000);

                    let mut lock = pressed_clone.lock().unwrap();
                    let macro_map = mapping_clone.lock().unwrap();

                    // CRITICAL FIX: Track if any state changed to force the UI to repaint instantly
                    let mut state_changed = false;

                    for (name, is_pressed) in &current_pressed {
                        let was_pressed = *lock.get(name).unwrap_or(&false);
                        if *is_pressed && !was_pressed {
                            if let Some(macro_str) = macro_map.get(name) {
                                simulate_macro(macro_str, false);
                            }
                            if sound_enabled.load(Ordering::SeqCst) {
                                let _ = click_tx.send(());
                            }
                            state_changed = true;
                        } else if !*is_pressed && was_pressed {
                            if let Some(macro_str) = macro_map.get(name) {
                                simulate_macro(macro_str, true);
                            }
                            state_changed = true;
                        }
                    }
                    *lock = current_pressed;

                    // Fire the repaint command instantly if you push or release a button
                    if state_changed {
                        update_tray_icon("pressed");
                        last_tray_update = std::time::Instant::now();
                        ctx_clone.request_repaint();
                    } else if last_tray_update.elapsed() > std::time::Duration::from_millis(1000) {
                        update_tray_icon("ready");
                        last_tray_update = std::time::Instant::now();
                    }
                } else if last_tray_update.elapsed() > std::time::Duration::from_millis(1000) {
                    update_tray_icon("disconnected");
                    last_tray_update = std::time::Instant::now();
                }
                std::thread::sleep(Duration::from_millis(16));
            }
        });

        let mut app = Self {
            state: AppState {
                profiles,
                current_profile_name,
                active_tab: Tab::Mappings,
                theme: AppTheme::System,
                close_to_tray: true,
                run_at_startup: false,
                is_paused: false,
                connected_device,
                pressed_inputs,
                active_mapping,
                recording_key: None,
                window_configured: false,
                sound_enabled: true,
                sound_enabled_atomic: sound_enabled,
                rename_target: None,
                rename_buffer: String::new(),
            },
            tray_hicon,
            sound_engine,
        };

        app.state.save_to_disk();
        app.check_startup_registry();
        app
    }

    fn check_startup_registry(&mut self) {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(run_key) = hkcu.open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run") {
            let val: Result<String, _> = run_key.get_value("JoyMapperRust");
            self.state.run_at_startup = val.is_ok();
        }
    }

    fn toggle_startup(&self, enabled: bool) {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok((run_key, _)) =
            hkcu.create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")
        {
            if enabled {
                if let Ok(exe_path) = std::env::current_exe() {
                    let _ =
                        run_key.set_value("JoyMapperRust", &exe_path.to_string_lossy().to_string());
                }
            } else {
                let _ = run_key.delete_value("JoyMapperRust");
            }
        }
    }

    fn apply_modern_theme(&self, ctx: &egui::Context) {
        let is_dark = match self.state.theme {
            AppTheme::System => is_windows_dark_mode(),
            AppTheme::Dark => true,
            AppTheme::Light => false,
        };

        let mut style = (*ctx.style()).clone();
        style.visuals = if is_dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        // Windows 11 rounding and padding
        style.visuals.window_rounding = egui::Rounding::same(12.0);
        style.visuals.widgets.noninteractive.rounding = egui::Rounding::same(6.0);
        style.visuals.widgets.inactive.rounding = egui::Rounding::same(6.0);
        style.visuals.widgets.hovered.rounding = egui::Rounding::same(6.0);
        style.visuals.widgets.active.rounding = egui::Rounding::same(6.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.item_spacing = egui::vec2(8.0, 10.0);
        style.spacing.menu_margin = egui::Margin::same(6.0);

        // Windows 11 accent — blue highlight
        let accent = Color32::from_rgb(0, 120, 212); // #0078D4
        style.visuals.selection.bg_fill = accent;
        style.visuals.hyperlink_color = accent;
        style.visuals.widgets.hovered.bg_fill = if is_dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, 20)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, 12)
        };
        style.visuals.widgets.active.bg_fill = if is_dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, 35)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, 20)
        };

        // Solid popup/menu background — avoids transparent context-menu readability issues
        let base_bg = if is_dark {
            Color32::from_rgba_unmultiplied(32, 32, 36, 252)
        } else {
            Color32::from_rgba_unmultiplied(243, 243, 243, 252)
        };
        style.visuals.window_fill = base_bg;
        style.visuals.panel_fill = Color32::TRANSPARENT;

        // Subtle border to distinguish panels
        let border_col = if is_dark {
            Color32::from_rgba_unmultiplied(255, 255, 255, 15)
        } else {
            Color32::from_rgba_unmultiplied(0, 0, 0, 18)
        };
        style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, border_col);

        // Elevated shadow for menus / popups
        style.visuals.window_shadow = egui::epaint::Shadow {
            offset: egui::vec2(0.0, 4.0),
            blur: 16.0,
            spread: 0.0,
            color: Color32::from_black_alpha(90),
        };

        ctx.set_style(style);
    }
}

// --- 5. UI RENDERING ---
impl eframe::App for JoyMapperApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Keep the sound engine's OutputStream alive for the dedicated audio thread.
        let _binding = &self.sound_engine;
        // --- 1. WINDOW SYSTEM LOGIC ---
        if MAIN_HWND.load(Ordering::SeqCst) == 0 {
            if let Ok(handle) = frame.window_handle() {
                if let raw_window_handle::RawWindowHandle::Win32(win32) = handle.as_raw() {
                    MAIN_HWND.store(win32.hwnd.get() as usize, Ordering::SeqCst);
                }
            }
        }

        if WAKE_UP_UI.swap(false, Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.request_repaint();
        }

        if !self.state.window_configured {
            if let Ok(handle) = frame.window_handle() {
                let _ = window_vibrancy::apply_mica(&handle, Some(is_windows_dark_mode()));

                if let raw_window_handle::RawWindowHandle::Win32(win32) = handle.as_raw() {
                    unsafe {
                        use windows_sys::Win32::UI::WindowsAndMessaging::*;
                        SendMessageW(win32.hwnd.get() as _, WM_SETICON, 1, self.tray_hicon);
                        SendMessageW(win32.hwnd.get() as _, WM_SETICON, 0, self.tray_hicon);
                    }
                }
            }
            self.state.window_configured = true;
        }

        // --- WINDOW CLOSE BEHAVIOR ---
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.state.close_to_tray {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }

        // --- 2. THEME & MACRO LOGIC ---
        self.apply_modern_theme(ctx);

        let is_dark = matches!(self.state.theme, AppTheme::Dark)
            || (matches!(self.state.theme, AppTheme::System) && is_windows_dark_mode());
        let bg_color = if is_dark {
            Color32::from_rgba_unmultiplied(30, 30, 35, 160)
        } else {
            Color32::from_rgba_unmultiplied(245, 245, 250, 160)
        };

        let recording_result = if let Some(ref target_key) = self.state.recording_key {
            ctx.input(|i| {
                let mut key_names: Vec<String> = Vec::new();
                if i.modifiers.ctrl {
                    key_names.push("ctrl".to_string());
                }
                if i.modifiers.shift {
                    key_names.push("shift".to_string());
                }
                if i.modifiers.alt {
                    key_names.push("alt".to_string());
                }
                if i.modifiers.mac_cmd {
                    key_names.push("win".to_string());
                }

                let key_event = i.events.iter().find_map(|e| {
                    if let egui::Event::Key {
                        key, pressed: true, ..
                    } = e
                    {
                        // Escape cancels recording without assigning
                        if *key == egui::Key::Escape {
                            return Some("__cancel__".to_string());
                        }
                        let raw = format!("{:?}", key).to_lowercase();
                        // Normalize egui Key debug names → our internal parse_key_string tokens.
                        // egui names OEM keys like Key::OpenBracket, Key::CloseBracket, etc.
                        // but our engine expects "[", "]", ";", etc.
                        let normalized = match raw.as_str() {
                            "openbracket" => "[".to_string(),
                            "closebracket" => "]".to_string(),
                            "backslash" => "\\".to_string(),
                            "semicolon" => ";".to_string(),
                            "quote" => "'".to_string(),
                            "comma" => ",".to_string(),
                            "period" => ".".to_string(),
                            "minus" => "-".to_string(),
                            "equals" | "plus" | "equal" => "=".to_string(),
                            "backtick" | "grave" | "graveaccent" => "`".to_string(),
                            "slash" => "/".to_string(),
                            // Numpad operator keys from egui (Key::NumpadAdd, etc.)
                            "numpadadd" => "num+".to_string(),
                            "numpadsubtract" => "num-".to_string(),
                            "numpadmultiply" => "num*".to_string(),
                            "numpaddivide" => "num/".to_string(),
                            "numpaddecimal" => "num.".to_string(),
                            "numpadenter" => "num_enter".to_string(),
                            other => {
                                // egui top-row number keys: Key::Num0..Num9
                                //   debug format → "num0".."num9"
                                //   These are the NUMBER ROW (VK 0x30-0x39), NOT numpad.
                                //   Token must be the bare digit so parse_key_string returns 0x30+n.
                                if other.len() == 4 && other.starts_with("num") {
                                    if let Ok(d) = other[3..].parse::<u8>() {
                                        if d <= 9 {
                                            return Some(d.to_string());
                                        }
                                    }
                                }
                                // egui numpad digit keys: Key::Numpad0..Numpad9
                                //   debug format → "numpad0".."numpad9"
                                //   These ARE the numpad (VK 0x60-0x69).
                                //   Token = "num0".."num9" so parse_key_string returns 0x60+n.
                                if other.len() == 7 && other.starts_with("numpad") {
                                    if let Ok(d) = other[6..].parse::<u8>() {
                                        if d <= 9 {
                                            return Some(format!("num{}", d));
                                        }
                                    }
                                }
                                other.to_string()
                            }
                        };
                        Some(normalized)
                    } else {
                        None
                    }
                });

                key_event.map(|k| {
                    key_names.push(k);
                    (target_key.clone(), key_names.join("+"))
                })
            })
        } else {
            None
        };

        if let Some((k, macro_str)) = recording_result {
            if macro_str == "__cancel__" {
                self.state.recording_key = None;
            } else {
                self.state
                    .profiles
                    .get_mut(&self.state.current_profile_name)
                    .unwrap()
                    .mappings
                    .insert(k, macro_str);
                self.state.save_to_disk();
                self.state.recording_key = None;
                ctx.request_repaint();
            }
        }

        // --- 3. UI RENDERING ---

        // TOP PANEL: Static Header & Tabs
        egui::TopBottomPanel::top("top_panel")
            .frame(egui::Frame::none().fill(bg_color).inner_margin(12.0))
            .show(ctx, |ui| {
                ui.heading(egui::RichText::new("🎮  JoyMapper Pro").size(20.0).strong());
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.state.active_tab, Tab::Mappings, "🎯 Mappings");
                    ui.selectable_value(&mut self.state.active_tab, Tab::Settings, "⚙ Settings");
                });
            });

        // CENTRAL PANEL: Fills the rest of the window (Responsive)
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(bg_color).inner_margin(12.0))
            .show(ctx, |ui| {

                match self.state.active_tab {
                    Tab::Mappings => {
                        ui.horizontal(|ui| {
                            ui.label("Profile:");
                            egui::ComboBox::from_id_source("profile_select")
                                .selected_text(&self.state.current_profile_name)
                                .show_ui(ui, |ui| {
                                    let names: Vec<_> = self.state.profiles.keys().cloned().collect();
                                    for name in names {
                                        if ui.selectable_value(&mut self.state.current_profile_name, name.clone(), name).clicked() {
                                            self.state.save_to_disk();
                                        }
                                    }
                                });

                            if self.state.profiles.len() > 1 {
                                if ui.button("✏").on_hover_text("Rename").clicked() {
                                    self.state.rename_target = Some(self.state.current_profile_name.clone());
                                    self.state.rename_buffer = self.state.current_profile_name.clone();
                                }
                            }

                            if ui.button("📂 Import").clicked() {
                                if let Some(path) = FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                                    if let Ok(content) = std::fs::read_to_string(&path) {
                                        if let Ok(profile) = serde_json::from_str::<Profile>(&content) {
                                            let stem = path.file_stem().unwrap().to_string_lossy().to_string();
                                            let dest = profiles_dir().join(format!("{}.json", stem));
                                            let _ = std::fs::copy(&path, &dest);
                                            self.state.profiles.insert(stem.clone(), profile);
                                            self.state.current_profile_name = stem;
                                            self.state.save_to_disk();
                                        }
                                    }
                                }
                            }

                            if ui.button("💾 Export").clicked() {
                                if let Some(path) = FileDialog::new().add_filter("JSON", &["json"]).save_file() {
                                    let profile = self.state.profiles.get(&self.state.current_profile_name).unwrap();
                                    let json = serde_json::to_string_pretty(profile).unwrap();
                                    let _ = std::fs::write(path, json);
                                }
                            }

                            if ui.button("➕ New").clicked() {
                                let mut new_mappings = HashMap::new();
                                for name in ALL_INPUTS { new_mappings.insert(name.to_string(), "".to_string()); }
                                let mut unique_name = "New Profile".to_string();
                                let mut counter = 1;
                                while self.state.profiles.contains_key(&unique_name) {
                                    counter += 1;
                                    unique_name = format!("New Profile ({})", counter);
                                }
                                self.state.profiles.insert(unique_name.clone(), Profile { mappings: new_mappings, ..Default::default() });
                                self.state.current_profile_name = unique_name;
                                self.state.save_to_disk();
                            }

                            if self.state.profiles.len() > 1 {
                                if ui.button("🗑 Delete").clicked() {
                                    let dir = profiles_dir();
                                    let path = dir.join(format!("{}.json", self.state.current_profile_name));
                                    let _ = std::fs::remove_file(&path);
                                    self.state.profiles.remove(&self.state.current_profile_name);
                                    self.state.current_profile_name = self.state.profiles.keys().next().unwrap().clone();
                                    self.state.save_to_disk();
                                }
                            }

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.button(if self.state.is_paused { "▶ Resume" } else { "⏸ Pause" }).clicked() {
                                    self.state.is_paused = !self.state.is_paused;
                                }
                            });
                        });

                        if let Some(ref rec_key) = self.state.recording_key {
                            ui.add_space(4.0);
                            egui::Frame::none()
                                .fill(Color32::from_rgba_unmultiplied(220, 60, 0, 200))
                                .rounding(6.0)
                                .inner_margin(egui::Margin::symmetric(10.0, 5.0))
                                .show(ui, |ui| {
                                    ui.colored_label(Color32::WHITE,
                                        format!("● Recording [ {} ] — press any key or combo, Esc to cancel", rec_key));
                                });
                        }

                        ui.add_space(10.0);
                        let mut needs_save = false;

                        // RESPONSIVE TABLE (JoyToKey Style)
                        use egui_extras::{TableBuilder, Column};

                        let table = TableBuilder::new(ui)
                            .striped(true)
                            .resizable(true)
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::initial(120.0).at_least(90.0))
                            .column(Column::remainder().at_least(100.0))
                            .column(Column::initial(50.0).at_least(40.0).clip(true))
                            .min_scrolled_height(0.0);

                        table.header(28.0, |mut header| {
                            header.col(|ui| { ui.strong("Controller Input"); });
                            header.col(|ui| { ui.strong("Mapped Key(s)"); });
                            header.col(|ui| { ui.strong("Action"); });
                        })
                        .body(|mut body| {
                            let lock = self.state.pressed_inputs.lock().unwrap();
                            let current_profile = self.state.profiles.get_mut(&self.state.current_profile_name).unwrap();

                            // CRITICAL FIX: Iterate over the constant ALL_INPUTS list, not the JSON map.
                            // This ensures the table renders in a perfect, logical order every time.
                            for name in ALL_INPUTS {
                                let input_name = name.to_string();
                                let is_pressed = *lock.get(&input_name).unwrap_or(&false);

                                body.row(32.0, |mut row| {
                                    row.col(|ui| {
                                        let mut text = egui::RichText::new(&input_name);
                                        if is_pressed {
                                            text = text.color(Color32::from_rgb(0, 200, 255)).strong();
                                        }
                                        ui.label(text);
                                    });

                                    row.col(|ui| {
                                        let val = current_profile.mappings.get(&input_name).unwrap();
                                        let display_text = if val.is_empty() { "Unmapped".to_string() } else { get_display_name(val) };

                                        let response = ui.add_sized(ui.available_size(), egui::Button::new(display_text));
                                        if response.clicked() {
                                            self.state.recording_key = Some(input_name.clone());
                                            ctx.request_repaint();
                                        }
                                        response.context_menu(|ui| {
                                            ui.set_min_width(180.0);
                                            ui.label(egui::RichText::new("Assign Key").strong());
                                            ui.separator();

                                            ui.menu_button("Mod Keys", |ui| {
                                                if ui.button("Ctrl").clicked() { current_profile.mappings.insert(input_name.clone(), "ctrl".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Alt").clicked() { current_profile.mappings.insert(input_name.clone(), "alt".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Shift").clicked() { current_profile.mappings.insert(input_name.clone(), "shift".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Win").clicked() { current_profile.mappings.insert(input_name.clone(), "win".to_string()); needs_save = true; ui.close_menu(); }
                                                ui.separator();
                                                if ui.button("Left Ctrl").clicked() { current_profile.mappings.insert(input_name.clone(), "lctrl".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Right Ctrl").clicked() { current_profile.mappings.insert(input_name.clone(), "rctrl".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Left Shift").clicked() { current_profile.mappings.insert(input_name.clone(), "lshift".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Right Shift").clicked() { current_profile.mappings.insert(input_name.clone(), "rshift".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Left Alt").clicked() { current_profile.mappings.insert(input_name.clone(), "lalt".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Right Alt").clicked() { current_profile.mappings.insert(input_name.clone(), "ralt".to_string()); needs_save = true; ui.close_menu(); }
                                            });

                                            ui.menu_button("Typing Keys", |ui| {
                                                if ui.button("Space").clicked() { current_profile.mappings.insert(input_name.clone(), "space".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Enter").clicked() { current_profile.mappings.insert(input_name.clone(), "enter".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Backspace").clicked() { current_profile.mappings.insert(input_name.clone(), "backspace".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Tab").clicked() { current_profile.mappings.insert(input_name.clone(), "tab".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Caps Lock").clicked() { current_profile.mappings.insert(input_name.clone(), "caps".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Escape").clicked() { current_profile.mappings.insert(input_name.clone(), "escape".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Delete").clicked() { current_profile.mappings.insert(input_name.clone(), "delete".to_string()); needs_save = true; ui.close_menu(); }
                                            });

                                            ui.menu_button("# Symbols", |ui| {
                                                if ui.button("; (Semicolon)").clicked() { current_profile.mappings.insert(input_name.clone(), ";".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("/ (Slash)").clicked() { current_profile.mappings.insert(input_name.clone(), "/".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("` (Backtick)").clicked() { current_profile.mappings.insert(input_name.clone(), "`".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("[ (LBracket)").clicked() { current_profile.mappings.insert(input_name.clone(), "[".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("\\ (Backslash)").clicked() { current_profile.mappings.insert(input_name.clone(), "\\".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("] (RBracket)").clicked() { current_profile.mappings.insert(input_name.clone(), "]".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("' (Quote)").clicked() { current_profile.mappings.insert(input_name.clone(), "'".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button(", (Comma)").clicked() { current_profile.mappings.insert(input_name.clone(), ",".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button(". (Period)").clicked() { current_profile.mappings.insert(input_name.clone(), ".".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("- (Minus)").clicked() { current_profile.mappings.insert(input_name.clone(), "-".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("= (Equals)").clicked() { current_profile.mappings.insert(input_name.clone(), "=".to_string()); needs_save = true; ui.close_menu(); }
                                            });

                                            ui.menu_button("Numpad", |ui| {
                                                ui.label(egui::RichText::new("Numpad Digits").weak().small());
                                                for d in 0u8..=9 {
                                                    if ui.button(format!("Num {}", d)).clicked() {
                                                        current_profile.mappings.insert(input_name.clone(), format!("num{}", d));
                                                        needs_save = true; ui.close_menu();
                                                    }
                                                }
                                                ui.separator();
                                                ui.label(egui::RichText::new("Numpad Operators").weak().small());
                                                if ui.button("Num +").clicked() { current_profile.mappings.insert(input_name.clone(), "num+".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Num -").clicked() { current_profile.mappings.insert(input_name.clone(), "num-".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Num *").clicked() { current_profile.mappings.insert(input_name.clone(), "num*".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Num /").clicked() { current_profile.mappings.insert(input_name.clone(), "num/".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Num .").clicked() { current_profile.mappings.insert(input_name.clone(), "num.".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Num Enter").clicked() { current_profile.mappings.insert(input_name.clone(), "num_enter".to_string()); needs_save = true; ui.close_menu(); }
                                            });

                                            ui.menu_button("Navigation", |ui| {
                                                if ui.button("↑ Arrow Up").clicked() { current_profile.mappings.insert(input_name.clone(), "arrowup".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("↓ Arrow Down").clicked() { current_profile.mappings.insert(input_name.clone(), "arrowdown".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("← Arrow Left").clicked() { current_profile.mappings.insert(input_name.clone(), "arrowleft".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("→ Arrow Right").clicked() { current_profile.mappings.insert(input_name.clone(), "arrowright".to_string()); needs_save = true; ui.close_menu(); }
                                                ui.separator();
                                                if ui.button("Home").clicked() { current_profile.mappings.insert(input_name.clone(), "home".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("End").clicked() { current_profile.mappings.insert(input_name.clone(), "end".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Page Up").clicked() { current_profile.mappings.insert(input_name.clone(), "pgup".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Page Down").clicked() { current_profile.mappings.insert(input_name.clone(), "pgdown".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Insert").clicked() { current_profile.mappings.insert(input_name.clone(), "insert".to_string()); needs_save = true; ui.close_menu(); }
                                            });

                                            ui.menu_button("Fn Keys", |ui| {
                                                for i in 1..=12 {
                                                    if ui.button(format!("F{}", i)).clicked() { current_profile.mappings.insert(input_name.clone(), format!("f{}", i)); needs_save = true; ui.close_menu(); }
                                                }
                                            });

                                            ui.separator();

                                            ui.menu_button("🔊 Volume & Media", |ui| {
                                                if ui.button("Mute").clicked() { current_profile.mappings.insert(input_name.clone(), "vol_mute".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Volume Up").clicked() { current_profile.mappings.insert(input_name.clone(), "vol_up".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Volume Down").clicked() { current_profile.mappings.insert(input_name.clone(), "vol_down".to_string()); needs_save = true; ui.close_menu(); }
                                                ui.separator();
                                                if ui.button("Play / Pause").clicked() { current_profile.mappings.insert(input_name.clone(), "media_play_pause".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Next Track").clicked() { current_profile.mappings.insert(input_name.clone(), "media_next".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Previous Track").clicked() { current_profile.mappings.insert(input_name.clone(), "media_prev".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Stop").clicked() { current_profile.mappings.insert(input_name.clone(), "media_stop".to_string()); needs_save = true; ui.close_menu(); }
                                            });

                                            ui.menu_button("🚀 App Launchers", |ui| {
                                                if ui.button("Calculator").clicked() { current_profile.mappings.insert(input_name.clone(), "launch_calc".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("My Computer").clicked() { current_profile.mappings.insert(input_name.clone(), "launch_pc".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Email").clicked() { current_profile.mappings.insert(input_name.clone(), "launch_mail".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Media Player").clicked() { current_profile.mappings.insert(input_name.clone(), "launch_media".to_string()); needs_save = true; ui.close_menu(); }
                                            });

                                            ui.menu_button("🌐 Browser Controls", |ui| {
                                                if ui.button("Back").clicked() { current_profile.mappings.insert(input_name.clone(), "browser_back".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Forward").clicked() { current_profile.mappings.insert(input_name.clone(), "browser_forward".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Refresh").clicked() { current_profile.mappings.insert(input_name.clone(), "browser_refresh".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Home").clicked() { current_profile.mappings.insert(input_name.clone(), "browser_home".to_string()); needs_save = true; ui.close_menu(); }
                                                if ui.button("Search").clicked() { current_profile.mappings.insert(input_name.clone(), "browser_search".to_string()); needs_save = true; ui.close_menu(); }
                                            });
                                        });
                                    });

                                    row.col(|ui| {
                                        ui.centered_and_justified(|ui| {
                                            if ui.button("🗑").on_hover_text("Clear mapping").clicked() {
                                                current_profile.mappings.insert(input_name.clone(), "".to_string());
                                                needs_save = true;
                                            }
                                        });
                                    });
                                });
                            }
                        });

                        if needs_save { self.state.save_to_disk(); }
                    }

                    Tab::Settings => {
                        ui.vertical(|ui| {
                            ui.checkbox(&mut self.state.close_to_tray, "Minimize to System Tray on close");
                            if ui.checkbox(&mut self.state.run_at_startup, "Launch silently on Windows startup").changed() {
                                self.toggle_startup(self.state.run_at_startup);
                            }

                            if ui.checkbox(&mut self.state.sound_enabled, "Play sound on button press").changed() {
                                self.state.sound_enabled_atomic.store(self.state.sound_enabled, Ordering::SeqCst);
                            }

                            ui.add_space(15.0);
                            ui.horizontal(|ui| {
                                ui.label("Theme:");
                                egui::ComboBox::from_id_source("theme_select")
                                    .selected_text(match self.state.theme {
                                        AppTheme::System => "System Default",
                                        AppTheme::Light => "Light",
                                        AppTheme::Dark => "Dark",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.state.theme, AppTheme::System, "System Default");
                                        ui.selectable_value(&mut self.state.theme, AppTheme::Light, "Light");
                                        ui.selectable_value(&mut self.state.theme, AppTheme::Dark, "Dark");
                                    });
                            });

                            ui.add_space(20.0);
                            ui.separator();
                            ui.add_space(10.0);

                            ui.label(egui::RichText::new("Hardware Status").strong());
                            ui.add_space(4.0);
                            let connected = *self.state.connected_device.lock().unwrap();

                            egui::Frame::none()
                                .fill(if is_dark { Color32::from_rgba_unmultiplied(20, 20, 25, 200) } else { Color32::from_rgba_unmultiplied(220, 220, 225, 200) })
                                .rounding(8.0)
                                .inner_margin(12.0)
                                .show(ui, |ui| {
                                    if connected {
                                        ui.colored_label(Color32::from_rgb(0, 200, 100), "Connected  \u{25CF}  Controller active via XInput");
                                        ui.small("Listening for button events at 60 Hz.");
                                    } else {
                                        ui.colored_label(Color32::from_rgb(220, 60, 60), "Disconnected  \u{25CF}  No controller detected");
                                        ui.small("Plug in an XInput controller to begin.");
                                    }
                                });
                        });
                    }
                }
            });

        // --- RENAME PROFILE DIALOG ---
        if let Some(ref target) = self.state.rename_target.clone() {
            let mut open = true;
            egui::Window::new("Rename Profile")
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .fixed_size([280.0, 100.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        if ui
                            .text_edit_singleline(&mut self.state.rename_buffer)
                            .lost_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            let new_name = self.state.rename_buffer.trim().to_string();
                            if !new_name.is_empty() && !self.state.profiles.contains_key(&new_name)
                            {
                                let dir = profiles_dir();
                                let old_path = dir.join(format!("{}.json", target));
                                let new_path = dir.join(format!("{}.json", &new_name));
                                let _ = std::fs::rename(&old_path, &new_path);
                                let mappings = self.state.profiles.remove(target).unwrap().mappings;
                                self.state.profiles.insert(
                                    new_name.clone(),
                                    Profile {
                                        mappings,
                                        ..Default::default()
                                    },
                                );
                                self.state.current_profile_name = new_name;
                                self.state.save_to_disk();
                                self.state.rename_target = None;
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.state.rename_target = None;
                        }
                        ui.add_space(10.0);
                        if ui.button("Rename").clicked() {
                            let new_name = self.state.rename_buffer.trim().to_string();
                            if !new_name.is_empty() && !self.state.profiles.contains_key(&new_name)
                            {
                                let old_name = self.state.rename_target.take().unwrap();
                                let dir = profiles_dir();
                                let old_path = dir.join(format!("{}.json", &old_name));
                                let new_path = dir.join(format!("{}.json", &new_name));
                                let _ = std::fs::rename(&old_path, &new_path);
                                let mappings =
                                    self.state.profiles.remove(&old_name).unwrap().mappings;
                                self.state.profiles.insert(
                                    new_name.clone(),
                                    Profile {
                                        mappings,
                                        ..Default::default()
                                    },
                                );
                                self.state.current_profile_name = new_name;
                                self.state.save_to_disk();
                            }
                        }
                    });
                });
            if !open {
                self.state.rename_target = None;
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([540.0, 520.0])
            .with_min_inner_size([480.0, 440.0])
            .with_decorations(true)
            .with_transparent(true),
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "JoyMapper Pro",
        options,
        Box::new(|cc| Box::new(JoyMapperApp::new(cc))),
    )
}
