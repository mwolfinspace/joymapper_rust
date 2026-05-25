#![windows_subsystem = "windows"] // Hides the console window in both debug and release builds

use eframe::egui;
use eframe::epaint::Color32;
use raw_window_handle::HasWindowHandle;
use rfd::FileDialog;
use rodio::{OutputStream, Sink};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
const DIGITAL_MAP: &[(&str, u16)] = &[
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
        // Core System Modifiers
        "ctrl" => Some(0x11),
        "shift" => Some(0x10),
        "alt" => Some(0x12),
        "win" | "command" | "meta" => Some(0x5B),

        // Navigation & Control Keys
        "space" => Some(0x20),
        "enter" => Some(0x0D),
        "escape" => Some(0x1B),
        "tab" => Some(0x09),
        "backspace" => Some(0x08),
        "caps" => Some(0x14),
        "insert" => Some(0x2D),
        "delete" => Some(0x2E),
        "home" => Some(0x24),
        "end" => Some(0x23),
        "pgup" => Some(0x21),
        "pgdown" => Some(0x22),
        "arrowup" => Some(0x26),
        "arrowdown" => Some(0x28),
        "arrowleft" => Some(0x25),
        "arrowright" => Some(0x27),

        // Exhaustive OEM Symbol Registry (shifted + unshifted)
        ";" | "semicolon" | ":" => Some(0xBA),
        "/" | "slash" | "?" => Some(0xBF),
        "`" | "backtick" | "~" => Some(0xC0),
        "[" | "lbracket" | "{" => Some(0xDB),
        "\\" | "backslash" | "|" => Some(0xDC),
        "]" | "rbracket" | "}" => Some(0xDD),
        "'" | "quote" | "\"" => Some(0xDE),
        "," | "comma" | "<" => Some(0xBC),
        "." | "period" | ">" => Some(0xBE),
        "-" | "minus" | "_" => Some(0xBD),
        "=" | "plus" => Some(0xBB),

        // Shifted symbol characters (same VK as their digit counterpart)
        "!" | "exclaim" => Some(0x31),
        "@" | "at" => Some(0x32),
        "#" | "hash" | "pound" => Some(0x33),
        "$" | "dollar" => Some(0x34),
        "%" | "percent" => Some(0x35),
        "^" | "caret" => Some(0x36),
        "&" | "ampersand" => Some(0x37),
        "*" | "asterisk" => Some(0x38),
        "(" => Some(0x39),
        ")" => Some(0x30),

        // Browser & Multimedia Keys (VK 0xA6–0xB7)
        "browser_back" => Some(0xA6),
        "browser_forward" => Some(0xA7),
        "browser_refresh" => Some(0xA8),
        "browser_stop" => Some(0xA9),
        "browser_search" => Some(0xAA),
        "browser_fav" | "browser_favorites" => Some(0xAB),
        "browser_home" => Some(0xAC),
        "vol_mute" | "volume_mute" => Some(0xAD),
        "vol_down" | "volume_down" => Some(0xAE),
        "vol_up" | "volume_up" => Some(0xAF),
        "media_next" | "media_next_track" => Some(0xB0),
        "media_prev" | "media_prev_track" => Some(0xB1),
        "media_stop" => Some(0xB2),
        "media_play_pause" => Some(0xB3),
        "launch_mail" => Some(0xB4),
        "launch_media" | "launch_media_select" => Some(0xB5),
        "launch_pc" | "launch_app1" => Some(0xB6),
        "launch_calc" | "launch_app2" => Some(0xB7),

        _ => {
            if k.len() == 1 {
                let c = k.chars().next().unwrap();
                if c.is_alphanumeric() {
                    Some(c.to_ascii_uppercase() as u16)
                } else {
                    None
                }
            } else if k.starts_with('f') {
                if let Ok(num) = k[1..].parse::<u16>() {
                    if (1..=24).contains(&num) {
                        return Some(0x6F + num);
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
    if raw.is_empty() {
        return "None".to_string();
    }
    match raw.to_lowercase().as_str() {
        "lbracket" => "[".to_string(),
        "rbracket" => "]".to_string(),
        "backslash" => "\\".to_string(),
        "semicolon" => ";".to_string(),
        "backtick" => "`".to_string(),
        "browser_back" => "Browser Back".to_string(),
        "browser_forward" => "Browser Fwd".to_string(),
        "browser_refresh" => "Browser Refresh".to_string(),
        "browser_stop" => "Browser Stop".to_string(),
        "browser_search" => "Browser Search".to_string(),
        "browser_fav" | "browser_favorites" => "Browser Favorites".to_string(),
        "browser_home" => "Browser Home".to_string(),
        "vol_mute" | "volume_mute" => "Vol Mute".to_string(),
        "vol_down" | "volume_down" => "Vol Down".to_string(),
        "vol_up" | "volume_up" => "Vol Up".to_string(),
        "media_next" | "media_next_track" => "Next Track".to_string(),
        "media_prev" | "media_prev_track" => "Prev Track".to_string(),
        "media_stop" => "Media Stop".to_string(),
        "media_play_pause" => "Play/Pause".to_string(),
        "launch_mail" => "Launch Mail".to_string(),
        "launch_media" | "launch_media_select" => "Launch Media".to_string(),
        "launch_pc" | "launch_app1" => "Launch PC".to_string(),
        "launch_calc" | "launch_app2" => "Launch Calc".to_string(),
        _ => raw.to_uppercase(),
    }
}

fn simulate_key(vk: u16, key_up: bool) {
    unsafe {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
        let mut input: INPUT = std::mem::zeroed();
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous.ki.wVk = vk;
        input.Anonymous.ki.wScan = MapVirtualKeyW(vk as u32, 0) as u16;
        let extended_vks: &[u16] = &[
            0x6F, 0x25, 0x26, 0x27, 0x28, 0x24, 0x23, 0x21, 0x22, 0x2D, 0x2E, 0xA3, 0xA5, 0xA6,
            0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF, 0xB0, 0xB1, 0xB2, 0xB3, 0xB4,
            0xB5, 0xB6, 0xB7,
        ];
        if extended_vks.contains(&vk) {
            input.Anonymous.ki.dwFlags |= KEYEVENTF_EXTENDEDKEY;
        }
        if key_up {
            input.Anonymous.ki.dwFlags |= KEYEVENTF_KEYUP;
        }
        SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
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

fn query_xinput_battery(index: u32) -> (u8, u8) {
    unsafe {
        use windows_sys::Win32::UI::Input::XboxController::*;
        let mut info: XINPUT_BATTERY_INFORMATION = std::mem::zeroed();
        let result = XInputGetBatteryInformation(index, BATTERY_DEVTYPE_GAMEPAD, &mut info);
        if result == 0 {
            (info.BatteryType, info.BatteryLevel)
        } else {
            (BATTERY_UNKNOWN, u8::MAX)
        }
    }
}

fn query_bluetooth_battery_percent() -> Option<u8> {
    let script = r#"
$ErrorActionPreference = 'SilentlyContinue'
$keys = @('DEVPKEY_Device_BatteryLevel', '{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2')
$devices = Get-PnpDevice -PresentOnly | Where-Object {
    ($_.Class -eq 'Bluetooth' -or $_.Class -eq 'HIDClass') -and
    (
        $_.FriendlyName -match '(?i)(xbox|controller|gamepad|8bitdo|joystick)' -or
        $_.InstanceId -match '(?i)(VID_045E|IG_00|IG_01|BTHENUM\\DEV_)'
    )
}

foreach ($device in $devices) {
    foreach ($key in $keys) {
        $property = Get-PnpDeviceProperty -InstanceId $device.InstanceId -KeyName $key
        if ($null -ne $property -and $property.Data -match '^\d+$') {
            $value = [int]$property.Data
            if ($value -ge 0 -and $value -le 100) {
                Write-Output $value
                exit 0
            }
        }
    }
}
exit 1
"#;

    let mut command = std::process::Command::new("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-Command",
        script,
    ]);

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }

    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.trim().parse::<u8>().ok())
        .filter(|value| *value <= 100)
}

fn query_battery(index: u32) -> (u8, u8) {
    query_xinput_battery(index)
}

const BATTERY_DISCONNECTED: u8 = 0x00;
const BATTERY_PERCENT: u8 = 0xFE;
const BATTERY_UNKNOWN: u8 = 0xFF;
const BATTERY_WIRED: u8 = 0x01;
const BATTERY_ALKALINE: u8 = 0x02;
const BATTERY_NIMH: u8 = 0x03;

// Extract the raw bitmap data (BITMAPINFOHEADER + XOR mask + AND mask) for
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
    #[serde(default)]
    total_key_presses: u64,
    mappings: HashMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct OldProfile {
    #[serde(default)]
    last_used: bool,
    mappings: HashMap<String, String>,
}

impl From<OldProfile> for Profile {
    fn from(old: OldProfile) -> Self {
        Profile {
            last_used: old.last_used,
            total_key_presses: 0,
            mappings: old
                .mappings
                .into_iter()
                .map(|(k, v)| (k, vec![v]))
                .collect(),
        }
    }
}

fn default_mapping_vec() -> Vec<String> {
    vec!["".to_string()]
}

struct Win11Colors {
    app_bg: Color32,
    surface: Color32,
    surface_alt: Color32,
    row_hover: Color32,
    row_active: Color32,
    border: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    accent_soft: Color32,
    success: Color32,
    danger: Color32,
    warning: Color32,
    empty_chip: Color32,
}

fn win11_colors(is_dark: bool) -> Win11Colors {
    if is_dark {
        Win11Colors {
            app_bg: Color32::from_rgba_unmultiplied(32, 32, 36, 178),
            surface: Color32::from_rgba_unmultiplied(43, 43, 48, 226),
            surface_alt: Color32::from_rgba_unmultiplied(255, 255, 255, 13),
            row_hover: Color32::from_rgba_unmultiplied(255, 255, 255, 8),
            row_active: Color32::from_rgba_unmultiplied(0, 120, 212, 38),
            border: Color32::from_rgba_unmultiplied(255, 255, 255, 22),
            text: Color32::from_rgb(243, 243, 243),
            muted: Color32::from_rgb(178, 178, 178),
            accent: Color32::from_rgb(0, 120, 212),
            accent_soft: Color32::from_rgba_unmultiplied(0, 120, 212, 48),
            success: Color32::from_rgb(108, 203, 95),
            danger: Color32::from_rgb(255, 99, 88),
            warning: Color32::from_rgb(255, 185, 0),
            empty_chip: Color32::from_rgba_unmultiplied(255, 255, 255, 22),
        }
    } else {
        Win11Colors {
            app_bg: Color32::from_rgba_unmultiplied(243, 243, 243, 214),
            surface: Color32::from_rgba_unmultiplied(255, 255, 255, 235),
            surface_alt: Color32::from_rgba_unmultiplied(0, 0, 0, 10),
            row_hover: Color32::from_rgba_unmultiplied(0, 0, 0, 6),
            row_active: Color32::from_rgba_unmultiplied(0, 120, 212, 32),
            border: Color32::from_rgba_unmultiplied(0, 0, 0, 24),
            text: Color32::from_rgb(32, 32, 32),
            muted: Color32::from_rgb(96, 96, 96),
            accent: Color32::from_rgb(0, 103, 192),
            accent_soft: Color32::from_rgba_unmultiplied(0, 103, 192, 26),
            success: Color32::from_rgb(16, 124, 16),
            danger: Color32::from_rgb(196, 43, 28),
            warning: Color32::from_rgb(157, 93, 0),
            empty_chip: Color32::from_rgba_unmultiplied(0, 0, 0, 14),
        }
    }
}

fn win11_card_frame(colors: &Win11Colors) -> egui::Frame {
    egui::Frame::none()
        .fill(colors.surface)
        .stroke(egui::Stroke::new(1.0, colors.border))
        .rounding(egui::Rounding::same(8.0))
        .inner_margin(egui::Margin::same(14.0))
}

fn input_display_name(raw: &str) -> String {
    raw.replace("DPAD_", "D-pad ")
        .replace("LS_", "Left stick ")
        .replace("RS_", "Right stick ")
        .replace('_', " ")
}

#[derive(Clone, Copy, PartialEq)]
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

#[derive(Serialize, Deserialize)]
struct AppConfig {
    #[serde(default = "default_start_minimized")]
    start_minimized: bool,
}

fn default_start_minimized() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            start_minimized: default_start_minimized(),
        }
    }
}

fn load_config() -> AppConfig {
    let path = profiles_dir().join("config.json");
    if let Ok(data) = std::fs::read_to_string(&path) {
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        AppConfig::default()
    }
}

fn save_config(config: &AppConfig) {
    let path = profiles_dir().join("config.json");
    if let Ok(json) = serde_json::to_string_pretty(config) {
        let _ = std::fs::write(path, json);
    }
}

struct AppState {
    profiles: HashMap<String, Profile>,
    current_profile_name: String,
    active_tab: Tab,
    theme: AppTheme,
    close_to_tray: bool,
    start_minimized: bool,
    run_at_startup: bool,
    is_paused: bool,
    connected_device: Arc<Mutex<bool>>,
    pressed_inputs: Arc<Mutex<HashMap<String, bool>>>,
    active_mapping: Arc<Mutex<HashMap<String, Vec<String>>>>,
    recording_key: Option<String>,
    recording_slot: Option<usize>,
    window_configured: bool,
    sound_enabled: bool,
    sound_enabled_atomic: Arc<AtomicBool>,
    rename_target: Option<String>,
    rename_buffer: String,
    key_press_counter: Arc<AtomicU64>,
    connection_start: Arc<Mutex<std::time::Instant>>,
    battery_info: Arc<Mutex<(u8, u8)>>,
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
            profile.total_key_presses = self.key_press_counter.load(Ordering::Relaxed);
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

impl Drop for JoyMapperApp {
    fn drop(&mut self) {
        self.state.save_to_disk();
    }
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
                        } else if let Ok(old) = serde_json::from_str::<OldProfile>(&data) {
                            let name = path.file_stem().unwrap().to_string_lossy().to_string();
                            profiles.insert(name, Profile::from(old));
                        }
                    }
                }
            }
        }

        if profiles.is_empty() {
            let mut default_mappings = HashMap::new();
            for name in ALL_INPUTS {
                default_mappings.insert(name.to_string(), default_mapping_vec());
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
                    .or_insert_with(default_mapping_vec);
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
        let connected_battery = connected_device.clone();
        let ctx_clone = cc.egui_ctx.clone();
        let ctx_battery = cc.egui_ctx.clone();

        let sound_engine = SoundEngine::new();
        let click_tx = sound_engine.sender();
        let sound_enabled = Arc::new(AtomicBool::new(true));
        let sound_enabled_poll = sound_enabled.clone();

        let key_press_counter = Arc::new(AtomicU64::new(
            profiles
                .get(&current_profile_name)
                .map_or(0, |p| p.total_key_presses),
        ));
        let key_counter_poll = key_press_counter.clone();

        let connection_start = Arc::new(Mutex::new(std::time::Instant::now()));
        let connection_start_poll = connection_start.clone();

        let battery_info = Arc::new(Mutex::new((BATTERY_UNKNOWN, u8::MAX)));
        let battery_info_poll = battery_info.clone();
        let battery_info_bluetooth = battery_info.clone();

        // Hardware Polling Thread (Full XInput coverage)
        std::thread::spawn(move || {
            const FAST_MS: u64 = 4;
            const SLOW_MS: u64 = 16;
            const IDLE_TIMEOUT: Duration = Duration::from_secs(600);

            let mut poll_interval = Duration::from_millis(FAST_MS);
            let mut last_activity = std::time::Instant::now();
            let mut was_connected = false;
            let mut last_tray_update = std::time::Instant::now();
            let mut last_battery_query = std::time::Instant::now() - Duration::from_secs(30);
            let click_tx = click_tx;
            let sound_enabled = sound_enabled_poll;
            loop {
                let (connected, buttons, lx, ly, rx, ry, lt, rt) = check_controller_full(0);

                if connected != was_connected {
                    *connected_clone.lock().unwrap() = connected;
                    was_connected = connected;
                    if connected {
                        *connection_start_poll.lock().unwrap() = std::time::Instant::now();
                    }
                    update_tray_icon(if connected { "ready" } else { "disconnected" });
                    last_tray_update = std::time::Instant::now();
                    ctx_clone.request_repaint();
                }

                if connected && last_battery_query.elapsed() > Duration::from_secs(30) {
                    let (btype, blevel) = query_battery(0);
                    let mut battery = battery_info_poll.lock().unwrap();
                    if battery.0 != BATTERY_PERCENT || btype == BATTERY_WIRED {
                        *battery = (btype, blevel);
                    }
                    last_battery_query = std::time::Instant::now();
                    ctx_clone.request_repaint();
                }

                let is_idle = last_activity.elapsed() >= IDLE_TIMEOUT;
                let target = if is_idle { SLOW_MS } else { FAST_MS };
                if poll_interval.as_millis() as u64 != target {
                    poll_interval = Duration::from_millis(target);
                }

                if connected {
                    let mut current_pressed = HashMap::new();

                    for (name, flag) in DIGITAL_MAP {
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

                    let mut state_changed = false;

                    for (name, is_pressed) in &current_pressed {
                        let was_pressed = *lock.get(name).unwrap_or(&false);
                        if *is_pressed && !was_pressed {
                            if let Some(keys_vec) = macro_map.get(name) {
                                for key_str in keys_vec {
                                    if !key_str.is_empty() {
                                        for part in key_str.split('+') {
                                            if let Some(vk) = parse_key_string(part) {
                                                simulate_key(vk, false);
                                                key_counter_poll.fetch_add(1, Ordering::Relaxed);
                                            }
                                        }
                                    }
                                }
                            }
                            if sound_enabled.load(Ordering::SeqCst) {
                                let _ = click_tx.send(());
                            }
                            state_changed = true;
                        } else if !*is_pressed && was_pressed {
                            if let Some(keys_vec) = macro_map.get(name) {
                                for key_str in keys_vec.iter().rev() {
                                    if !key_str.is_empty() {
                                        for part in key_str.split('+').rev() {
                                            if let Some(vk) = parse_key_string(part) {
                                                simulate_key(vk, true);
                                            }
                                        }
                                    }
                                }
                            }
                            state_changed = true;
                        }
                    }
                    *lock = current_pressed;

                    if state_changed {
                        last_activity = std::time::Instant::now();
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
                std::thread::sleep(poll_interval);
            }
        });

        std::thread::spawn(move || loop {
            if *connected_battery.lock().unwrap() {
                if let Some(percent) = query_bluetooth_battery_percent() {
                    let mut battery = battery_info_bluetooth.lock().unwrap();
                    if battery.0 != BATTERY_WIRED && *battery != (BATTERY_PERCENT, percent) {
                        *battery = (BATTERY_PERCENT, percent);
                        ctx_battery.request_repaint();
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(30));
        });

        let config = load_config();

        let mut app = Self {
            state: AppState {
                profiles,
                current_profile_name,
                active_tab: Tab::Mappings,
                theme: AppTheme::System,
                close_to_tray: true,
                start_minimized: config.start_minimized,
                run_at_startup: false,
                is_paused: false,
                connected_device,
                pressed_inputs,
                active_mapping,
                recording_key: None,
                recording_slot: None,
                window_configured: false,
                sound_enabled: true,
                sound_enabled_atomic: sound_enabled,
                rename_target: None,
                rename_buffer: String::new(),
                key_press_counter,
                connection_start,
                battery_info,
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
        style.visuals.window_rounding = egui::Rounding::same(8.0);
        style.visuals.widgets.noninteractive.rounding = egui::Rounding::same(4.0);
        style.visuals.widgets.inactive.rounding = egui::Rounding::same(4.0);
        style.visuals.widgets.hovered.rounding = egui::Rounding::same(4.0);
        style.visuals.widgets.active.rounding = egui::Rounding::same(4.0);
        style.spacing.button_padding = egui::vec2(14.0, 7.0);
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.menu_margin = egui::Margin::same(8.0);
        style.spacing.indent = 16.0;

        // Windows 11 accent — blue highlight
        let colors = win11_colors(is_dark);
        style.visuals.selection.bg_fill = colors.accent;
        style.visuals.hyperlink_color = colors.accent;
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
            Color32::from_rgba_unmultiplied(255, 255, 255, 252)
        };
        style.visuals.window_fill = base_bg;
        style.visuals.panel_fill = Color32::TRANSPARENT;

        // Subtle border to distinguish panels
        style.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, colors.border);
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, colors.border);

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
            if self.state.start_minimized {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }

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
                self.state.recording_slot = None;
            } else {
                let slot = self.state.recording_slot.take().unwrap_or(0);
                if let Some(keys_vec) = self
                    .state
                    .profiles
                    .get_mut(&self.state.current_profile_name)
                    .unwrap()
                    .mappings
                    .get_mut(&k)
                {
                    if slot < keys_vec.len() {
                        keys_vec[slot] = macro_str;
                    } else {
                        keys_vec.push(macro_str);
                    }
                }
                self.state.save_to_disk();
                self.state.recording_key = None;
                ctx.request_repaint();
            }
        }

        // --- 3. UI RENDERING ---
        let colors = win11_colors(is_dark);
        let connected = *self.state.connected_device.lock().unwrap();
        let status_text = if self.state.is_paused {
            "Paused"
        } else if connected {
            "Controller connected"
        } else {
            "No controller"
        };
        let status_color = if self.state.is_paused {
            colors.warning
        } else if connected {
            colors.success
        } else {
            colors.danger
        };

        egui::TopBottomPanel::top("top_panel")
            .frame(
                egui::Frame::none()
                    .fill(colors.app_bg)
                    .inner_margin(egui::Margin::symmetric(18.0, 14.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("JoyMapper Pro")
                                .size(22.0)
                                .strong()
                                .color(colors.text),
                        );
                        ui.label(
                            egui::RichText::new("XInput to keyboard profiles")
                                .size(12.0)
                                .color(colors.muted),
                        );
                    });

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::Frame::none()
                            .fill(colors.surface_alt)
                            .stroke(egui::Stroke::new(1.0, colors.border))
                            .rounding(egui::Rounding::same(4.0))
                            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.colored_label(status_color, "o");
                                    ui.label(egui::RichText::new(status_text).color(colors.text));
                                });
                            });
                    });
                });

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    let tabs = [("Mappings", Tab::Mappings), ("Settings", Tab::Settings)];
                    for (label, tab) in &tabs {
                        let selected = self.state.active_tab == *tab;
                        let text_color = if selected { egui::Color32::WHITE } else { colors.text };
                        let bg = if selected { colors.accent } else { egui::Color32::TRANSPARENT };
                            if ui.add(egui::Button::new(egui::RichText::new(*label).color(text_color)).fill(bg)).clicked() {
                            self.state.active_tab = *tab;
                        }
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(colors.app_bg)
                    .inner_margin(egui::Margin::symmetric(18.0, 10.0)),
            )
            .show(ctx, |ui| {
                match self.state.active_tab {
                    Tab::Mappings => {
                        win11_card_frame(&colors).show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new("Profile").color(colors.muted));
                                egui::ComboBox::from_id_source("profile_select")
                                    .selected_text(egui::RichText::new(&self.state.current_profile_name).color(colors.text))
                                    .show_ui(ui, |ui| {
                                        let names: Vec<_> = self.state.profiles.keys().cloned().collect();
                                        for name in names {
                                            if ui
                                                .selectable_value(&mut self.state.current_profile_name, name.clone(), name)
                                                .clicked()
                                            {
                                                self.state.save_to_disk();
                                            }
                                        }
                                    });

                                if self.state.profiles.len() > 1 && ui.button("Rename").clicked() {
                                    self.state.rename_target = Some(self.state.current_profile_name.clone());
                                    self.state.rename_buffer = self.state.current_profile_name.clone();
                                }

                                if ui.button("Import").clicked() {
                                    if let Some(path) = FileDialog::new().add_filter("JSON", &["json"]).pick_file() {
                                        if let Ok(content) = std::fs::read_to_string(&path) {
                                            let profile = serde_json::from_str::<Profile>(&content)
                                                .or_else(|_| serde_json::from_str::<OldProfile>(&content).map(Profile::from));
                                            if let Ok(profile) = profile {
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

                                if ui.button("Export").clicked() {
                                    if let Some(path) = FileDialog::new().add_filter("JSON", &["json"]).save_file() {
                                        let profile = self.state.profiles.get(&self.state.current_profile_name).unwrap();
                                        let json = serde_json::to_string_pretty(profile).unwrap();
                                        let _ = std::fs::write(path, json);
                                    }
                                }

                                if ui.button("New").clicked() {
                                    let mut new_mappings = HashMap::new();
                                    for name in ALL_INPUTS {
                                        new_mappings.insert(name.to_string(), default_mapping_vec());
                                    }
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

                                if self.state.profiles.len() > 1 && ui.button("Delete").clicked() {
                                    let dir = profiles_dir();
                                    let path = dir.join(format!("{}.json", self.state.current_profile_name));
                                    let _ = std::fs::remove_file(&path);
                                    self.state.profiles.remove(&self.state.current_profile_name);
                                    self.state.current_profile_name = self.state.profiles.keys().next().unwrap().clone();
                                    self.state.save_to_disk();
                                }

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let label = if self.state.is_paused { "Resume" } else { "Pause" };
                                    if ui
                                        .add(egui::Button::new(label).fill(if self.state.is_paused { colors.accent } else { colors.surface_alt }))
                                        .clicked()
                                    {
                                        self.state.is_paused = !self.state.is_paused;
                                    }
                                });
                            });
                        });

                        if let Some(ref rec_key) = self.state.recording_key {
                            ui.add_space(10.0);
                            egui::Frame::none()
                                .fill(if is_dark {
                                    Color32::from_rgba_unmultiplied(80, 54, 0, 230)
                                } else {
                                    Color32::from_rgba_unmultiplied(255, 244, 206, 240)
                                })
                                .stroke(egui::Stroke::new(1.0, colors.warning))
                                .rounding(egui::Rounding::same(8.0))
                                .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                                .show(ui, |ui| {
                                    ui.colored_label(
                                        colors.warning,
                                        format!("Recording {}. Press a key or combo, Esc cancels.", input_display_name(rec_key)),
                                    );
                                });
                        }

                        ui.add_space(10.0);
                        let mut needs_save = false;
                        let mut recording_click: Option<(String, usize)> = None;

                        let pressed_mask: Vec<bool> = {
                            let lock = self.state.pressed_inputs.lock().unwrap();
                            ALL_INPUTS.iter().map(|name| *lock.get(*name).unwrap_or(&false)).collect()
                        };
                        let recording_name = self.state.recording_key.clone();
                        let recording_slot = self.state.recording_slot;

                        win11_card_frame(&colors).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_sized([150.0, 20.0], egui::Label::new(egui::RichText::new("Input").small().color(colors.muted)));
                                ui.label(egui::RichText::new("Mapped keys").small().color(colors.muted));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(egui::RichText::new("Action").small().color(colors.muted));
                                });
                            });
                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(2.0);

                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    for (row_idx, input_name) in ALL_INPUTS.iter().enumerate() {
                                        let is_pressed = pressed_mask[row_idx];
                                        let row_fill = if is_pressed {
                                            colors.row_active
                                        } else if row_idx % 2 == 0 {
                                            Color32::TRANSPARENT
                                        } else {
                                            colors.row_hover
                                        };

                                        egui::Frame::none()
                                            .fill(row_fill)
                                            .rounding(egui::Rounding::same(6.0))
                                            .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                                            .show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.allocate_ui(egui::vec2(150.0, 30.0), |ui| {
                                                        let text_color = if is_pressed { colors.accent } else { colors.text };
                                                        ui.label(egui::RichText::new(input_display_name(input_name)).strong().color(text_color));
                                                    });

                                                    let key_slots = self.state.profiles.get_mut(&self.state.current_profile_name)
                                                        .unwrap()
                                                        .mappings
                                                        .get_mut(*input_name)
                                                        .unwrap();

                                                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center).with_main_wrap(true), |ui| {
                                                        let slots_count = key_slots.len();

                                                        for idx in 0..slots_count {
                                                            let current_val = &key_slots[idx];
                                                            let display = get_display_name(current_val);
                                                            let is_recording = recording_name.as_deref() == Some(*input_name) && recording_slot == Some(idx);
                                                            let chip_fill = if is_recording {
                                                                colors.warning
                                                            } else if current_val.is_empty() {
                                                                colors.empty_chip
                                                            } else {
                                                                colors.accent_soft
                                                            };
                                                            let chip_text = if is_recording {
                                                                Color32::BLACK
                                                            } else if current_val.is_empty() {
                                                                colors.muted
                                                            } else {
                                                                colors.text
                                                            };
                                                            let chip_stroke = if is_recording {
                                                                egui::Stroke::new(1.5, colors.warning)
                                                            } else if current_val.is_empty() {
                                                                egui::Stroke::new(1.0, colors.border)
                                                            } else {
                                                                egui::Stroke::new(1.0, colors.accent)
                                                            };

                                                            let cell_btn = ui.add(
                                                                egui::Button::new(egui::RichText::new(display).monospace().color(chip_text))
                                                                    .fill(chip_fill)
                                                                    .stroke(chip_stroke)
                                                                    .min_size(egui::vec2(56.0, 28.0)),
                                                            );

                                                            if cell_btn.clicked() {
                                                                recording_click = Some((input_name.to_string(), idx));
                                                                ctx.request_repaint();
                                                            }

                                                            cell_btn.context_menu(|ui| {
                                                                ui.set_min_width(220.0);
                                                                ui.label(egui::RichText::new("Assign target key").strong());
                                                                ui.separator();

                                                                ui.menu_button("Letters A-M", |ui| {
                                                                    for c in b'A'..=b'M' {
                                                                        let ch = (c as char).to_string();
                                                                        if ui.button(&ch).clicked() {
                                                                            key_slots[idx] = ch.to_lowercase();
                                                                            needs_save = true;
                                                                            ui.close_menu();
                                                                        }
                                                                    }
                                                                });

                                                                ui.menu_button("Letters N-Z", |ui| {
                                                                    for c in b'N'..=b'Z' {
                                                                        let ch = (c as char).to_string();
                                                                        if ui.button(&ch).clicked() {
                                                                            key_slots[idx] = ch.to_lowercase();
                                                                            needs_save = true;
                                                                            ui.close_menu();
                                                                        }
                                                                    }
                                                                });

                                                                ui.menu_button("Numbers 0-9", |ui| {
                                                                    for n in b'0'..=b'9' {
                                                                        let num_str = (n as char).to_string();
                                                                        if ui.button(&num_str).clicked() {
                                                                            key_slots[idx] = num_str;
                                                                            needs_save = true;
                                                                            ui.close_menu();
                                                                        }
                                                                    }
                                                                });

                                                                ui.menu_button("Symbols", |ui| {
                                                                    let syms = [
                                                                        ("[", "lbracket"), ("{", "lbracket"),
                                                                        ("]", "rbracket"), ("}", "rbracket"),
                                                                        ("\\", "backslash"), ("|", "backslash"),
                                                                        (";", "semicolon"), (":", "semicolon"),
                                                                        ("/", "slash"), ("?", "slash"),
                                                                        ("`", "backtick"), ("~", "backtick"),
                                                                        ("'", "quote"), ("\"", "quote"),
                                                                        (",", "comma"), ("<", "comma"),
                                                                        (".", "period"), (">", "period"),
                                                                        ("-", "minus"), ("_", "minus"),
                                                                        ("=", "plus"),
                                                                        ("!", "exclaim"), ("@", "at"), ("#", "hash"),
                                                                        ("$", "dollar"), ("%", "percent"), ("^", "caret"),
                                                                        ("&", "ampersand"), ("*", "asterisk"),
                                                                        ("(", "("), (")", ")"),
                                                                    ];
                                                                    for (disp, name) in syms {
                                                                        if ui.button(format!("{} ({})", disp, name)).clicked() {
                                                                            key_slots[idx] = name.to_string();
                                                                            needs_save = true;
                                                                            ui.close_menu();
                                                                        }
                                                                    }
                                                                });

                                                                ui.menu_button("Modifiers and controls", |ui| {
                                                                    let controls = ["Ctrl", "Shift", "Alt", "Win", "Space", "Enter", "Tab", "Backspace", "Delete", "Escape"];
                                                                    for ctrl in controls {
                                                                        if ui.button(ctrl).clicked() {
                                                                            key_slots[idx] = ctrl.to_lowercase();
                                                                            needs_save = true;
                                                                            ui.close_menu();
                                                                        }
                                                                    }
                                                                });

                                                                ui.menu_button("Function keys", |ui| {
                                                                    ui.horizontal_wrapped(|ui| {
                                                                        for f in 1..=12 {
                                                                            let f_str = format!("F{}", f);
                                                                            if ui.button(&f_str).clicked() {
                                                                                key_slots[idx] = f_str.to_lowercase();
                                                                                needs_save = true;
                                                                                ui.close_menu();
                                                                            }
                                                                        }
                                                                    });
                                                                });

                                                                ui.menu_button("Browser", |ui| {
                                                                    let browser_keys = ["Browser Back", "Browser Fwd", "Browser Refresh", "Browser Stop", "Browser Search", "Browser Fav", "Browser Home"];
                                                                    let browser_tokens = ["browser_back", "browser_forward", "browser_refresh", "browser_stop", "browser_search", "browser_fav", "browser_home"];
                                                                    for (disp, tok) in browser_keys.iter().zip(browser_tokens.iter()) {
                                                                        if ui.button(*disp).clicked() {
                                                                            key_slots[idx] = tok.to_string();
                                                                            needs_save = true;
                                                                            ui.close_menu();
                                                                        }
                                                                    }
                                                                });

                                                                ui.menu_button("Media", |ui| {
                                                                    let media_keys = ["Vol Mute", "Vol Down", "Vol Up", "Next Track", "Prev Track", "Media Stop", "Play/Pause"];
                                                                    let media_tokens = ["vol_mute", "vol_down", "vol_up", "media_next", "media_prev", "media_stop", "media_play_pause"];
                                                                    for (disp, tok) in media_keys.iter().zip(media_tokens.iter()) {
                                                                        if ui.button(*disp).clicked() {
                                                                            key_slots[idx] = tok.to_string();
                                                                            needs_save = true;
                                                                            ui.close_menu();
                                                                        }
                                                                    }
                                                                });

                                                                ui.menu_button("Apps and launchers", |ui| {
                                                                    let app_keys = ["Launch Mail", "Launch Media", "Launch PC", "Launch Calc"];
                                                                    let app_tokens = ["launch_mail", "launch_media", "launch_pc", "launch_calc"];
                                                                    for (disp, tok) in app_keys.iter().zip(app_tokens.iter()) {
                                                                        if ui.button(*disp).clicked() {
                                                                            key_slots[idx] = tok.to_string();
                                                                            needs_save = true;
                                                                            ui.close_menu();
                                                                        }
                                                                    }
                                                                });

                                                                ui.separator();
                                                                if ui.button("Clear slot").clicked() {
                                                                    key_slots[idx] = "".to_string();
                                                                    needs_save = true;
                                                                    ui.close_menu();
                                                                }
                                                            });
                                                        }

                                                        if slots_count < 10 {
                                                            if ui
                                                                .add(egui::Button::new("+").fill(colors.surface_alt).min_size(egui::vec2(30.0, 28.0)))
                                                                .on_hover_text("Add simultaneous key slot")
                                                                .clicked()
                                                            {
                                                                key_slots.push("".to_string());
                                                                needs_save = true;
                                                            }
                                                        }
                                                    });

                                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                                        if ui
                                                            .add(egui::Button::new("Reset").fill(colors.surface_alt).min_size(egui::vec2(64.0, 28.0)))
                                                            .on_hover_text("Reset to a single unmapped slot")
                                                            .clicked()
                                                        {
                                                            key_slots.clear();
                                                            key_slots.push("".to_string());
                                                            needs_save = true;
                                                        }
                                                    });
                                                });
                                            });
                                        ui.add_space(2.0);
                                    }
                                });
                        });

                        if let Some((name, slot)) = recording_click {
                            self.state.recording_key = Some(name);
                            self.state.recording_slot = Some(slot);
                            ctx.request_repaint();
                        }
                        if needs_save {
                            self.state.save_to_disk();
                        }
                    }

                    Tab::Settings => {
                        let connected = *self.state.connected_device.lock().unwrap();

                        win11_card_frame(&colors).show(ui, |ui| {
                            ui.label(egui::RichText::new("General").size(16.0).strong().color(colors.text));
                            ui.add_space(8.0);
                            ui.checkbox(&mut self.state.close_to_tray, "Close button minimizes to system tray");
                            if ui.checkbox(&mut self.state.start_minimized, "Start minimized to system tray").changed() {
                                save_config(&AppConfig { start_minimized: self.state.start_minimized });
                            }
                            if ui.checkbox(&mut self.state.run_at_startup, "Start with Windows").changed() {
                                self.toggle_startup(self.state.run_at_startup);
                            }

                            if ui.checkbox(&mut self.state.sound_enabled, "Play sound on controller press").changed() {
                                self.state.sound_enabled_atomic.store(self.state.sound_enabled, Ordering::SeqCst);
                            }

                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                ui.label("Theme");
                                egui::ComboBox::from_id_source("theme_select")
                                    .selected_text(match self.state.theme {
                                        AppTheme::System => "Use Windows setting",
                                        AppTheme::Light => "Light",
                                        AppTheme::Dark => "Dark",
                                    })
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(&mut self.state.theme, AppTheme::System, "Use Windows setting");
                                        ui.selectable_value(&mut self.state.theme, AppTheme::Light, "Light");
                                        ui.selectable_value(&mut self.state.theme, AppTheme::Dark, "Dark");
                                    });
                            });
                        });

                        ui.add_space(10.0);
                        win11_card_frame(&colors).show(ui, |ui| {
                            ui.label(egui::RichText::new("Hardware").size(16.0).strong().color(colors.text));
                            ui.add_space(8.0);
                            if connected {
                                ui.colored_label(colors.success, "Connected - controller active through XInput");
                                ui.label(egui::RichText::new("Controller 0 | polling at 1000 Hz | XInput engine").color(colors.muted));
                                let elapsed = self.state.connection_start.lock().unwrap().elapsed();
                                let secs = elapsed.as_secs();
                                let mins = secs / 60;
                                let hrs = mins / 60;
                                if hrs > 0 {
                                    ui.label(format!("Connected for {}h {}m {}s", hrs, mins % 60, secs % 60));
                                } else if mins > 0 {
                                    ui.label(format!("Connected for {}m {}s", mins, secs % 60));
                                } else {
                                    ui.label(format!("Connected for {}s", secs));
                                }

                                let (btype, blevel) = *self.state.battery_info.lock().unwrap();
                                if btype == BATTERY_WIRED {
                                    ui.label("Power: wired");
                                } else if btype == BATTERY_PERCENT {
                                    let color = if blevel <= 15 {
                                        colors.danger
                                    } else if blevel <= 35 {
                                        colors.warning
                                    } else {
                                        colors.success
                                    };
                                    ui.colored_label(color, format!("Battery: {}% (Bluetooth)", blevel));
                                } else if btype == BATTERY_DISCONNECTED {
                                    ui.label("Battery: disconnected");
                                } else if btype == BATTERY_ALKALINE || btype == BATTERY_NIMH {
                                    let kind = if btype == BATTERY_NIMH { "NiMH" } else { "Alkaline" };
                                    let (color, level_str) = match blevel {
                                        0 => (colors.danger, "Empty"),
                                        1 => (colors.warning, "Low"),
                                        2 => (colors.warning, "Medium"),
                                        _ => (colors.success, "Full"),
                                    };
                                    ui.colored_label(color, format!("Battery: {} ({})", level_str, kind));
                                } else if btype == BATTERY_UNKNOWN && blevel <= 3 {
                                    let (color, level_str) = match blevel {
                                        0 => (colors.danger, "Empty"),
                                        1 => (colors.warning, "Low"),
                                        2 => (colors.warning, "Medium"),
                                        _ => (colors.success, "Full"),
                                    };
                                    ui.colored_label(color, format!("Battery: {} (wireless)", level_str));
                                } else {
                                    ui.label("Battery: unknown");
                                }
                            } else {
                                ui.colored_label(colors.danger, "Disconnected - no controller detected");
                                ui.label(egui::RichText::new("Plug in an XInput controller to begin.").color(colors.muted));
                            }
                        });

                        ui.add_space(10.0);
                        win11_card_frame(&colors).show(ui, |ui| {
                            ui.label(egui::RichText::new("Statistics").size(16.0).strong().color(colors.text));
                            ui.add_space(8.0);
                            let count = self.state.key_press_counter.load(Ordering::Relaxed);
                            ui.label(format!("Total key presses recorded: {}", count));
                            ui.label(egui::RichText::new("Saved to the active profile on exit.").color(colors.muted));
                        });
                    }
                }
            });

        if let Some(ref target) = self.state.rename_target.clone() {
            let mut open = true;
            egui::Window::new("Rename Profile")
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .fixed_size([340.0, 132.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Profile name");
                    if ui
                        .text_edit_singleline(&mut self.state.rename_buffer)
                        .lost_focus()
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        let new_name = self.state.rename_buffer.trim().to_string();
                        if !new_name.is_empty() && !self.state.profiles.contains_key(&new_name) {
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

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.state.rename_target = None;
                        }
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
    let config = load_config();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 640.0])
            .with_min_inner_size([620.0, 520.0])
            .with_decorations(true)
            .with_transparent(true)
            .with_visible(!config.start_minimized),
        persist_window: true,
        ..Default::default()
    };

    eframe::run_native(
        "JoyMapper Pro",
        options,
        Box::new(|cc| Box::new(JoyMapperApp::new(cc))),
    )
}
