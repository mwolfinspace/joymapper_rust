//! In-app auto-updater.
//!
//! Flow: ask the public GitHub release feed for the newest release, compare it
//! with the running build, let the user confirm, stream the new `.exe` next to
//! the current one, then hand the actual swap to a tiny detached script that
//! waits for both app processes to exit before replacing the file and starting
//! it again.
//!
//! GitHub is used because it is the public mirror: the Gitea instance requires
//! authentication even for its read-only API, which end users do not have.

use serde::Deserialize;
use std::ffi::c_void;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use windows_sys::Win32::Networking::WinHttp::*;
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

const GITHUB_API: &str = "https://api.github.com/repos/mwolfinspace/joymapper_rust/releases/latest";
/// Rate-limit-free fallback: GitHub's public release feed. The asset URL can be
/// built from the tag, so this still works when the API returns HTTP 403.
const GITHUB_FEED: &str = "https://github.com/mwolfinspace/joymapper_rust/releases.atom";
const ASSET_NAME: &str = "joymapper_rust.exe";
const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

// ---------------------------------------------------------------------------
// Release metadata
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub notes: String,
    pub url: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// State shared between the UI thread and the background worker.
#[derive(Clone)]
pub enum UpdateState {
    Idle,
    Checking,
    Available(Release),
    Downloading { version: String, progress: f32 },
    Ready,
    UpToDate,
    Error(String),
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// True when the automatic check has not run in the last 24 hours.
pub fn check_due() -> bool {
    let last = crate::load_config().last_update_check;
    now_secs().saturating_sub(last) >= CHECK_INTERVAL_SECS
}

pub fn mark_checked() {
    crate::update_config(|cfg| cfg.last_update_check = now_secs());
}

fn parse_version(v: &str) -> Vec<u64> {
    v.trim()
        .trim_start_matches(['v', 'V'])
        .split('.')
        .map(|part| {
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().unwrap_or(0)
        })
        .collect()
}

pub fn is_newer(remote: &str, current: &str) -> bool {
    let r = parse_version(remote);
    let c = parse_version(current);
    let len = r.len().max(c.len());
    for i in 0..len {
        let a = *r.get(i).unwrap_or(&0);
        let b = *c.get(i).unwrap_or(&0);
        if a != b {
            return a > b;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Minimal WinHTTP client (no extra crates, uses the Windows TLS stack)
// ---------------------------------------------------------------------------

struct WinHttpHandle(*mut c_void);

impl Drop for WinHttpHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                WinHttpCloseHandle(self.0);
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn os_error(what: &str) -> String {
    format!("{}: {}", what, std::io::Error::last_os_error())
}

fn open_request(url: &str) -> Result<(WinHttpHandle, WinHttpHandle, WinHttpHandle), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| "update URL is not https".to_string())?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };

    let agent = wide("JoyMapperPro-Updater/1.0");
    let host_w = wide(host);
    let path_w = wide(path);

    unsafe {
        let session = WinHttpOpen(
            agent.as_ptr(),
            WINHTTP_ACCESS_TYPE_DEFAULT_PROXY,
            std::ptr::null(),
            std::ptr::null(),
            0,
        );
        if session.is_null() {
            return Err(os_error("WinHttpOpen"));
        }
        let session = WinHttpHandle(session);

        // Fail fast instead of hanging the worker thread forever.
        WinHttpSetTimeouts(session.0, 8_000, 10_000, 15_000, 30_000);

        let connect = WinHttpConnect(session.0, host_w.as_ptr(), 443, 0);
        if connect.is_null() {
            return Err(os_error("WinHttpConnect"));
        }
        let connect = WinHttpHandle(connect);

        let request = WinHttpOpenRequest(
            connect.0,
            windows_sys::w!("GET"),
            path_w.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            return Err(os_error("WinHttpOpenRequest"));
        }

        Ok((session, connect, WinHttpHandle(request)))
    }
}

fn send_request(request: *mut c_void) -> Result<u32, String> {
    let headers = wide(
        "Accept: application/vnd.github+json\r\n\
         User-Agent: JoyMapperPro-Updater\r\n\
         X-GitHub-Api-Version: 2022-11-28\r\n",
    );

    unsafe {
        // u32::MAX == -1: the header block is null-terminated.
        if WinHttpSendRequest(
            request,
            headers.as_ptr(),
            u32::MAX,
            std::ptr::null(),
            0,
            0,
            0,
        ) == 0
        {
            return Err(os_error("WinHttpSendRequest"));
        }
        if WinHttpReceiveResponse(request, std::ptr::null_mut()) == 0 {
            return Err(os_error("WinHttpReceiveResponse"));
        }

        let mut status: u32 = 0;
        let mut len = std::mem::size_of::<u32>() as u32;
        if WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            std::ptr::null(),
            &mut status as *mut u32 as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(os_error("WinHttpQueryHeaders(status)"));
        }
        Ok(status)
    }
}

fn content_length(request: *mut c_void) -> Option<u64> {
    let mut len: u64 = 0;
    let mut size = std::mem::size_of::<u64>() as u32;
    let ok = unsafe {
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER64,
            std::ptr::null(),
            &mut len as *mut u64 as *mut c_void,
            &mut size,
            std::ptr::null_mut(),
        )
    };
    if ok != 0 {
        Some(len)
    } else {
        None
    }
}

fn read_chunk(request: *mut c_void, buf: &mut [u8]) -> Result<usize, String> {
    let mut read: u32 = 0;
    let ok = unsafe {
        WinHttpReadData(
            request,
            buf.as_mut_ptr() as *mut c_void,
            buf.len() as u32,
            &mut read,
        )
    };
    if ok == 0 {
        return Err(os_error("WinHttpReadData"));
    }
    Ok(read as usize)
}

fn available_bytes(request: *mut c_void) -> Result<u32, String> {
    let mut avail: u32 = 0;
    let ok = unsafe { WinHttpQueryDataAvailable(request, &mut avail) };
    if ok == 0 {
        return Err(os_error("WinHttpQueryDataAvailable"));
    }
    Ok(avail)
}

fn http_get(url: &str, limit: usize) -> Result<(u32, Vec<u8>), String> {
    let (_session, _connect, request) = open_request(url)?;
    let status = send_request(request.0)?;

    let mut body = Vec::new();
    loop {
        let avail = available_bytes(request.0)?;
        if avail == 0 {
            break;
        }
        let want = avail.min(64 * 1024) as usize;
        let mut buf = vec![0u8; want];
        let read = read_chunk(request.0, &mut buf)?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&buf[..read]);
        if body.len() > limit {
            return Err("server response is too large".to_string());
        }
    }
    Ok((status, body))
}

fn download(
    url: &str,
    dest: &Path,
    size_hint: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let (_session, _connect, request) = open_request(url)?;
    let status = send_request(request.0)?;
    if status != 200 {
        return Err(format!("download failed (HTTP {})", status));
    }

    let total = content_length(request.0)
        .filter(|n| *n > 0)
        .unwrap_or(size_hint);

    let mut file = std::fs::File::create(dest)
        .map_err(|e| format!("cannot create {}: {}", dest.display(), e))?;

    let mut done: u64 = 0;
    loop {
        let avail = available_bytes(request.0)?;
        if avail == 0 {
            break;
        }
        let want = avail.min(64 * 1024) as usize;
        let mut buf = vec![0u8; want];
        let read = read_chunk(request.0, &mut buf)?;
        if read == 0 {
            break;
        }
        file.write_all(&buf[..read])
            .map_err(|e| format!("cannot write update: {}", e))?;
        done += read as u64;
        progress(done, total);
    }
    file.flush()
        .map_err(|e| format!("cannot flush update: {}", e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Checking
// ---------------------------------------------------------------------------

fn check_api() -> Result<Option<Release>, String> {
    let (status, body) = http_get(GITHUB_API, 1024 * 1024)?;
    if status == 404 {
        return Ok(None);
    }
    if status != 200 {
        return Err(format!("GitHub returned HTTP {}", status));
    }

    let api: ApiRelease =
        serde_json::from_slice(&body).map_err(|e| format!("unexpected release response: {}", e))?;

    let version = api.tag_name.trim_start_matches(['v', 'V']).to_string();
    if !is_newer(&version, CURRENT_VERSION) {
        return Ok(None);
    }

    let asset = api
        .assets
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(ASSET_NAME))
        .or_else(|| {
            api.assets
                .iter()
                .find(|a| a.name.to_ascii_lowercase().ends_with(".exe"))
        })
        .ok_or_else(|| "the latest release has no .exe download".to_string())?;

    Ok(Some(Release {
        version,
        notes: api.body.unwrap_or_default(),
        url: asset.browser_download_url.clone(),
        size: asset.size,
    }))
}

fn check_feed_for(current: &str) -> Result<Option<Release>, String> {
    let (status, body) = http_get(GITHUB_FEED, 2 * 1024 * 1024)?;
    if status != 200 {
        return Err(format!("GitHub release feed returned HTTP {}", status));
    }
    let text = String::from_utf8_lossy(&body);
    let marker = "/releases/tag/";
    let start = text
        .find(marker)
        .ok_or_else(|| "the release feed has no entries".to_string())?
        + marker.len();
    let rest = &text[start..];
    let end = rest
        .find('"')
        .ok_or_else(|| "malformed release feed".to_string())?;
    let tag = &rest[..end];
    let version = tag.trim_start_matches(['v', 'V']).to_string();
    if !is_newer(&version, current) {
        return Ok(None);
    }
    Ok(Some(Release {
        version,
        notes: String::new(),
        url: format!(
            "https://github.com/mwolfinspace/joymapper_rust/releases/download/{}/{}",
            tag, ASSET_NAME
        ),
        size: 0,
    }))
}

fn check_feed() -> Result<Option<Release>, String> {
    check_feed_for(CURRENT_VERSION)
}

pub fn check() -> Result<Option<Release>, String> {
    match check_api() {
        Ok(result) => Ok(result),
        // Unauthenticated API calls share an hourly quota per IP; the atom feed
        // is not rate-limited, so a busy network can still receive updates.
        Err(api_error) => {
            check_feed().map_err(|feed_error| format!("{} (fallback: {})", api_error, feed_error))
        }
    }
}

/// Check, record the timestamp, and report whether a newer build exists.
pub fn check_and_mark() -> Result<Option<Release>, String> {
    let result = check();
    if result.is_ok() {
        mark_checked();
    }
    result
}

// ---------------------------------------------------------------------------
// Installing
// ---------------------------------------------------------------------------

/// The swap script waits for every running instance (UI + tray) to exit, moves
/// the new exe into place, starts the new build, then deletes itself.
/// tasklist/find/ping use absolute paths so a stripped-down PATH cannot break
/// the update.
fn swap_script(file_name: &str, staged: &Path, exe: &Path, restart_ui: bool) -> String {
    let restart_arg = if restart_ui { " --ui" } else { "" };
    format!(
        "@echo off\r\n\
         setlocal\r\n\
         set tries=0\r\n\
         :wait\r\n\
         \"%SystemRoot%\\System32\\tasklist.exe\" /FI \"IMAGENAME eq {name}\" 2>nul | \"%SystemRoot%\\System32\\find.exe\" /I \"{name}\" >nul\r\n\
         if not errorlevel 1 (\r\n\
           \"%SystemRoot%\\System32\\ping.exe\" -n 2 127.0.0.1 >nul\r\n\
           goto wait\r\n\
         )\r\n\
         :retry\r\n\
         move /Y \"{staged}\" \"{exe}\" >nul 2>&1\r\n\
         if not errorlevel 1 goto done\r\n\
         set /a tries+=1\r\n\
         if %tries% GEQ 10 exit /b 1\r\n\
         \"%SystemRoot%\\System32\\ping.exe\" -n 2 127.0.0.1 >nul\r\n\
         goto retry\r\n\
         :done\r\n\
         start \"\" \"{exe}\"{arg}\r\n\
         (goto) 2>nul & del \"%~f0\"\r\n",
        name = file_name,
        staged = staged.display(),
        exe = exe.display(),
        arg = restart_arg,
    )
}

/// Download `release` next to the running exe and launch a detached script that
/// swaps the file and restarts the app once every instance has exited.
pub fn install(
    release: &Release,
    restart_ui: bool,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot locate the app: {}", e))?;
    let dir = exe
        .parent()
        .ok_or_else(|| "cannot locate the app folder".to_string())?;
    let file_name = exe
        .file_name()
        .ok_or_else(|| "cannot locate the app name".to_string())?
        .to_string_lossy()
        .to_string();

    let staged = dir.join(format!("{}.new", file_name));
    let _ = std::fs::remove_file(&staged);

    if let Err(e) = download(&release.url, &staged, release.size, progress) {
        let _ = std::fs::remove_file(&staged);
        return Err(e);
    }

    // Refuse anything that is not a Windows executable; this also catches
    // captive-portal HTML pages that return HTTP 200.
    let meta = std::fs::metadata(&staged).map_err(|e| format!("cannot read update: {}", e))?;
    if meta.len() < 512 * 1024 {
        let _ = std::fs::remove_file(&staged);
        return Err("the downloaded update is too small to be valid".to_string());
    }
    let mut magic = [0u8; 2];
    std::fs::File::open(&staged)
        .and_then(|mut f| f.read_exact(&mut magic))
        .map_err(|e| format!("cannot read update: {}", e))?;
    if &magic != b"MZ" {
        let _ = std::fs::remove_file(&staged);
        return Err("the downloaded file is not a Windows executable".to_string());
    }

    let script = std::env::temp_dir().join("JoyMapperPro-update.cmd");
    let body = swap_script(&file_name, &staged, &exe, restart_ui);

    std::fs::write(&script, body)
        .map_err(|e| format!("cannot prepare the update script: {}", e))?;

    std::process::Command::new("cmd")
        .arg("/C")
        .arg(&script)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("cannot start the update script: {}", e))?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Cross-process shutdown (UI + tray must both release the exe before the swap)
// ---------------------------------------------------------------------------

static QUIT_EVENT: AtomicUsize = AtomicUsize::new(0);

pub fn init_quit_event() {
    unsafe {
        let handle = CreateEventW(
            std::ptr::null_mut(),
            1, // manual reset: every process gets woken
            0,
            windows_sys::w!("JoyMapperProQuitEvent"),
        );
        if !handle.is_null() {
            QUIT_EVENT.store(handle as usize, Ordering::SeqCst);
        }
    }
}

fn quit_handle() -> *mut c_void {
    if QUIT_EVENT.load(Ordering::SeqCst) == 0 {
        init_quit_event();
    }
    QUIT_EVENT.load(Ordering::SeqCst) as *mut c_void
}

pub fn quit_handle_raw() -> *mut c_void {
    quit_handle()
}

pub fn request_quit() {
    unsafe {
        let handle = quit_handle();
        if !handle.is_null() {
            SetEvent(handle);
        }
    }
}

pub fn quit_requested() -> bool {
    let handle = quit_handle();
    !handle.is_null() && unsafe { WaitForSingleObject(handle, 0) == 0 }
}

// ---------------------------------------------------------------------------
// Native prompts for tray mode (no egui window available there)
// ---------------------------------------------------------------------------

fn message_box(text: &str, title: &str, flags: u32) -> i32 {
    let text_w = wide(text);
    let title_w = wide(title);
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text_w.as_ptr(),
            title_w.as_ptr(),
            flags,
        )
    }
}

/// Ask the user (tray mode) whether to install `release`.
pub fn ask_install(release: &Release) -> bool {
    let notes: String = release.notes.chars().take(600).collect();
    let text = if notes.trim().is_empty() {
        format!(
            "JoyMapper Pro v{} is available (you have v{}).\n\n\
             Download and install it now?\nThe app will restart automatically.",
            release.version, CURRENT_VERSION
        )
    } else {
        format!(
            "JoyMapper Pro v{} is available (you have v{}).\n\n{}\n\n\
             Download and install it now?\nThe app will restart automatically.",
            release.version,
            CURRENT_VERSION,
            notes.trim()
        )
    };
    message_box(
        &text,
        "JoyMapper Pro Update",
        MB_YESNO | MB_ICONQUESTION | MB_TOPMOST | MB_SETFOREGROUND,
    ) == IDYES
}

pub fn show_error(text: &str) {
    message_box(
        text,
        "JoyMapper Pro Update",
        MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND,
    );
}

/// Background update flow for the tray process.
pub fn tray_auto_check(auto: bool) {
    if auto && !check_due() {
        return;
    }
    let release = match check_and_mark() {
        Ok(Some(release)) => release,
        Ok(None) => {
            if !auto {
                message_box(
                    &format!("JoyMapper Pro v{} is up to date.", CURRENT_VERSION),
                    "JoyMapper Pro Update",
                    MB_OK | MB_ICONINFORMATION | MB_TOPMOST | MB_SETFOREGROUND,
                );
            }
            return;
        }
        Err(e) => {
            if !auto {
                show_error(&format!("Could not check for updates.\n\n{}", e));
            }
            return;
        }
    };

    if !ask_install(&release) {
        return;
    }

    match install(&release, false, &mut |_, _| {}) {
        Ok(()) => {
            request_quit();
            // Give the script a moment to start before this process disappears.
            std::thread::sleep(std::time::Duration::from_millis(700));
            std::process::exit(0);
        }
        Err(e) => show_error(&format!("The update could not be installed.\n\n{}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering() {
        assert!(is_newer("1.0.10", "1.0.9"));
        assert!(is_newer("v1.1.0", "1.0.9"));
        assert!(!is_newer("1.0.9", "1.0.9"));
        assert!(!is_newer("v1.0.7", "1.0.9"));
        assert!(is_newer("2.0", "1.9.9"));
    }

    #[test]
    fn swap_script_contents() {
        let script = swap_script(
            "joymapper_rust.exe",
            Path::new("C:\\Apps\\JoyMapper\\joymapper_rust.exe.new"),
            Path::new("C:\\Apps\\JoyMapper\\joymapper_rust.exe"),
            true,
        );
        assert!(script.contains("IMAGENAME eq joymapper_rust.exe"));
        assert!(script.contains("move /Y"));
        assert!(script.contains("start \"\" \"C:\\Apps\\JoyMapper\\joymapper_rust.exe\" --ui"));
        assert!(script.contains("del \"%~f0\""));
    }
}
