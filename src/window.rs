use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::Shell::ExtractIconExW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::cockpit;
use crate::diagnose;
use crate::flyout;
use crate::localization::{self, LanguageId, Strings};
use crate::models::{AppUsageData, UsageData, UsageSection};
use crate::pace;
use crate::native_interop::{
    self, TIMER_ANIM, TIMER_COUNTDOWN, TIMER_POLL, TIMER_RESET_POLL, TIMER_UPDATE_CHECK,
    WM_APP_TRAY, WM_APP_USAGE_UPDATED, WM_MOUSELEAVE,
};
use crate::poller;
use crate::providers::{self, ProviderId};
use std::ffi::c_void;
use crate::theme;
use crate::tray_icon;
use crate::updater::{self, InstallChannel, ReleaseDescriptor, UpdateCheckResult};

/// Wrapper to make HWND sendable across threads (safe for PostMessage usage)
#[derive(Clone, Copy)]
struct SendHwnd(isize);

unsafe impl Send for SendHwnd {}

impl SendHwnd {
    fn from_hwnd(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }
    fn to_hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

/// Shared application state
struct AppState {
    hwnd: SendHwnd,
    taskbar_hwnd: Option<HWND>,
    tray_notify_hwnd: Option<HWND>,
    win_event_hook: Option<HWINEVENTHOOK>,
    is_dark: bool,
    embedded: bool,
    language_override: Option<LanguageId>,
    language: LanguageId,
    install_channel: InstallChannel,

    session_text: String,
    weekly_text: String,
    codex_session_text: String,
    codex_weekly_text: String,
    antigravity_session_text: String,
    antigravity_weekly_text: String,
    scoped_text: String,
    scoped_label: String,
    show_claude_code: bool,
    show_codex: bool,
    show_antigravity: bool,
    show_scoped_weekly: bool,
    pace: pace::Settings,

    data: Option<AppUsageData>,
    /// When `data` last arrived, for the flyout's "updated" line.
    last_poll_at: Option<SystemTime>,
    /// Tapes emptying after their window rolled over.
    drains: Vec<Drain>,

    poll_interval_ms: u32,
    retry_count: u32,
    force_notify_auth_error: bool,
    auth_error_paused_polling: bool,
    auth_watch_mode: poller::CredentialWatchMode,
    auth_watch_snapshot: poller::CredentialWatchSnapshot,
    last_poll_ok: bool,
    update_status: UpdateStatus,
    last_update_check_unix: Option<u64>,

    taskbar_index: usize,
    taskbar_device: Option<String>,
    pin_to_primary_taskbar: bool,
    auto_install_updates: bool,
    update_check_interval_hours: u64,
    tray_offset: i32,
    /// Where the left button went down, in screen coordinates, and the client
    /// x under it. A press becomes a drag once it travels past the system
    /// threshold; otherwise its release is a click that opens the flyout.
    press: Option<(i32, i32, i32)>,
    /// The pointer is over the plate, which lights it like a taskbar button.
    hover: bool,
    dragging: bool,
    drag_start_mouse_x: i32,
    drag_start_client_x: i32,
    drag_start_offset: i32,

    widget_visible: bool,
}

/// A tape emptying after its window rolled over: the panel's only motion.
#[derive(Clone, Copy)]
struct Drain {
    provider: ProviderId,
    lane: usize,
    from: f64,
    started: Instant,
}

const DRAIN_DURATION: Duration = Duration::from_millis(250);

impl AppState {
    fn is_shown(&self, provider: ProviderId) -> bool {
        match provider {
            ProviderId::ClaudeCode => self.show_claude_code,
            ProviderId::Codex => self.show_codex,
            ProviderId::Antigravity => self.show_antigravity,
        }
    }

    fn set_shown(&mut self, provider: ProviderId, value: bool) {
        match provider {
            ProviderId::ClaudeCode => self.show_claude_code = value,
            ProviderId::Codex => self.show_codex = value,
            ProviderId::Antigravity => self.show_antigravity = value,
        }
    }

    /// The providers currently on screen, in display order. The render path,
    /// the tray icons and the poll loop all walk this rather than naming
    /// providers one at a time.
    fn active_providers(&self) -> Vec<ProviderId> {
        providers::PROVIDERS
            .into_iter()
            .filter(|provider| self.is_shown(*provider))
            .collect()
    }

    fn session_text_for(&self, provider: ProviderId) -> &str {
        match provider {
            ProviderId::ClaudeCode => &self.session_text,
            ProviderId::Codex => &self.codex_session_text,
            ProviderId::Antigravity => &self.antigravity_session_text,
        }
    }

    fn weekly_text_for(&self, provider: ProviderId) -> &str {
        match provider {
            ProviderId::ClaudeCode => &self.weekly_text,
            ProviderId::Codex => &self.codex_weekly_text,
            ProviderId::Antigravity => &self.antigravity_weekly_text,
        }
    }

    /// Shows the "waiting for the first answer" placeholder on every row.
    fn mark_texts_pending(&mut self) {
        self.session_text = "...".to_string();
        self.weekly_text = "...".to_string();
        self.scoped_text = "...".to_string();
        self.codex_session_text = "...".to_string();
        self.codex_weekly_text = "...".to_string();
        self.antigravity_session_text = "...".to_string();
        self.antigravity_weekly_text = "...".to_string();
    }
}

#[derive(Clone, Debug)]
enum UpdateStatus {
    Idle,
    Checking,
    Applying,
    UpToDate,
    Available(ReleaseDescriptor),
}

const RETRY_BASE_MS: u32 = 30_000; // 30 seconds

const POLL_1_MIN: u32 = 60_000;
const POLL_5_MIN: u32 = 300_000;
const POLL_15_MIN: u32 = 900_000;
const POLL_1_HOUR: u32 = 3_600_000;

// Menu item IDs for update frequency
const IDM_FREQ_1MIN: u16 = 10;
const IDM_FREQ_5MIN: u16 = 11;
const IDM_FREQ_15MIN: u16 = 12;
const IDM_FREQ_1HOUR: u16 = 13;
const IDM_START_WITH_WINDOWS: u16 = 20;
const IDM_RESET_POSITION: u16 = 30;
const IDM_VERSION_ACTION: u16 = 31;
const IDM_LANG_SYSTEM: u16 = 40;
const IDM_LANG_ENGLISH: u16 = 41;
const IDM_LANG_DUTCH: u16 = 42;
const IDM_LANG_SPANISH: u16 = 43;
const IDM_LANG_FRENCH: u16 = 44;
const IDM_LANG_GERMAN: u16 = 45;
const IDM_LANG_JAPANESE: u16 = 46;
const IDM_LANG_KOREAN: u16 = 47;
const IDM_LANG_TRADITIONAL_CHINESE: u16 = 48;
const IDM_LANG_RUSSIAN: u16 = 49;
const IDM_LANG_PORTUGUESE_BRAZIL: u16 = 50;
const IDM_LANG_SIMPLIFIED_CHINESE: u16 = 51;
const IDM_PACE_COLORS: u16 = 32;
const IDM_SCOPED_WEEKLY_ROW: u16 = 33;
const IDM_AUTO_INSTALL_UPDATES: u16 = 34;
const IDM_DETAILED_TIME: u16 = 35;
// The Models menu ids are no longer defined here: each provider carries its
// own id in `src/providers.rs`.

const WM_DPICHANGED_MSG: u32 = 0x02E0;
const WM_APP_UPDATE_CHECK_COMPLETE: u32 = WM_APP + 2;
/// Asks the UI thread to re-run the taskbar choice. Re-parenting a window from
/// the watchdog thread is not safe, so the request is posted instead.
const WM_APP_REATTACH: u32 = WM_APP + 4;
const TRAY_ICON_UPDATE_REPOSITION_SUPPRESS_MS: u64 = 750;

/// How often the watchdog thread polls for an explorer.exe restart (which
/// recreates the taskbar and wipes our tray-icon registration).
const TASKBAR_WATCH_INTERVAL_SECS: u64 = 2;
/// Consecutive misses before the taskbar is treated as replaced rather than
/// mid-rebuild. Locking the session was measured hiding it for about six
/// seconds, so this leaves room above that.
const TASKBAR_MISSES_BEFORE_REATTACH: u32 = 6;
/// Consecutive checks on the wrong screen before moving back. Locking the
/// session, or attaching over RDP, reshuffles which monitor is primary for a
/// few seconds, and reacting inside that window would land us anywhere. The
/// churn was measured at about six seconds, so this sits clear of it - the same
/// margin `TASKBAR_MISSES_BEFORE_REATTACH` gets, which at three checks this
/// constant did not have.
const WRONG_SCREEN_CHECKS_BEFORE_MOVE: u32 = 6;
/// How long startup waits for the pinned screen's taskbar to show up before
/// settling for whatever exists. A relaunch triggered during a session lock lands
/// squarely inside the rebuild, and attaching immediately is what put the widget
/// on the wrong screen in plain sight until the watchdog corrected it.
const PRIMARY_WAIT_CHECKS: u32 = 16;
const PRIMARY_WAIT_INTERVAL_MS: u64 = 500;

static SUPPRESS_TRAY_REPOSITION_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

/// Current system DPI (96 = 100% scaling, 144 = 150%, 192 = 200%, etc.)
static CURRENT_DPI: AtomicU32 = AtomicU32::new(96);

/// Show the countdown as "3h59m" instead of the rounded "4h". Ambient like the DPI
/// above, and for the same reason: the text and the column it has to fit in are
/// measured in three different places, none of which wants a flag threaded through it.
static DETAILED_TIME: AtomicBool = AtomicBool::new(false);

fn detailed_time() -> bool {
    DETAILED_TIME.load(Ordering::Relaxed)
}

/// Scale a base pixel value (designed at 96 DPI) to the current DPI.
fn sc(px: i32) -> i32 {
    let dpi = CURRENT_DPI.load(Ordering::Relaxed);
    (px as f64 * dpi as f64 / 96.0).round() as i32
}

/// The same factor as `sc`, for the drawing code, which works in fractions.
pub(crate) fn scale() -> f32 {
    CURRENT_DPI.load(Ordering::Relaxed) as f32 / 96.0
}

/// The plate's width follows its content, so it is measured when the widget
/// is drawn and kept here for the positioning code, which runs in places that
/// already hold the state lock.
static WIDGET_W: AtomicI32 = AtomicI32::new(0);
/// The window's height: the design height, or the taskbar's when that is
/// shorter (Windows 10 at 100%), so the plate is never cut off at the bottom.
static WIDGET_H: AtomicI32 = AtomicI32::new(0);

fn total_widget_width() -> i32 {
    match WIDGET_W.load(Ordering::Relaxed) {
        0 => sc(150),
        width => width,
    }
}

fn widget_height() -> i32 {
    match WIDGET_H.load(Ordering::Relaxed) {
        0 => sc(WIDGET_HEIGHT),
        height => height,
    }
}

/// Re-query the monitor DPI for our window and update the cached value.
/// Uses GetDpiForWindow which returns the live DPI (unlike GetDpiForSystem
/// which is cached at process startup and never changes).
fn refresh_dpi() {
    let hwnd = {
        let state = lock_state();
        state.as_ref().map(|s| s.hwnd.to_hwnd())
    };
    if let Some(hwnd) = hwnd {
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        if dpi > 0 {
            CURRENT_DPI.store(dpi, Ordering::Relaxed);
        }
    }
}

/// Spacing below which two relaunches are treated as a storm (e.g. explorer.exe
/// crash-looping); when detected we back off instead of spawning in a tight loop.
const RELAUNCH_THROTTLE_SECS: u64 = 10;
const RELAUNCH_BACKOFF_SECS: u64 = 30;
/// Environment flag set on a relaunched child so it waits for the previous
/// instance's single-instance mutex instead of exiting immediately.
const ENV_RELAUNCH: &str = "CCUM_RELAUNCH";
/// Unix timestamp (seconds) of the relaunch that spawned this process, passed to
/// the child so it can detect a relaunch storm.
const ENV_LAST_RELAUNCH_UNIX: &str = "CCUM_LAST_RELAUNCH_UNIX";

/// Relaunch the widget as a fresh process after explorer.exe has restarted.
///
/// When the shell restarts it destroys our embedded child window outright (the
/// window is gone, not merely orphaned - `IsWindow` returns false) and leaves
/// the UI thread parked in `GetMessage` with no window to recreate in place.
/// Spawning a clean new process - which re-embeds into the freshly created
/// taskbar - and exiting this one is the robust recovery. The child is flagged
/// via `ENV_RELAUNCH` so it waits for this instance's single-instance mutex to
/// be released before taking over (see the guard in `run`).
fn relaunch_self() {
    // Back off if we are relaunching very soon after the relaunch that spawned
    // us: that signals the shell is crash-looping, not a one-off restart.
    let now = now_unix_secs();
    let last = std::env::var(ENV_LAST_RELAUNCH_UNIX)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if last != 0 && now.saturating_sub(last) < RELAUNCH_THROTTLE_SECS {
        diagnose::log("relaunch storm detected; backing off before relaunching");
        std::thread::sleep(Duration::from_secs(RELAUNCH_BACKOFF_SECS));
    }

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            diagnose::log_error("watchdog: unable to resolve current executable", error);
            return;
        }
    };

    let args: Vec<String> = std::env::args().skip(1).collect();
    match std::process::Command::new(exe)
        .args(&args)
        .env(ENV_RELAUNCH, "1")
        .env(ENV_LAST_RELAUNCH_UNIX, now.to_string())
        .spawn()
    {
        Ok(_) => {
            diagnose::log("watchdog: relaunched fresh instance, exiting old one");
            std::process::exit(0);
        }
        Err(error) => {
            diagnose::log_error("watchdog: unable to spawn relaunched instance", error);
        }
    }
}

/// Keep the widget attached to the right taskbar, on its own thread so it
/// survives a dead UI message loop.
///
/// Two very different failures are handled here. If explorer destroys our
/// embedded child window, the message loop has nothing left to recreate and only
/// a fresh process can recover - but only once a taskbar exists again to attach
/// to. If the window is alive and merely its taskbar was rebuilt, or the pinned
/// screen changed underneath it, re-attaching in place is enough and avoids the
/// visible restart.
fn spawn_taskbar_watchdog() {
    std::thread::spawn(move || {
        let mut consecutive_misses = 0_u32;
        let mut wrong_screen_checks = 0_u32;
        loop {
            std::thread::sleep(Duration::from_secs(TASKBAR_WATCH_INTERVAL_SECS));
            let stored = {
                let state = lock_state();
                state.as_ref().and_then(|s| s.taskbar_hwnd)
            };
            // Only relevant once we have embedded into a taskbar at least once.
            let Some(old) = stored else {
                continue;
            };
            // Explorer taking the taskbar down destroys our child window with
            // it, and that is the only signal that actually requires a new
            // process: the message loop cannot recover in-process. A taskbar
            // handle merely absent from the enumeration is a rebuild in
            // progress, which is recoverable by re-attaching.
            let our_hwnd = {
                let state = lock_state();
                match state.as_ref() {
                    Some(s) => s.hwnd.to_hwnd(),
                    None => continue,
                }
            };
            let taskbars = native_interop::find_taskbars();
            let our_window_gone = unsafe { !IsWindow(our_hwnd).as_bool() };

            if taskbars.is_empty() {
                // No taskbar anywhere: the shell is down, or the session is
                // ending. Relaunching now would attach to nothing and leave the
                // widget stranded as a popup at the origin, with this watchdog
                // inert because taskbar_hwnd would stay None. Wait it out.
                if our_window_gone {
                    diagnose::log("watchdog: window gone but no taskbar yet, waiting for the shell");
                }
                consecutive_misses = 0;
                continue;
            }

            if our_window_gone {
                diagnose::log("watchdog: our window is gone and the shell is back -> relaunching");
                relaunch_self();
                continue;
            }

            if let Some(current) = taskbars.iter().find(|taskbar| taskbar.hwnd == old) {
                consecutive_misses = 0;

                // A session lock, or an RDP attach, tears the secondary
                // taskbars down and rebuilds them; the widget can be left on a
                // screen that is no longer the one it was pinned to. Existing
                // is not enough, it has to be the right screen.
                let pinned = {
                    let state = lock_state();
                    match state.as_ref() {
                        Some(s) => s.pin_to_primary_taskbar,
                        None => continue,
                    }
                };
                let dragging = {
                    let state = lock_state();
                    state.as_ref().map(|s| s.dragging).unwrap_or(false)
                };
                if dragging {
                    // Re-parenting under a held mouse button would yank the
                    // widget away mid-drag; `position_at_taskbar` refuses to move
                    // during a drag for the same reason.
                    wrong_screen_checks = 0;
                    continue;
                }

                if should_move_back_to_primary(current, &taskbars, pinned) {
                    wrong_screen_checks += 1;
                    if wrong_screen_checks >= WRONG_SCREEN_CHECKS_BEFORE_MOVE {
                        wrong_screen_checks = 0;
                        diagnose::log(format!(
                            "watchdog: pinned widget sits on {:?}, primary is elsewhere -> re-attaching",
                            current.device
                        ));
                        unsafe {
                            let _ = PostMessageW(our_hwnd, WM_APP_REATTACH, WPARAM(0), LPARAM(0));
                        }
                    }
                } else {
                    wrong_screen_checks = 0;
                }
                continue;
            }

            // Our window is alive, so the shell is rebuilding rather than
            // restarting: wait for the churn to settle, then re-attach in place.
            // No new process, so nothing blinks.
            consecutive_misses += 1;
            if consecutive_misses < TASKBAR_MISSES_BEFORE_REATTACH {
                diagnose::log(format!(
                    "watchdog: taskbar {:?} missing ({consecutive_misses}/{TASKBAR_MISSES_BEFORE_REATTACH}), waiting",
                    old.0
                ));
                continue;
            }

            consecutive_misses = 0;
            diagnose::log(format!(
                "watchdog: taskbar {:?} replaced -> re-attaching",
                old.0
            ));
            unsafe {
                let _ = PostMessageW(our_hwnd, WM_APP_REATTACH, WPARAM(0), LPARAM(0));
            }
        }
    });
}

fn load_embedded_app_icons() -> (HICON, HICON) {
    unsafe {
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            return (HICON::default(), HICON::default());
        }

        let mut large_icon = HICON::default();
        let mut small_icon = HICON::default();
        let extracted = ExtractIconExW(
            PCWSTR::from_raw(exe_buf.as_ptr()),
            0,
            Some(&mut large_icon),
            Some(&mut small_icon),
            1,
        );

        if extracted == 0 {
            (HICON::default(), HICON::default())
        } else {
            (large_icon, small_icon)
        }
    }
}

unsafe impl Send for AppState {}

static STATE: Mutex<Option<AppState>> = Mutex::new(None);

/// Lock STATE safely, recovering from poisoned mutex
fn lock_state() -> MutexGuard<'static, Option<AppState>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn settings_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(appdata)
        .join("ClaudeCodeUsageMonitor")
        .join("settings.json")
}

#[derive(Debug, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    tray_offset: i32,
    #[serde(default)]
    taskbar_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    taskbar_device: Option<String>,
    /// `None` means the file predates the setting, which is what lets an
    /// existing screen choice survive the upgrade instead of being pinned over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pin_to_primary_taskbar: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scoped_label_last: Option<String>,
    #[serde(default = "default_poll_interval")]
    poll_interval_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_update_check_unix: Option<u64>,
    #[serde(default = "default_widget_visible")]
    widget_visible: bool,
    #[serde(default = "default_show_claude_code")]
    show_claude_code: bool,
    #[serde(default = "default_show_codex")]
    show_codex: bool,
    #[serde(default = "default_show_antigravity")]
    show_antigravity: bool,
    #[serde(default = "default_true")]
    show_scoped_weekly: bool,
    /// Set when the file on disk failed to parse. Never serialized: it only
    /// stops this run from writing defaults over content we could not read.
    #[serde(skip)]
    unreadable: bool,
    #[serde(default = "default_true")]
    pace_colors: bool,
    /// Off by default: the compact rounded countdown is the narrower widget.
    #[serde(default)]
    detailed_time: bool,
    #[serde(default = "default_true")]
    auto_install_updates: bool,
    #[serde(default = "default_update_check_interval_hours")]
    update_check_interval_hours: u64,
    #[serde(default = "default_pace_on_track")]
    pace_on_track: f64,
    #[serde(default = "default_pace_at_risk")]
    pace_at_risk: f64,
    #[serde(default = "default_pace_min_elapsed_fraction")]
    pace_min_elapsed_fraction: f64,
    #[serde(default = "default_pace_color_on_track")]
    pace_color_on_track: String,
    #[serde(default = "default_pace_color_at_risk")]
    pace_color_at_risk: String,
    #[serde(default = "default_pace_color_over")]
    pace_color_over: String,
}

impl SettingsFile {
    fn is_shown(&self, provider: ProviderId) -> bool {
        match provider {
            ProviderId::ClaudeCode => self.show_claude_code,
            ProviderId::Codex => self.show_codex,
            ProviderId::Antigravity => self.show_antigravity,
        }
    }

    fn set_shown(&mut self, provider: ProviderId, value: bool) {
        match provider {
            ProviderId::ClaudeCode => self.show_claude_code = value,
            ProviderId::Codex => self.show_codex = value,
            ProviderId::Antigravity => self.show_antigravity = value,
        }
    }
}

/// Whatever the file says, the widget has to show something: a hand-edited
/// file with every provider off falls back to the default one instead of
/// rendering an empty strip.
fn keep_one_provider_visible(settings: &mut SettingsFile) {
    if providers::PROVIDERS
        .into_iter()
        .all(|provider| !settings.is_shown(provider))
    {
        settings.set_shown(ProviderId::ClaudeCode, true);
    }
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            tray_offset: 0,
            taskbar_index: 0,
            taskbar_device: None,
            pin_to_primary_taskbar: Some(true),
            scoped_label_last: None,
            poll_interval_ms: default_poll_interval(),
            language: None,
            last_update_check_unix: None,
            widget_visible: true,
            show_claude_code: true,
            show_codex: false,
            show_antigravity: false,
            show_scoped_weekly: true,
            unreadable: false,
            pace_colors: true,
            detailed_time: false,
            auto_install_updates: true,
            update_check_interval_hours: default_update_check_interval_hours(),
            pace_on_track: default_pace_on_track(),
            pace_at_risk: default_pace_at_risk(),
            pace_min_elapsed_fraction: default_pace_min_elapsed_fraction(),
            pace_color_on_track: default_pace_color_on_track(),
            pace_color_at_risk: default_pace_color_at_risk(),
            pace_color_over: default_pace_color_over(),
        }
    }
}

fn default_poll_interval() -> u32 {
    POLL_15_MIN
}

fn default_true() -> bool {
    true
}

fn default_update_check_interval_hours() -> u64 {
    24
}

fn default_pace_on_track() -> f64 {
    pace::DEFAULT_ON_TRACK
}

fn default_pace_at_risk() -> f64 {
    pace::DEFAULT_AT_RISK
}

fn default_pace_min_elapsed_fraction() -> f64 {
    pace::DEFAULT_MIN_ELAPSED_FRACTION
}

fn default_pace_color_on_track() -> String {
    pace::DEFAULT_COLOR_ON_TRACK.to_string()
}

fn default_pace_color_at_risk() -> String {
    pace::DEFAULT_COLOR_AT_RISK.to_string()
}

fn default_pace_color_over() -> String {
    pace::DEFAULT_COLOR_OVER.to_string()
}

fn default_widget_visible() -> bool {
    true
}

fn default_show_claude_code() -> bool {
    true
}

fn default_show_codex() -> bool {
    false
}

fn default_show_antigravity() -> bool {
    false
}

fn load_settings() -> SettingsFile {
    let content = match std::fs::read_to_string(settings_path()) {
        Ok(c) => c,
        Err(_) => return SettingsFile::default(),
    };
    let mut settings: SettingsFile = match serde_json::from_str(&content) {
        Ok(settings) => settings,
        Err(error) => {
            // Everything below would otherwise be silently replaced by defaults
            // and written back over the file, losing hand-tuned values. Say so,
            // and refuse to overwrite what we could not read.
            diagnose::log_error("settings file could not be parsed; using defaults", error);
            let mut defaults = SettingsFile::default();
            defaults.unreadable = true;
            return defaults;
        }
    };
    keep_one_provider_visible(&mut settings);
    if settings.pin_to_primary_taskbar.is_none() {
        // A file written before this setting existed may already carry a screen
        // the user picked; pinning would override and then overwrite it.
        let screen_already_chosen =
            settings.taskbar_device.is_some() || settings.taskbar_index != 0;
        settings.pin_to_primary_taskbar = Some(!screen_already_chosen);
    }
    settings
}

fn save_settings(settings: &SettingsFile) {
    if settings.unreadable {
        diagnose::log("refusing to overwrite a settings file that could not be parsed");
        return;
    }

    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, json);
    }
}

fn pace_settings_from(settings: &SettingsFile) -> pace::Settings {
    pace::Settings::sanitized(
        settings.pace_colors,
        settings.pace_on_track,
        settings.pace_at_risk,
        settings.pace_min_elapsed_fraction,
        &settings.pace_color_on_track,
        &settings.pace_color_at_risk,
        &settings.pace_color_over,
    )
}

/// Written back over the file as it is on disk, so the values that are only
/// tunable by hand keep whatever the user put there.
fn save_state_settings() {
    // Read the file before taking the lock and write it after releasing it: the
    // UI thread blocks on this mutex, and %APPDATA% can be slow (sync, scanners).
    let mut settings = load_settings();
    {
        let state = lock_state();
        let Some(s) = state.as_ref() else {
            return;
        };
        settings.tray_offset = s.tray_offset;
        settings.taskbar_index = s.taskbar_index;
        settings.taskbar_device = s.taskbar_device.clone();
        settings.pin_to_primary_taskbar = Some(s.pin_to_primary_taskbar);
        settings.scoped_label_last = if s.scoped_label.is_empty() {
            None
        } else {
            Some(s.scoped_label.clone())
        };
        settings.poll_interval_ms = s.poll_interval_ms;
        settings.language = s
            .language_override
            .map(|language| language.code().to_string());
        settings.last_update_check_unix = s.last_update_check_unix;
        settings.widget_visible = s.widget_visible;
        settings.show_claude_code = s.show_claude_code;
        settings.show_codex = s.show_codex;
        settings.show_antigravity = s.show_antigravity;
        settings.show_scoped_weekly = s.show_scoped_weekly;
        settings.pace_colors = s.pace.enabled;
        settings.auto_install_updates = s.auto_install_updates;
        settings.update_check_interval_hours = s.update_check_interval_hours;
    }
    settings.detailed_time = detailed_time();
    save_settings(&settings);
}

/// One icon per provider on screen, built from the provider table: adding a
/// provider no longer means adding another hand-written block here. Each is
/// the widget's 5-hour readout at icon size, with the same hot rule.
fn tray_icon_data_from_state() -> Vec<tray_icon::TrayIconData> {
    let state = lock_state();
    match state.as_ref() {
        Some(s) if s.last_poll_ok => {
            let now = SystemTime::now();
            let palette = cockpit::Palette::new(s.is_dark, &s.pace);
            let mut heat = Vec::new();
            let mut icons: Vec<tray_icon::TrayIconData> = s
                .active_providers()
                .into_iter()
                .map(|provider| {
                    let session = shown_usage(s, provider)
                        .map(|usage| &usage.session)
                        .filter(|section| reported(section));
                    let (readout, pace) = readout_for(s, session, pace::SESSION_WINDOW, now);
                    heat.push(heat_of(&readout, pace));
                    tray_icon::TrayIconData {
                        kind: provider.into(),
                        badge: session.map(|_| cockpit::Badge {
                            text: readout.text,
                            band: readout.band,
                            hot: false,
                        }),
                        palette,
                        tooltip: tray_tooltip_for(s, provider),
                    }
                })
                .collect();
            if let Some(badge) = hottest(&heat).and_then(|index| icons[index].badge.as_mut()) {
                badge.hot = true;
            }
            icons
        }
        Some(s) => s
            .active_providers()
            .into_iter()
            .map(|provider| tray_icon::TrayIconData {
                kind: provider.into(),
                badge: None,
                palette: cockpit::Palette::new(s.is_dark, &s.pace),
                tooltip: match provider {
                    ProviderId::ClaudeCode => s.language.strings().window_title.to_string(),
                    ProviderId::Codex => s.language.strings().codex_window_title.to_string(),
                    ProviderId::Antigravity => {
                        s.language.strings().antigravity_window_title.to_string()
                    }
                },
            })
            .collect(),
        None => Vec::new(),
    }
}

impl From<ProviderId> for tray_icon::TrayIconKind {
    fn from(provider: ProviderId) -> Self {
        match provider {
            ProviderId::ClaudeCode => Self::Claude,
            ProviderId::Codex => Self::Codex,
            ProviderId::Antigravity => Self::Antigravity,
        }
    }
}

fn tray_tooltip_for(state: &AppState, provider: ProviderId) -> String {
    let name = provider.label(state.language.strings());
    let session = state.session_text_for(provider);
    let weekly = state.weekly_text_for(provider);

    if provider == ProviderId::ClaudeCode && scoped_row_visible(state) {
        format!(
            "{name} 5h: {session} | 7d: {weekly} | {}: {}",
            state.scoped_label, state.scoped_text
        )
    } else {
        format!("{name} 5h: {session} | 7d: {weekly}")
    }
}

fn sync_tray_icons(hwnd: HWND) {
    let icons = tray_icon_data_from_state();
    tray_icon::sync(hwnd, &icons);
}

fn toggle_widget_visibility(hwnd: HWND) {
    let new_visible = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            s.widget_visible = !s.widget_visible;
            s.widget_visible
        } else {
            return;
        }
    };
    save_state_settings();
    unsafe {
        if new_visible {
            position_at_taskbar();
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            render_layered();
        } else {
            flyout::hide();
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

/// Whether startup should hold off: we want the primary monitor's taskbar and the
/// shell has not produced it yet.
fn should_wait_for_primary(taskbars: &[native_interop::TaskbarWindow], pinned: bool) -> bool {
    pinned && !taskbars.iter().any(|taskbar| taskbar.is_primary)
}

/// Give the shell a bounded chance to finish rebuilding before we choose a screen.
fn wait_for_primary_taskbar(pinned: bool) {
    if !pinned {
        return;
    }

    for attempt in 0..PRIMARY_WAIT_CHECKS {
        if !should_wait_for_primary(&native_interop::find_taskbars(), pinned) {
            if attempt > 0 {
                diagnose::log(format!(
                    "primary taskbar appeared after {} ms",
                    attempt as u64 * PRIMARY_WAIT_INTERVAL_MS
                ));
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(PRIMARY_WAIT_INTERVAL_MS));
    }

    diagnose::log(
        "no primary taskbar appeared in time; attaching to whatever the shell offers",
    );
}

/// Whether a pinned widget has ended up on the wrong screen and a correct one
/// is available. While a session is locked or an RDP attach is in flight,
/// Windows can report no primary monitor at all: moving then would land the
/// widget anywhere, so an absent primary means stay put.
fn should_move_back_to_primary(
    current: &native_interop::TaskbarWindow,
    taskbars: &[native_interop::TaskbarWindow],
    pinned: bool,
) -> bool {
    if !pinned || current.is_primary {
        return false;
    }
    taskbars.iter().any(|taskbar| taskbar.is_primary)
}

/// Returns the taskbar to use and whether the remembered monitor was found.
/// A false flag means the pick is a fallback and must not overwrite the
/// remembered preference.
fn select_taskbar(
    taskbars: &[native_interop::TaskbarWindow],
    requested_index: usize,
    requested_device: Option<&str>,
    pin_to_primary: bool,
) -> (usize, bool) {
    // Pinning wins over everything: the primary monitor is an identity Windows
    // itself maintains, so it cannot drift the way an index or a handle does.
    if pin_to_primary {
        if let Some(index) = taskbars.iter().position(|taskbar| taskbar.is_primary) {
            return (index, true);
        }
    }

    if let Some(device) = requested_device {
        if let Some(index) = taskbars
            .iter()
            .position(|taskbar| taskbar.device.as_deref() == Some(device))
        {
            return (index, true);
        }
    }

    (
        requested_index.min(taskbars.len().saturating_sub(1)),
        false,
    )
}

/// Pick the taskbar to embed into. The remembered monitor wins over the
/// remembered index: an index is a position in a list sorted by geometry, so it
/// silently points at another screen whenever the enumeration is incomplete or
/// the display layout changes.
fn attach_to_taskbar(
    hwnd: HWND,
    requested_index: usize,
    requested_device: Option<&str>,
    pin_to_primary: bool,
) -> bool {
    let taskbars = native_interop::find_taskbars();
    if taskbars.is_empty() {
        diagnose::log("taskbar not found; using fallback popup window");
        return false;
    }

    let (index, matched_device) =
        select_taskbar(&taskbars, requested_index, requested_device, pin_to_primary);
    if !matched_device {
        if let Some(device) = requested_device {
            diagnose::log(format!(
                "remembered monitor {device} is absent from {} taskbar(s); falling back to index",
                taskbars.len()
            ));
        }
    }
    let taskbar = taskbars[index].clone();
    diagnose::log(format!(
        "taskbar selected index={index} count={} device={:?} primary={} pinned={pin_to_primary} matched_by={} hwnd={:?} rect=({}, {}, {}, {})",
        taskbars.len(),
        taskbar.device,
        taskbar.is_primary,
        if matched_device { "monitor" } else { "index" },
        taskbar.hwnd,
        taskbar.rect.left,
        taskbar.rect.top,
        taskbar.rect.right,
        taskbar.rect.bottom
    ));

    let old_hook = {
        let mut state = lock_state();
        state.as_mut().and_then(|s| s.win_event_hook.take())
    };
    if let Some(hook) = old_hook {
        native_interop::unhook_win_event(hook);
    }

    native_interop::embed_in_taskbar(hwnd, taskbar.hwnd);

    let tray_notify = native_interop::find_child_window(taskbar.hwnd, "TrayNotifyWnd");
    if tray_notify.is_some() {
        diagnose::log("TrayNotifyWnd found");
    } else {
        diagnose::log("TrayNotifyWnd not found");
    }

    let hook = tray_notify.and_then(|tray_hwnd| {
        let thread_id = native_interop::get_window_thread_id(tray_hwnd);
        native_interop::set_tray_event_hook(thread_id, on_tray_location_changed)
    });
    if hook.is_some() {
        diagnose::log("tray event hook installed");
    } else {
        diagnose::log("tray event hook could not be installed");
    }

    let mut state = lock_state();
    if let Some(s) = state.as_mut() {
        s.taskbar_hwnd = Some(taskbar.hwnd);
        s.tray_notify_hwnd = tray_notify;
        s.win_event_hook = hook;
        s.embedded = true;
        // Only record the choice when it is one we can stand behind. A fallback
        // pick would otherwise overwrite the preference with the wrong screen.
        if matched_device || requested_device.is_none() {
            s.taskbar_index = index;
            s.taskbar_device = taskbar.device.clone();
        }
    }
    true
}

fn taskbar_at_point(pt: POINT) -> Option<(usize, native_interop::TaskbarWindow)> {
    native_interop::find_taskbars()
        .into_iter()
        .enumerate()
        .find(|(_, taskbar)| {
            pt.x >= taskbar.rect.left
                && pt.x < taskbar.rect.right
                && pt.y >= taskbar.rect.top
                && pt.y < taskbar.rect.bottom
        })
}

fn tray_left_for_taskbar(taskbar_hwnd: HWND, taskbar_rect: RECT) -> i32 {
    let mut tray_left = taskbar_rect.right;
    if let Some(tray_hwnd) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(tray_rect) = native_interop::get_window_rect_safe(tray_hwnd) {
            tray_left = tray_rect.left;
        }
    }
    tray_left
}

fn clamp_offset_for_taskbar(taskbar_hwnd: HWND, taskbar_rect: RECT, offset: i32) -> i32 {
    let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
    let max_offset = (tray_left - taskbar_rect.left - total_widget_width()).max(0);
    offset.clamp(0, max_offset)
}

fn offset_for_drop_point(
    taskbar_hwnd: HWND,
    taskbar_rect: RECT,
    pt: POINT,
    drag_start_client_x: i32,
) -> i32 {
    let tray_left = tray_left_for_taskbar(taskbar_hwnd, taskbar_rect);
    let desired_left = pt.x - taskbar_rect.left - drag_start_client_x;
    let offset = tray_left - taskbar_rect.left - total_widget_width() - desired_left;
    clamp_offset_for_taskbar(taskbar_hwnd, taskbar_rect, offset)
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Clamped so a hand-edited file cannot turn the check into a hot loop against
/// the GitHub API, nor push it so far out that it never runs.
fn update_check_interval(hours: u64) -> Duration {
    Duration::from_secs(hours.clamp(1, 24 * 30) * 60 * 60)
}

fn auto_update_check_due(last_update_check_unix: Option<u64>, hours: u64) -> bool {
    let Some(last_update_check_unix) = last_update_check_unix else {
        return true;
    };

    now_unix_secs().saturating_sub(last_update_check_unix) >= update_check_interval(hours).as_secs()
}

fn schedule_auto_update_check(hwnd: HWND) {
    let delay_ms = {
        let state = lock_state();
        let Some(s) = state.as_ref() else {
            return;
        };

        if auto_update_check_due(s.last_update_check_unix, s.update_check_interval_hours) {
            None
        } else {
            let elapsed = now_unix_secs().saturating_sub(s.last_update_check_unix.unwrap_or(0));
            let remaining_secs = update_check_interval(s.update_check_interval_hours)
                .as_secs()
                .saturating_sub(elapsed);
            Some((remaining_secs.saturating_mul(1000)).min(u32::MAX as u64) as u32)
        }
    };

    unsafe {
        let _ = KillTimer(hwnd, TIMER_UPDATE_CHECK);
        if let Some(delay_ms) = delay_ms {
            SetTimer(hwnd, TIMER_UPDATE_CHECK, delay_ms.max(1), None);
        }
    }
}

/// Formats one row, telling "nothing was reported" apart from "zero used".
/// The heuristic is the one Antigravity already relied on: a section with
/// neither a reset time nor any consumption carries no information, so it
/// reads `--` instead of an invented 0%.
fn format_section(section: &UsageSection, strings: Strings, detailed: bool) -> String {
    if section.resets_at.is_none() && section.percentage == 0.0 {
        "--".to_string()
    } else {
        poller::format_line(section, strings, detailed)
    }
}

fn refresh_usage_texts(state: &mut AppState) {
    if !state.last_poll_ok {
        return;
    }

    let strings = state.language.strings();
    let detailed = detailed_time();
    let Some(data) = state.data.as_ref() else {
        return;
    };

    if let Some(claude_code) = data.get(ProviderId::ClaudeCode) {
        state.session_text = format_section(&claude_code.session, strings, detailed);
        state.weekly_text = format_section(&claude_code.weekly, strings, detailed);
        match claude_code.scoped.as_ref() {
            Some(scoped) => {
                state.scoped_text = poller::format_line(&scoped.section, strings, detailed);
                state.scoped_label = scoped.label.clone();
            }
            None => {
                // Keep the label, and therefore the row: dropping it would
                // reflow the whole widget over one incomplete poll.
                state.scoped_text = "--".to_string();
            }
        }
    } else if state.show_claude_code {
        state.session_text = "!".to_string();
        state.weekly_text = "!".to_string();
        state.scoped_text = "!".to_string();
    }

    if let Some(codex) = data.get(ProviderId::Codex) {
        state.codex_session_text = format_section(&codex.session, strings, detailed);
        state.codex_weekly_text = format_section(&codex.weekly, strings, detailed);
    } else if state.show_codex {
        state.codex_session_text = "!".to_string();
        state.codex_weekly_text = "!".to_string();
    }

    if let Some(antigravity) = data.get(ProviderId::Antigravity) {
        state.antigravity_session_text =
            format_section(&antigravity.session, strings, detailed);
        state.antigravity_weekly_text =
            format_section(&antigravity.weekly, strings, detailed);
    } else if state.show_antigravity {
        state.antigravity_session_text = "!".to_string();
        state.antigravity_weekly_text = "!".to_string();
    }
}

// --- what the panel shows ---------------------------------------------------
//
// Every surface (widget, flyout, tray) reads a window through the same three
// helpers below, so a value cannot be red in one place and neutral in another.

const LANE_SESSION: usize = 0;
const LANE_WEEKLY: usize = 1;
const LANE_SCOPED: usize = 2;

fn lane_section(usage: &UsageData, lane: usize) -> Option<&UsageSection> {
    match lane {
        LANE_SESSION => Some(&usage.session),
        LANE_WEEKLY => Some(&usage.weekly),
        _ => usage.scoped.as_ref().map(|scoped| &scoped.section),
    }
}

fn lane_window(lane: usize) -> Duration {
    if lane == LANE_SESSION {
        pace::SESSION_WINDOW
    } else {
        pace::WEEKLY_WINDOW
    }
}

fn lane_label(state: &AppState, lane: usize) -> String {
    let strings = state.language.strings();
    match lane {
        LANE_SESSION => strings.session_window.to_string(),
        LANE_WEEKLY => strings.weekly_window.to_string(),
        _ => state.scoped_label.clone(),
    }
}

/// The windows a provider shows: the per-model one only for Claude, and only
/// once the API has named it.
fn lanes_for(state: &AppState, provider: ProviderId) -> Vec<usize> {
    if provider == ProviderId::ClaudeCode && scoped_row_visible(state) {
        vec![LANE_SESSION, LANE_WEEKLY, LANE_SCOPED]
    } else {
        vec![LANE_SESSION, LANE_WEEKLY]
    }
}

/// A provider that could not be read: the credentials were refused, or the
/// last answer did not include it. Said with a mark, never with red.
fn provider_failed(state: &AppState, provider: ProviderId) -> bool {
    state.auth_error_paused_polling
        || state
            .data
            .as_ref()
            .is_some_and(|data| data.get(provider).is_none())
}

fn shown_usage(state: &AppState, provider: ProviderId) -> Option<&UsageData> {
    if provider_failed(state, provider) {
        return None;
    }
    state.data.as_ref().and_then(|data| data.get(provider))
}

fn gauge_for(
    state: &AppState,
    provider: ProviderId,
    lane: usize,
    section: Option<&UsageSection>,
    now: SystemTime,
) -> cockpit::Gauge {
    let Some(section) = section.filter(|section| reported(section)) else {
        return cockpit::Gauge::default();
    };
    let window = lane_window(lane);
    let mut fraction = section.percentage.clamp(0.0, 100.0) / 100.0;
    if let Some(drain) = state
        .drains
        .iter()
        .find(|drain| drain.provider == provider && drain.lane == lane)
    {
        let t = drain.started.elapsed().as_secs_f64() / DRAIN_DURATION.as_secs_f64();
        if t < 1.0 {
            let eased = 1.0 - 2f64.powf(-10.0 * t);
            fraction = drain.from + (fraction - drain.from) * eased;
        }
    }
    cockpit::Gauge {
        fraction: Some(fraction),
        band: pace::band(pace::pace(section, window, now, &state.pace), &state.pace),
        bug: pace::elapsed_fraction(section, window, now, &state.pace),
    }
}

/// The boxed value and the pace behind it, which decides the hot one.
fn readout_for(
    state: &AppState,
    section: Option<&UsageSection>,
    window: Duration,
    now: SystemTime,
) -> (cockpit::Readout, Option<f64>) {
    match section.filter(|section| reported(section)) {
        Some(section) => {
            let pace = pace::pace(section, window, now, &state.pace);
            (
                cockpit::Readout {
                    text: format!("{:.0}", section.percentage.clamp(0.0, 999.0)),
                    band: pace::band(pace, &state.pace),
                    hot: false,
                },
                pace,
            )
        }
        None => (
            cockpit::Readout {
                text: "--".to_string(),
                ..Default::default()
            },
            None,
        ),
    }
}

/// Only a value in the red band competes for the reversed box.
fn heat_of(readout: &cockpit::Readout, pace: Option<f64>) -> Option<f64> {
    (readout.band == Some(pace::Band::Over)).then_some(pace).flatten()
}

/// The one value on a surface allowed the loud treatment, if any is red.
fn hottest(heat: &[Option<f64>]) -> Option<usize> {
    heat.iter()
        .enumerate()
        .filter_map(|(index, heat)| heat.map(|heat| (index, heat)))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// The widest countdowns this language and format can print.
fn countdown_samples(strings: Strings, detailed: bool, now: SystemTime) -> Vec<String> {
    let seconds: [u64; 4] = if detailed {
        [59, 59 * 60 + 59, 23 * 3600 + 59 * 60, 6 * 86_400 + 23 * 3600]
    } else {
        [59, 59 * 60, 23 * 3600, 6 * 86_400]
    };
    seconds
        .iter()
        .map(|&secs| {
            // A second of margin, or the formatter's own clock read lands a
            // hair short and rounds down.
            let at = now + Duration::from_secs(secs + 1);
            poller::format_countdown(Some(at), strings, detailed)
        })
        .chain(std::iter::once(strings.now.to_string()))
        .collect()
}

/// The taskbar plate. Several providers get a row each (5-hour tape, 7-day
/// hairline); a single one gets a row per window instead.
fn widget_model(state: &AppState) -> cockpit::WidgetModel {
    let strings = state.language.strings();
    let detailed = detailed_time();
    let now = SystemTime::now();
    let active = state.active_providers();

    let row = |provider: ProviderId, lane: usize, code: String, sub_lane: Option<usize>| {
        let usage = shown_usage(state, provider);
        let section = usage
            .and_then(|usage| lane_section(usage, lane))
            .filter(|section| reported(section));
        let (readout, pace) = readout_for(state, section, lane_window(lane), now);
        let heat = heat_of(&readout, pace);
        let row = cockpit::WidgetRow {
            code,
            failed: provider_failed(state, provider),
            gauge: gauge_for(state, provider, lane, section, now),
            sub: sub_lane.map(|sub_lane| {
                let sub = usage.and_then(|usage| lane_section(usage, sub_lane));
                gauge_for(state, provider, sub_lane, sub, now)
            }),
            readout,
            countdown: section
                .map(|section| poller::format_countdown(section.resets_at, strings, detailed))
                .unwrap_or_default(),
        };
        (row, heat)
    };

    let built: Vec<(cockpit::WidgetRow, Option<f64>)> = match active.as_slice() {
        [provider] => lanes_for(state, *provider)
            .into_iter()
            .map(|lane| row(*provider, lane, lane_label(state, lane).to_uppercase(), None))
            .collect(),
        _ => active
            .iter()
            .map(|&provider| {
                row(provider, LANE_SESSION, provider.code().to_string(), Some(LANE_WEEKLY))
            })
            .collect(),
    };

    let heat: Vec<Option<f64>> = built.iter().map(|(_, heat)| *heat).collect();
    let mut rows: Vec<cockpit::WidgetRow> = built.into_iter().map(|(row, _)| row).collect();
    if let Some(index) = hottest(&heat) {
        rows[index].readout.hot = true;
    }

    let plate = if flyout::is_visible() || (state.press.is_some() && !state.dragging) {
        cockpit::PlateState::Active
    } else if state.hover {
        cockpit::PlateState::Hover
    } else {
        cockpit::PlateState::Rest
    };

    cockpit::WidgetModel {
        rows,
        plate,
        palette: cockpit::Palette::new(state.is_dark, &state.pace),
        countdown_samples: countdown_samples(strings, detailed, now),
    }
}

/// The panel the widget opens: every window of every provider, with its exact
/// reset, and a projection wherever the limit would land before the reset.
pub(crate) fn flyout_model() -> Option<cockpit::FlyoutModel> {
    let state = lock_state();
    let s = state.as_ref()?;
    let strings = s.language.strings();
    let now = SystemTime::now();

    let mut alerts: Vec<(Duration, cockpit::Alert)> = Vec::new();
    let mut heat: Vec<(usize, usize, Option<f64>)> = Vec::new();
    let mut groups = Vec::new();

    for provider in s.active_providers() {
        let usage = shown_usage(s, provider);
        let name = provider.label(strings).to_uppercase();
        let columns = lanes_for(s, provider)
            .into_iter()
            .enumerate()
            .map(|(column, lane)| {
                let window = lane_window(lane);
                let label = lane_label(s, lane).to_uppercase();
                let section = usage
                    .and_then(|usage| lane_section(usage, lane))
                    .filter(|section| reported(section));
                let (readout, pace) = readout_for(s, section, window, now);
                heat.push((groups.len(), column, heat_of(&readout, pace)));

                if let Some(to_limit) =
                    section.and_then(|section| pace::time_to_limit(section, window, now, &s.pace))
                {
                    let at = now + to_limit + Duration::from_secs(1);
                    let when = poller::format_countdown(Some(at), strings, true);
                    alerts.push((
                        to_limit,
                        cockpit::Alert {
                            who: format!("{name} {label}"),
                            // The words in capitals like the rest of the
                            // panel; the countdown as it reads everywhere else.
                            text: strings.limit_projection.to_uppercase().replace("{TIME}", &when),
                            band: readout.band,
                        },
                    ));
                }

                cockpit::FlyoutColumn {
                    gauge: gauge_for(s, provider, lane, section, now),
                    reset: section
                        .and_then(|section| section.resets_at)
                        .and_then(native_interop::format_local_time)
                        .unwrap_or_default(),
                    countdown: section
                        .map(|section| poller::format_countdown(section.resets_at, strings, true))
                        .unwrap_or_default(),
                    label,
                    readout,
                }
            })
            .collect();
        // The reason in words: a refused sign-in is something the user can act
        // on (the balloon they were shown says how); a missing answer is not.
        let cause = provider_failed(s, provider).then(|| {
            if s.auth_error_paused_polling {
                strings.sign_in_again.to_uppercase()
            } else {
                strings.no_data.to_uppercase()
            }
        });
        groups.push(cockpit::FlyoutGroup {
            name,
            cause,
            columns,
        });
    }

    let flat: Vec<Option<f64>> = heat.iter().map(|(_, _, heat)| *heat).collect();
    if let Some(index) = hottest(&flat) {
        let (group, column, _) = heat[index];
        groups[group].columns[column].readout.hot = true;
    }

    // Soonest first, and no more than a glance can take in.
    alerts.sort_by_key(|(to_limit, _)| *to_limit);
    alerts.truncate(3);

    Some(cockpit::FlyoutModel {
        title: strings.usage_title.to_uppercase(),
        updated: s
            .last_poll_at
            .and_then(native_interop::format_local_time)
            .map(|time| strings.updated_at.replace("{time}", &time).to_uppercase())
            .unwrap_or_default(),
        alerts: alerts.into_iter().map(|(_, alert)| alert).collect(),
        groups,
        legend: strings.bug_legend.to_string(),
        buttons: [strings.refresh.to_uppercase(), strings.settings.to_uppercase()],
        palette: cockpit::Palette::new(s.is_dark, &s.pace),
    })
}

fn set_window_title(hwnd: HWND, strings: Strings) {
    unsafe {
        let title = native_interop::wide_str(strings.window_title);
        let _ = SetWindowTextW(hwnd, PCWSTR::from_raw(title.as_ptr()));
    }
}

fn show_info_message(hwnd: HWND, title: &str, message: &str) {
    unsafe {
        let title_wide = native_interop::wide_str(title);
        let message_wide = native_interop::wide_str(message);
        let _ = MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn show_error_message(hwnd: HWND, title: &str, message: &str) {
    unsafe {
        let title_wide = native_interop::wide_str(title);
        let message_wide = native_interop::wide_str(message);
        let _ = MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn show_update_prompt(hwnd: HWND, strings: Strings, release: &ReleaseDescriptor) -> bool {
    let message = strings
        .update_prompt_now
        .replace("{version}", &release.latest_version);

    unsafe {
        let title_wide = native_interop::wide_str(strings.update_available);
        let message_wide = native_interop::wide_str(&message);
        MessageBoxW(
            hwnd,
            PCWSTR::from_raw(message_wide.as_ptr()),
            PCWSTR::from_raw(title_wide.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

fn apply_language_to_state(state: &mut AppState, language_override: Option<LanguageId>) {
    state.language_override = language_override;
    state.language = localization::resolve_language(language_override);
    set_window_title(state.hwnd.to_hwnd(), state.language.strings());
    refresh_usage_texts(state);
}

fn update_language_change() -> bool {
    let mut state = lock_state();
    let Some(app_state) = state.as_mut() else {
        return false;
    };

    if app_state.language_override.is_some() {
        return false;
    }

    let new_language = localization::detect_system_language();
    if new_language == app_state.language {
        return false;
    }

    apply_language_to_state(app_state, None);
    true
}

fn version_action_label(
    strings: Strings,
    language: LanguageId,
    install_channel: InstallChannel,
    status: &UpdateStatus,
) -> String {
    let current = env!("CARGO_PKG_VERSION");
    match status {
        UpdateStatus::Idle => format!("v{current} - {}", strings.check_for_updates),
        UpdateStatus::Checking => format!("v{current} - {}", strings.checking_for_updates),
        UpdateStatus::Applying => format!("v{current} - {}", strings.applying_update),
        UpdateStatus::UpToDate => format!("v{current} - {}", strings.up_to_date_short),
        UpdateStatus::Available(release) => match install_channel {
            InstallChannel::Portable => {
                format!(
                    "v{current} - {} v{}",
                    strings.update_to, release.latest_version
                )
            }
            InstallChannel::Winget => format!(
                "v{current} - {} v{}",
                localization::update_via_winget(language),
                release.latest_version
            ),
        },
    }
}

fn begin_update_check(hwnd: HWND, interactive: bool) {
    let send_hwnd = SendHwnd::from_hwnd(hwnd);
    let (strings, install_channel) = {
        let mut state = lock_state();
        let Some(app_state) = state.as_mut() else {
            return;
        };

        if matches!(
            app_state.update_status,
            UpdateStatus::Checking | UpdateStatus::Applying
        ) {
            if interactive {
                show_info_message(
                    hwnd,
                    app_state.language.strings().updates,
                    app_state.language.strings().update_in_progress,
                );
            }
            return;
        }

        app_state.update_status = UpdateStatus::Checking;
        (app_state.language.strings(), app_state.install_channel)
    };

    std::thread::spawn(move || {
        let hwnd = send_hwnd.to_hwnd();
        let checked_at = now_unix_secs();
        match updater::check_for_updates() {
            Ok(UpdateCheckResult::UpToDate) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::UpToDate;
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive {
                    show_info_message(hwnd, strings.updates, strings.up_to_date);
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
            Ok(UpdateCheckResult::Available(release)) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Available(release.clone());
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();

                let auto_install = {
                    let state = lock_state();
                    state.as_ref().map(|s| s.auto_install_updates).unwrap_or(false)
                };

                if interactive {
                    if show_update_prompt(hwnd, strings, &release) {
                        match install_channel {
                            InstallChannel::Portable => begin_update_apply(hwnd, release),
                            InstallChannel::Winget => begin_winget_update(hwnd),
                        }
                    }
                } else if auto_install {
                    match install_channel {
                        // Only portable installs update themselves. A WinGet copy
                        // is updated through WinGet, which opens a console window
                        // and has no business appearing unattended.
                        InstallChannel::Portable => {
                            diagnose::log(format!(
                                "auto-installing update {}",
                                release.latest_version
                            ));
                            begin_update_apply(hwnd, release);
                        }
                        InstallChannel::Winget => diagnose::log(
                            "update available; leaving it to WinGet rather than self-updating",
                        ),
                    }
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
            Err(error) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Idle;
                        s.last_update_check_unix = Some(checked_at);
                    }
                }
                save_state_settings();
                if interactive {
                    let message = format!("{}.\n\n{}", strings.update_failed, error);
                    show_error_message(hwnd, strings.updates, &message);
                }
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

fn begin_update_apply(hwnd: HWND, release: ReleaseDescriptor) {
    let send_hwnd = SendHwnd::from_hwnd(hwnd);
    let strings = {
        let mut state = lock_state();
        let Some(app_state) = state.as_mut() else {
            return;
        };

        if matches!(
            app_state.update_status,
            UpdateStatus::Checking | UpdateStatus::Applying
        ) {
            show_info_message(
                hwnd,
                app_state.language.strings().updates,
                app_state.language.strings().update_in_progress,
            );
            return;
        }

        app_state.update_status = UpdateStatus::Applying;
        app_state.language.strings()
    };

    std::thread::spawn(move || {
        let hwnd = send_hwnd.to_hwnd();
        match updater::begin_self_update(&release) {
            Ok(()) => unsafe {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            },
            Err(error) => {
                {
                    let mut state = lock_state();
                    if let Some(s) = state.as_mut() {
                        s.update_status = UpdateStatus::Available(release);
                    }
                }
                let message = format!("{}.\n\n{}", strings.update_failed, error);
                show_error_message(hwnd, strings.updates, &message);
                unsafe {
                    let _ = PostMessageW(hwnd, WM_APP_UPDATE_CHECK_COMPLETE, WPARAM(0), LPARAM(0));
                }
            }
        }
    });
}

fn begin_winget_update(hwnd: HWND) {
    let strings = {
        let state = lock_state();
        state.as_ref().map(|s| s.language.strings())
    }
    .unwrap_or(LanguageId::English.strings());

    match updater::begin_winget_update() {
        Ok(()) => unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        },
        Err(error) => {
            let message = format!("{}.\n\n{}", strings.update_failed, error);
            show_error_message(hwnd, strings.updates, &message);
        }
    }
}

const STARTUP_REGISTRY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_REGISTRY_KEY: &str = "ClaudeCodeUsageMonitor";

/// Returns true only if the startup registry value points to this executable.
fn is_startup_enabled() -> bool {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);
        let key_name = native_interop::wide_str(STARTUP_REGISTRY_KEY);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        );
        if result.is_err() {
            return false;
        }

        // Query the size of the value
        let mut data_size: u32 = 0;
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            None,
            Some(&mut data_size),
        );
        if result.is_err() || data_size == 0 {
            let _ = RegCloseKey(hkey);
            return false;
        }

        // Read the value
        let mut buf = vec![0u8; data_size as usize];
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key_name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr()),
            Some(&mut data_size),
        );
        let _ = RegCloseKey(hkey);
        if result.is_err() {
            return false;
        }

        // Convert the registry value (UTF-16) to a string
        let wide_slice =
            std::slice::from_raw_parts(buf.as_ptr() as *const u16, data_size as usize / 2);
        let reg_value = String::from_utf16_lossy(wide_slice)
            .trim_end_matches('\0')
            .to_string();

        // Get the current executable path
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            return false;
        }
        let current_exe = String::from_utf16_lossy(&exe_buf[..len]);

        // Case-insensitive comparison (Windows paths are case-insensitive)
        reg_value.eq_ignore_ascii_case(&current_exe)
    }
}

fn set_startup_enabled(enable: bool) {
    unsafe {
        let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);

        let mut hkey = HKEY::default();
        let result = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if result.is_err() {
            return;
        }

        let key_name = native_interop::wide_str(STARTUP_REGISTRY_KEY);

        if enable {
            let mut exe_buf = [0u16; 260];
            let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
            if len > 0 {
                // Write the wide string including null terminator
                let byte_len = ((len + 1) * 2) as u32;
                let _ = RegSetValueExW(
                    hkey,
                    PCWSTR::from_raw(key_name.as_ptr()),
                    0,
                    REG_SZ,
                    Some(std::slice::from_raw_parts(
                        exe_buf.as_ptr() as *const u8,
                        byte_len as usize,
                    )),
                );
            }
        } else {
            let _ = RegDeleteValueW(hkey, PCWSTR::from_raw(key_name.as_ptr()));
        }

        let _ = RegCloseKey(hkey);
    }
}

/// The widget window's design height; the plate inside it is 40 px.
const WIDGET_HEIGHT: i32 = 46;

/// The scoped row is only meaningful for Claude, and only once the API has
/// actually reported a scoped limit.
fn scoped_row_visible(state: &AppState) -> bool {
    state.show_claude_code && state.show_scoped_weekly && !state.scoped_label.is_empty()
}
pub fn run() {
    // Enable Per-Monitor DPI Awareness V2 for crisp rendering at any scale factor
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        CURRENT_DPI.store(GetDpiForSystem(), Ordering::Relaxed);
    }
    diagnose::log("window::run started");

    // Single-instance guard: silently exit if another instance is running.
    // Exception: when relaunched after an explorer restart (ENV_RELAUNCH set),
    // wait for the previous instance to release the mutex, then take over.
    let is_relaunch = std::env::var(ENV_RELAUNCH).is_ok();
    let mutex_name = native_interop::wide_str("Global\\ClaudeCodeUsageMonitor");
    let _mutex = unsafe {
        let handle = CreateMutexW(None, true, PCWSTR::from_raw(mutex_name.as_ptr()));
        match handle {
            Ok(h) => {
                if GetLastError() == ERROR_ALREADY_EXISTS {
                    if is_relaunch {
                        diagnose::log("relaunch: waiting for previous instance to exit");
                        let wait_result = WaitForSingleObject(h, 10_000);
                        if wait_result != WAIT_OBJECT_0 && wait_result != WAIT_ABANDONED {
                            diagnose::log(format!(
                                "startup aborted: previous instance did not exit cleanly ({wait_result:?})"
                            ));
                            return;
                        }
                    } else {
                        diagnose::log("startup aborted: another instance is already running");
                        return;
                    }
                }
                h
            }
            Err(error) => {
                diagnose::log_error(
                    "startup aborted: unable to create single-instance mutex",
                    error,
                );
                return;
            }
        }
    };

    let class_name = native_interop::wide_str("ClaudeCodeUsageMonitor");

    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null()).unwrap();
        let (large_icon, small_icon) = load_embedded_app_icons();

        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(hinstance.0),
            hIcon: large_icon,
            hIconSm: small_icon,
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
            ..Default::default()
        };

        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            diagnose::log("RegisterClassExW returned 0");
        }

        let settings = load_settings();
        // Before the first width is computed below, since it depends on this.
        DETAILED_TIME.store(settings.detailed_time, Ordering::Relaxed);
        let language_override = settings.language.as_deref().and_then(LanguageId::from_code);
        let language = localization::resolve_language(language_override);
        let install_channel = updater::current_install_channel();

        // Create as layered popup (will be reparented into taskbar)
        let title = native_interop::wide_str(language.strings().window_title);
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
            PCWSTR::from_raw(class_name.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            total_widget_width(),
            widget_height(),
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        )
        .unwrap();

        if !large_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_BIG as usize),
                LPARAM(large_icon.0 as isize),
            );
        }
        if !small_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                WPARAM(ICON_SMALL as usize),
                LPARAM(small_icon.0 as isize),
            );
        }

        diagnose::log(format!("main window created hwnd={:?}", hwnd));

        let is_dark = theme::is_dark_mode();
        let mut embedded = false;

        {
            let mut state = lock_state();
            *state = Some(AppState {
                hwnd: SendHwnd::from_hwnd(hwnd),
                taskbar_hwnd: None,
                tray_notify_hwnd: None,
                win_event_hook: None,
                is_dark,
                embedded: false,
                language_override,
                language,
                install_channel,
                session_text: "--".to_string(),
                weekly_text: "--".to_string(),
                codex_session_text: "--".to_string(),
                codex_weekly_text: "--".to_string(),
                antigravity_session_text: "--".to_string(),
                antigravity_weekly_text: "--".to_string(),
                scoped_text: "--".to_string(),
                scoped_label: settings.scoped_label_last.clone().unwrap_or_default(),
                show_claude_code: settings.show_claude_code,
                show_codex: settings.show_codex,
                show_antigravity: settings.show_antigravity,
                show_scoped_weekly: settings.show_scoped_weekly,
                pace: pace_settings_from(&settings),
                data: None,
                last_poll_at: None,
                drains: Vec::new(),
                poll_interval_ms: settings.poll_interval_ms,
                retry_count: 0,
                force_notify_auth_error: false,
                auth_error_paused_polling: false,
                auth_watch_mode: poller::CredentialWatchMode::ActiveSource,
                auth_watch_snapshot: Vec::new(),
                last_poll_ok: false,
                update_status: UpdateStatus::Idle,
                last_update_check_unix: settings.last_update_check_unix,
                taskbar_index: settings.taskbar_index,
                taskbar_device: settings.taskbar_device.clone(),
                pin_to_primary_taskbar: settings.pin_to_primary_taskbar.unwrap_or(true),
                auto_install_updates: settings.auto_install_updates,
                update_check_interval_hours: settings.update_check_interval_hours,
                tray_offset: settings.tray_offset,
                press: None,
                hover: false,
                dragging: false,
                drag_start_mouse_x: 0,
                drag_start_client_x: 0,
                drag_start_offset: 0,
                widget_visible: settings.widget_visible,
            });
        }

        // Try to embed in taskbar
        wait_for_primary_taskbar(settings.pin_to_primary_taskbar.unwrap_or(true));
        if attach_to_taskbar(
            hwnd,
            settings.taskbar_index,
            settings.taskbar_device.as_deref(),
            settings.pin_to_primary_taskbar.unwrap_or(true),
        ) {
            embedded = true;
            // Record the monitor now, rather than waiting for whatever menu
            // action happens to save next.
            save_state_settings();
        }

        // If not embedded, fall back to topmost popup with SetLayeredWindowAttributes
        if !embedded {
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }

        // Register system tray icon(s)
        sync_tray_icons(hwnd);

        // Position and show (only if widget_visible preference is true)
        position_at_taskbar();
        if settings.widget_visible {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        diagnose::log("window shown");

        // Initial render via UpdateLayeredWindow (for embedded) or InvalidateRect (fallback)
        render_layered();

        // Poll timer: 15 minutes
        let initial_poll_ms = {
            let state = lock_state();
            state
                .as_ref()
                .map(|s| s.poll_interval_ms)
                .unwrap_or(POLL_15_MIN)
        };
        SetTimer(hwnd, TIMER_POLL, initial_poll_ms, None);

        // Watch for explorer.exe restarts so we can re-embed and re-add the tray
        // icon (the shell discards tray registrations when it restarts). This
        // runs on a dedicated thread, NOT a window timer: once explorer destroys
        // the taskbar, our embedded child window stops receiving all messages
        // (WM_TIMER included), so a timer would never fire again.
        spawn_taskbar_watchdog();

        // Initial poll
        let send_hwnd = SendHwnd::from_hwnd(hwnd);
        std::thread::spawn(move || {
            diagnose::log("initial poll thread started");
            do_poll(send_hwnd);
        });

        schedule_auto_update_check(hwnd);
        let should_check_updates = {
            let state = lock_state();
            state
                .as_ref()
                .map(|s| {
                    auto_update_check_due(s.last_update_check_unix, s.update_check_interval_hours)
                })
                .unwrap_or(false)
        };
        if should_check_updates {
            begin_update_check(hwnd, false);
        }

        // Initial theme check
        check_theme_change();

        // Message loop
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Draws the plate and pushes it to the layered window. Everything goes through
/// GDI+ into a premultiplied bitmap, so edges and glyphs blend with whatever the
/// taskbar shows behind them instead of against an assumed colour.
pub(crate) fn render_layered() {
    refresh_dpi();
    let (hwnd_val, embedded, model) = {
        let state = lock_state();
        match state.as_ref() {
            Some(s) => (s.hwnd, s.embedded, widget_model(s)),
            None => return,
        }
    };
    let hwnd = hwnd_val.to_hwnd();
    let k = scale();

    let Some(layout) = cockpit::Painter::measuring()
        .map(|painter| cockpit::widget_layout(&painter, &model, k))
    else {
        return;
    };
    // The plate is right-anchored, so a new width means a new position.
    if WIDGET_W.swap(layout.width, Ordering::Relaxed) != layout.width {
        position_at_taskbar();
    }

    // For non-embedded fallback, just invalidate and let WM_PAINT handle it
    if !embedded {
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
        return;
    }

    let width = layout.width;
    let height = widget_height();

    unsafe {
        let screen_dc = GetDC(hwnd);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0, // BI_RGB
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let mem_dc = CreateCompatibleDC(screen_dc);
        let dib =
            CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();

        if dib.is_invalid() || bits.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(hwnd, screen_dc);
            return;
        }

        let old_bmp = SelectObject(mem_dc, dib);
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u32, (width * height) as usize);
        pixels.fill(0);

        if let Some(painter) = cockpit::Painter::new(bits, width, height) {
            cockpit::paint_widget(&painter, &model, &layout, k, height);
        }

        // Fully transparent pixels would let clicks fall through to the taskbar;
        // an alpha of 1 is invisible and still ours.
        for px in pixels.iter_mut() {
            if *px >> 24 == 0 {
                *px = 0x0100_0000;
            }
        }

        let pt_src = POINT { x: 0, y: 0 };
        let sz = SIZE {
            cx: width,
            cy: height,
        };
        let blend = BLENDFUNCTION {
            BlendOp: 0, // AC_SRC_OVER
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: 1, // AC_SRC_ALPHA
        };

        let _ = UpdateLayeredWindow(
            hwnd,
            screen_dc,
            None,
            Some(&sz),
            mem_dc,
            Some(&pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );

        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(hwnd, screen_dc);
    }
}

/// A window worth drawing: it was reported at all. An absent window leaves its
/// place empty instead of claiming a zero.
fn reported(section: &UsageSection) -> bool {
    !(section.resets_at.is_none() && section.percentage == 0.0)
}

/// A window that started over between two polls: its reset moved on by far
/// more than jitter, and less has been spent since.
fn rolled_over(before: &UsageSection, after: &UsageSection) -> bool {
    match (before.resets_at, after.resets_at) {
        (Some(before_reset), Some(after_reset)) => {
            after_reset > before_reset + Duration::from_secs(30 * 60)
                && after.percentage < before.percentage
        }
        _ => false,
    }
}

/// Windows' own "Show animations" switch; off means the drain is skipped.
fn animations_enabled() -> bool {
    let mut enabled = BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some(&mut enabled as *mut BOOL as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    enabled.as_bool()
}
fn do_poll(send_hwnd: SendHwnd) {
    let hwnd = send_hwnd.to_hwnd();
    let active = {
        let state = lock_state();
        state
            .as_ref()
            .map(|s| s.active_providers())
            .unwrap_or_else(|| vec![ProviderId::ClaudeCode])
    };

    match poller::poll(&active) {
        Ok(data) => {
            let mut scoped_label_changed = false;
            let mut state = lock_state();
            if let Some(s) = state.as_mut() {
                // Stop fast-poll if reset data is now fresh
                if !poller::app_is_past_reset(&data) {
                    unsafe {
                        let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                    }
                }

                if let Some(previous) = s.data.as_ref().filter(|_| animations_enabled()) {
                    let started = Instant::now();
                    for entry in data.iter() {
                        let Some(before) = previous.get(entry.id) else { continue };
                        for lane in [LANE_SESSION, LANE_WEEKLY, LANE_SCOPED] {
                            if let (Some(old), Some(new)) =
                                (lane_section(before, lane), lane_section(&entry.data, lane))
                            {
                                if rolled_over(old, new) {
                                    s.drains.push(Drain {
                                        provider: entry.id,
                                        lane,
                                        from: old.percentage.clamp(0.0, 100.0) / 100.0,
                                        started,
                                    });
                                }
                            }
                        }
                    }
                }

                s.data = Some(data);
                s.last_poll_at = Some(SystemTime::now());
                s.last_poll_ok = true;
                let label_before = s.scoped_label.clone();
                refresh_usage_texts(s);
                scoped_label_changed = s.scoped_label != label_before;

                // Recovered from errors — restore normal poll interval
                if s.retry_count > 0 {
                    s.retry_count = 0;
                    let interval = s.poll_interval_ms;
                    unsafe {
                        SetTimer(hwnd, TIMER_POLL, interval, None);
                    }
                }
                s.force_notify_auth_error = false;
                s.auth_error_paused_polling = false;
                s.auth_watch_mode = poller::CredentialWatchMode::ActiveSource;
                s.auth_watch_snapshot.clear();
            }

            drop(state);
            if scoped_label_changed {
                save_state_settings();
            }

            unsafe {
                let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(0), LPARAM(0));
            }
        }
        Err(e) => {
            let auth_watch = match e {
                poller::PollError::AuthRequired | poller::PollError::TokenExpired
                    if active == [ProviderId::Antigravity] =>
                {
                    Some((
                        poller::CredentialWatchMode::Antigravity,
                        poller::credential_watch_snapshot(poller::CredentialWatchMode::Antigravity),
                    ))
                }
                poller::PollError::AuthRequired | poller::PollError::TokenExpired => Some((
                    poller::CredentialWatchMode::ActiveSource,
                    poller::credential_watch_snapshot(poller::CredentialWatchMode::ActiveSource),
                )),
                poller::PollError::NoCredentials => Some((
                    poller::CredentialWatchMode::AllSources,
                    poller::credential_watch_snapshot(poller::CredentialWatchMode::AllSources),
                )),
                poller::PollError::RequestFailed => None,
            };
            // Distinguish auth-required errors from transient errors.
            let notify_auth_error = {
                let mut state = lock_state();
                let mut should_notify = false;
                if let Some(s) = state.as_mut() {
                    s.last_poll_ok = false;
                    match auth_watch {
                        Some((watch_mode, watch_snapshot)) => {
                            // Only show the balloon on the first failure so it doesn't spam.
                            if s.retry_count == 0 || s.force_notify_auth_error {
                                should_notify = true;
                            }
                            s.force_notify_auth_error = false;
                            s.auth_error_paused_polling = true;
                            s.auth_watch_mode = watch_mode;
                            s.auth_watch_snapshot = watch_snapshot;
                            s.session_text = "!".to_string();
                            s.weekly_text = "!".to_string();
                            s.codex_session_text = "!".to_string();
                            s.codex_weekly_text = "!".to_string();
                            s.antigravity_session_text = "!".to_string();
                            s.antigravity_weekly_text = "!".to_string();
                            s.retry_count = s.retry_count.saturating_add(1);
                            unsafe {
                                let _ = KillTimer(hwnd, TIMER_POLL);
                                let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                                let _ = KillTimer(hwnd, TIMER_COUNTDOWN);
                                SetTimer(hwnd, TIMER_POLL, s.poll_interval_ms, None);
                            }
                        }
                        _ => {
                            // Transient network / credential-missing errors: exponential backoff.
                            s.force_notify_auth_error = false;
                            s.auth_error_paused_polling = false;
                            s.auth_watch_mode = poller::CredentialWatchMode::ActiveSource;
                            s.auth_watch_snapshot.clear();
                            s.session_text = "...".to_string();
            s.scoped_text = "...".to_string();
                            s.weekly_text = "...".to_string();
                            s.codex_session_text = "...".to_string();
                            s.codex_weekly_text = "...".to_string();
                            s.antigravity_session_text = "...".to_string();
                            s.antigravity_weekly_text = "...".to_string();
                            s.retry_count = s.retry_count.saturating_add(1);
                            let backoff = RETRY_BASE_MS.saturating_mul(
                                1u32.checked_shl(s.retry_count - 1).unwrap_or(u32::MAX),
                            );
                            let retry_ms = backoff.min(s.poll_interval_ms);
                            unsafe {
                                let _ = KillTimer(hwnd, TIMER_RESET_POLL);
                                SetTimer(hwnd, TIMER_POLL, retry_ms, None);
                            }
                        }
                    }
                }
                should_notify
            };

            if notify_auth_error {
                let balloon = {
                    let state = lock_state();
                    state.as_ref().map(|s| {
                        if s.show_claude_code {
                            (
                                s.language.strings(),
                                tray_icon::TrayIconKind::Claude,
                                s.language.strings().token_expired_title,
                                s.language.strings().token_expired_body,
                            )
                        } else if s.show_codex {
                            (
                                s.language.strings(),
                                tray_icon::TrayIconKind::Codex,
                                s.language.strings().codex_token_expired_title,
                                s.language.strings().codex_token_expired_body,
                            )
                        } else {
                            (
                                s.language.strings(),
                                tray_icon::TrayIconKind::Antigravity,
                                s.language.strings().antigravity_token_expired_title,
                                s.language.strings().antigravity_token_expired_body,
                            )
                        }
                    })
                };
                if let Some((_strings, kind, title, body)) = balloon {
                    tray_icon::notify_balloon(hwnd, kind, title, body);
                }
            }

            unsafe {
                let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn schedule_countdown_timer() {
    let state = lock_state();
    let s = match state.as_ref() {
        Some(s) => s,
        None => return,
    };

    let hwnd = s.hwnd.to_hwnd();
    if !s.last_poll_ok {
        unsafe {
            let _ = KillTimer(hwnd, TIMER_COUNTDOWN);
            let _ = KillTimer(hwnd, TIMER_RESET_POLL);
        }
        return;
    }

    let data = match &s.data {
        Some(d) => d,
        None => return,
    };

    // If a reset time has passed, poll every 5s to pick up fresh data
    if poller::app_is_past_reset(data) {
        unsafe {
            SetTimer(hwnd, TIMER_RESET_POLL, 5_000, None);
        }
    }

    let detailed = detailed_time();
    let delays = data.iter().flat_map(|entry| {
        [
            poller::time_until_display_change(entry.data.session.resets_at, detailed),
            poller::time_until_display_change(entry.data.weekly.resets_at, detailed),
        ]
    });
    let min_delay = delays.flatten().min();

    let ms = min_delay
        .unwrap_or(Duration::from_secs(60))
        .as_millis()
        .max(1000) as u32;

    unsafe {
        SetTimer(hwnd, TIMER_COUNTDOWN, ms, None);
    }
}

fn check_theme_change() {
    let new_dark = theme::is_dark_mode();
    let changed = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            if s.is_dark != new_dark {
                s.is_dark = new_dark;
                true
            } else {
                false
            }
        } else {
            false
        }
    };
    if changed {
        render_layered();
        flyout::refresh();
        // The badges are drawn in the panel's day or night palette too.
        let hwnd = lock_state().as_ref().map(|s| s.hwnd.to_hwnd());
        if let Some(hwnd) = hwnd {
            sync_tray_icons(hwnd);
        }
    }
}

fn check_language_change() {
    if update_language_change() {
        render_layered();
        flyout::refresh();
    }
}

fn update_display() {
    let mut state = lock_state();
    let s = match state.as_mut() {
        Some(s) => s,
        None => return,
    };

    // Don't overwrite error text with stale cached data
    if !s.last_poll_ok {
        return;
    }

    refresh_usage_texts(s);
}

fn suppress_tray_reposition_for(duration: Duration) {
    let mut until = SUPPRESS_TRAY_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    *until = Some(Instant::now() + duration);
}

fn tray_reposition_is_suppressed() -> bool {
    let now = Instant::now();
    let mut until = SUPPRESS_TRAY_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|e| e.into_inner());

    match *until {
        Some(deadline) if now < deadline => true,
        Some(_) => {
            *until = None;
            false
        }
        None => false,
    }
}

fn position_at_taskbar() {
    refresh_dpi();
    // Drop the app-state lock before any Win32 call that may synchronously
    // re-enter our window procedure.
    let (hwnd, embedded, tray_offset, taskbar_hwnd) = {
        let state = lock_state();
        let s = match state.as_ref() {
            Some(s) => s,
            None => return,
        };

        // Don't fight the user's drag
        if s.dragging {
            return;
        }

        let taskbar_hwnd = match s.taskbar_hwnd {
            Some(h) => h,
            None => {
                diagnose::log("position_at_taskbar skipped: no taskbar handle");
                return;
            }
        };

        (s.hwnd.to_hwnd(), s.embedded, s.tray_offset, taskbar_hwnd)
    };

    let taskbar_rect = match native_interop::get_taskbar_rect(taskbar_hwnd) {
        Some(r) => r,
        None => {
            diagnose::log("position_at_taskbar skipped: unable to query taskbar rect");
            return;
        }
    };

    let taskbar_height = taskbar_rect.bottom - taskbar_rect.top;
    let mut tray_left = taskbar_rect.right;
    let anchor_top = taskbar_rect.top;
    let anchor_height = taskbar_height;

    if let Some(tray_hwnd) = native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd") {
        if let Some(tray_rect) = native_interop::get_window_rect_safe(tray_hwnd) {
            tray_left = tray_rect.left;
        }
    }

    let widget_width = total_widget_width();
    let max_offset = (tray_left - taskbar_rect.left - widget_width).max(0);
    let tray_offset = tray_offset.clamp(0, max_offset);
    let offset_changed = {
        let mut state = lock_state();
        if let Some(s) = state.as_mut() {
            if s.tray_offset != tray_offset {
                s.tray_offset = tray_offset;
                true
            } else {
                false
            }
        } else {
            false
        }
    };
    if offset_changed {
        save_state_settings();
    }

    let widget_height = sc(WIDGET_HEIGHT).min(taskbar_height).max(1);
    WIDGET_H.store(widget_height, Ordering::Relaxed);
    let y = compute_anchor_y(anchor_top, anchor_height, widget_height);
    if embedded {
        // Child window: coordinates relative to parent (taskbar)
        let x = tray_left - taskbar_rect.left - widget_width - tray_offset;
        native_interop::move_window(hwnd, x, y - taskbar_rect.top, widget_width, widget_height);
        diagnose::log(format!(
            "positioned embedded widget at x={x} y={} w={widget_width} h={widget_height}",
            y - taskbar_rect.top
        ));
    } else {
        // Topmost popup: screen coordinates
        let x = tray_left - widget_width - tray_offset;
        native_interop::move_window(hwnd, x, y, widget_width, widget_height);
        diagnose::log(format!(
            "positioned fallback widget at x={x} y={y} w={widget_width} h={widget_height}"
        ));
    }
}

fn compute_anchor_y(anchor_top: i32, anchor_height: i32, widget_height: i32) -> i32 {
    let anchor_bottom = anchor_top + anchor_height;
    (anchor_bottom - widget_height).max(anchor_top)
}

/// WinEvent callback for tray icon location changes
unsafe extern "system" fn on_tray_location_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    static LAST_REPOSITION: Mutex<Option<std::time::Instant>> = Mutex::new(None);

    let is_tray = {
        let state = lock_state();
        state
            .as_ref()
            .and_then(|s| s.tray_notify_hwnd)
            .map(|h| h == hwnd)
            .unwrap_or(false)
    };

    if is_tray {
        if tray_reposition_is_suppressed() {
            return;
        }

        let should_reposition = {
            let mut last = LAST_REPOSITION.lock().unwrap_or_else(|e| e.into_inner());
            let now = std::time::Instant::now();
            if last
                .map(|t| now.duration_since(t).as_millis() > 500)
                .unwrap_or(true)
            {
                *last = Some(now);
                true
            } else {
                false
            }
        };
        if should_reposition {
            position_at_taskbar();
            render_layered();
        }
    }
}

/// Main window procedure
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            // For non-embedded fallback, paint normally
            let embedded = {
                let state = lock_state();
                state.as_ref().map(|s| s.embedded).unwrap_or(false)
            };
            if embedded {
                // Layered windows don't use WM_PAINT; just validate the region
                let mut ps = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut ps);
                let _ = EndPaint(hwnd, &ps);
            } else {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                paint(hdc, hwnd);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_DISPLAYCHANGE | WM_DPICHANGED_MSG | WM_SETTINGCHANGE => {
            if msg == WM_DPICHANGED_MSG {
                let new_dpi = (wparam.0 & 0xFFFF) as u32;
                CURRENT_DPI.store(new_dpi, Ordering::Relaxed);
            }
            if msg == WM_SETTINGCHANGE {
                check_theme_change();
                check_language_change();
            }
            refresh_dpi();
            position_at_taskbar();
            render_layered();
            LRESULT(0)
        }
        WM_TIMER => {
            let timer_id = wparam.0;
            match timer_id {
                TIMER_POLL => {
                    let auth_watch = {
                        let state = lock_state();
                        state.as_ref().map(|s| {
                            (
                                s.auth_error_paused_polling,
                                s.auth_watch_mode,
                                s.auth_watch_snapshot.clone(),
                            )
                        })
                    };
                    match auth_watch {
                        Some((true, watch_mode, previous_snapshot)) => {
                            let current_snapshot = poller::credential_watch_snapshot(watch_mode);
                            if current_snapshot != previous_snapshot {
                                let mut state = lock_state();
                                if let Some(s) = state.as_mut() {
                                    if s.auth_error_paused_polling
                                        && s.auth_watch_mode == watch_mode
                                    {
                                        s.auth_watch_snapshot = current_snapshot;
                                    }
                                }
                                drop(state);
                                let sh = SendHwnd::from_hwnd(hwnd);
                                std::thread::spawn(move || {
                                    do_poll(sh);
                                });
                            }
                        }
                        Some((false, _, _)) => {
                            let sh = SendHwnd::from_hwnd(hwnd);
                            std::thread::spawn(move || {
                                do_poll(sh);
                            });
                        }
                        None => {}
                    }
                }
                TIMER_COUNTDOWN => {
                    update_display();
                    render_layered();
                    flyout::refresh();
                    schedule_countdown_timer();
                }
                TIMER_ANIM => {
                    let running = {
                        let mut state = lock_state();
                        state.as_mut().is_some_and(|s| {
                            // Keep a finished drain for one more frame, so the
                            // last one drawn is the resting value.
                            s.drains.retain(|drain| drain.started.elapsed() < DRAIN_DURATION * 2);
                            !s.drains.is_empty()
                        })
                    };
                    if !running {
                        let _ = KillTimer(hwnd, TIMER_ANIM);
                    }
                    render_layered();
                }
                TIMER_RESET_POLL => {
                    let should_poll = {
                        let state = lock_state();
                        state
                            .as_ref()
                            .map(|s| !s.auth_error_paused_polling)
                            .unwrap_or(false)
                    };
                    if should_poll {
                        let sh = SendHwnd::from_hwnd(hwnd);
                        std::thread::spawn(move || {
                            do_poll(sh);
                        });
                    }
                }
                TIMER_UPDATE_CHECK => {
                    begin_update_check(hwnd, false);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_USAGE_UPDATED => {
            check_theme_change();
            check_language_change();
            // The per-model row widens the label column, so poll data can change
            // the widget's width. It is right-anchored, so a resize without a
            // reposition slides it under the notification area.
            position_at_taskbar();
            render_layered();
            flyout::refresh();
            let draining = lock_state().as_ref().is_some_and(|s| !s.drains.is_empty());
            if draining {
                SetTimer(hwnd, TIMER_ANIM, 16, None);
            }
            schedule_countdown_timer();
            suppress_tray_reposition_for(Duration::from_millis(
                TRAY_ICON_UPDATE_REPOSITION_SUPPRESS_MS,
            ));
            sync_tray_icons(hwnd);
            LRESULT(0)
        }
        WM_APP_UPDATE_CHECK_COMPLETE => {
            schedule_auto_update_check(hwnd);
            LRESULT(0)
        }
        WM_APP_REATTACH => {
            let (index, device, pinned) = {
                let state = lock_state();
                match state.as_ref() {
                    Some(s) => (
                        s.taskbar_index,
                        s.taskbar_device.clone(),
                        s.pin_to_primary_taskbar,
                    ),
                    None => return LRESULT(0),
                }
            };
            if attach_to_taskbar(hwnd, index, device.as_deref(), pinned) {
                save_state_settings();
                position_at_taskbar();
                render_layered();
                // A taskbar rebuild discards the shell's tray registrations, which
                // is why the watchdog exists at all; the relaunch path used to
                // restore them on startup and this path has to do it itself.
                sync_tray_icons(hwnd);
            }
            LRESULT(0)
        }
        WM_SETCURSOR => {
            let is_dragging = {
                let state = lock_state();
                state.as_ref().map(|s| s.dragging).unwrap_or(false)
            };
            if is_dragging {
                let cursor = LoadCursorW(HINSTANCE::default(), IDC_SIZEWE).unwrap_or_default();
                SetCursor(cursor);
                return LRESULT(1);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN => {
            // Anywhere on the plate: a press is a click until it travels.
            let client_x = (lparam.0 & 0xFFFF) as i16 as i32;
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    s.press = Some((pt.x, pt.y, client_x));
                }
            }
            SetCapture(hwnd);
            render_layered();
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            {
                let mut state = lock_state();
                if let Some(s) = state.as_mut() {
                    s.hover = false;
                }
            }
            render_layered();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            // Capture taken away mid-gesture (a menu, a focus change): end it
            // here, or the drag flag would block every later reposition.
            let ended_drag = {
                let mut state = lock_state();
                state.as_mut().is_some_and(|s| {
                    s.press = None;
                    std::mem::replace(&mut s.dragging, false)
                })
            };
            if ended_drag {
                save_state_settings();
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let (threshold_x, threshold_y) = (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG));
            let entered = {
                let mut state = lock_state();
                state
                    .as_mut()
                    .is_some_and(|s| !std::mem::replace(&mut s.hover, true))
            };
            if entered {
                // Ask for the WM_MOUSELEAVE that puts the plate back to rest.
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
                render_layered();
            }
            let (is_dragging, drag_began) = {
                let mut state = lock_state();
                match state.as_mut() {
                    Some(s) => {
                        let mut began = false;
                        if let (false, Some((x0, y0, client_x))) = (s.dragging, s.press) {
                            if (pt.x - x0).abs() > threshold_x || (pt.y - y0).abs() > threshold_y {
                                s.dragging = true;
                                s.drag_start_mouse_x = x0;
                                s.drag_start_client_x = client_x;
                                s.drag_start_offset = s.tray_offset;
                                began = true;
                            }
                        }
                        (s.dragging, began)
                    }
                    None => (false, false),
                }
            };
            if drag_began {
                flyout::hide();
            }
            if is_dragging {
                let move_target = {
                    let mut state = lock_state();
                    let s = match state.as_mut() {
                        Some(s) => s,
                        None => return LRESULT(0),
                    };

                    // Moving mouse left = positive delta = larger offset (further left)
                    let delta = s.drag_start_mouse_x - pt.x;
                    let mut new_offset = s.drag_start_offset + delta;

                    // Clamp: offset >= 0 (can't go right of default)
                    if new_offset < 0 {
                        new_offset = 0;
                    }

                    let taskbar_hwnd = s.taskbar_hwnd;
                    let embedded = s.embedded;
                    let hwnd_val = s.hwnd.to_hwnd();

                    // Clamp: don't go past left edge of taskbar
                    if let Some(taskbar_hwnd) = taskbar_hwnd {
                        if let Some(taskbar_rect) = native_interop::get_taskbar_rect(taskbar_hwnd) {
                            let mut tray_left = taskbar_rect.right;
                            if let Some(tray_hwnd) =
                                native_interop::find_child_window(taskbar_hwnd, "TrayNotifyWnd")
                            {
                                if let Some(tray_rect) =
                                    native_interop::get_window_rect_safe(tray_hwnd)
                                {
                                    tray_left = tray_rect.left;
                                }
                            }
                            let widget_width = total_widget_width();
                            let max_offset = (tray_left - taskbar_rect.left - widget_width).max(0);
                            if new_offset > max_offset {
                                new_offset = max_offset;
                            }

                            s.tray_offset = new_offset;

                            let taskbar_height = taskbar_rect.bottom - taskbar_rect.top;
                            let anchor_top = taskbar_rect.top;
                            let anchor_height = taskbar_height;
                            let widget_height = widget_height();
                            let y = compute_anchor_y(anchor_top, anchor_height, widget_height);
                            let x = if embedded {
                                tray_left - taskbar_rect.left - widget_width - new_offset
                            } else {
                                tray_left - widget_width - new_offset
                            };
                            Some((
                                hwnd_val,
                                embedded,
                                x,
                                y,
                                taskbar_rect.top,
                                widget_width,
                                widget_height,
                            ))
                        } else {
                            s.tray_offset = new_offset;
                            None
                        }
                    } else {
                        s.tray_offset = new_offset;
                        None
                    }
                };

                if let Some((hwnd_val, embedded, x, y, taskbar_top, widget_width, widget_height)) =
                    move_target
                {
                    if embedded {
                        native_interop::move_window(
                            hwnd_val,
                            x,
                            y - taskbar_top,
                            widget_width,
                            widget_height,
                        );
                    } else {
                        native_interop::move_window(hwnd_val, x, y, widget_width, widget_height);
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let (drag_result, clicked) = {
                let mut state = lock_state();
                match state.as_mut() {
                    Some(s) => {
                        let pressed = s.press.take().is_some();
                        if s.dragging {
                            s.dragging = false;
                            (Some((s.taskbar_index, s.drag_start_client_x)), false)
                        } else {
                            (None, pressed)
                        }
                    }
                    None => (None, false),
                }
            };
            let _ = ReleaseCapture();
            if clicked {
                flyout::toggle(hwnd);
            }
            // Out of the pressed look, into whatever the flyout left it in.
            render_layered();
            if let Some((current_taskbar_index, drag_start_client_x)) = drag_result {
                if let Some((target_index, target_taskbar)) = taskbar_at_point(pt) {
                    if target_index != current_taskbar_index {
                        let new_offset = offset_for_drop_point(
                            target_taskbar.hwnd,
                            target_taskbar.rect,
                            pt,
                            drag_start_client_x,
                        );
                        {
                            let mut state = lock_state();
                            if let Some(s) = state.as_mut() {
                                s.tray_offset = new_offset;
                            }
                        }
                        // Dragging onto another taskbar is an explicit choice, so
                        // the remembered monitor is deliberately not consulted:
                        // the index decides, and the new monitor is recorded.
                        // Taken from the taskbar the drop actually landed on. Re-enumerating
                        // here and indexing by position could read a different screen,
                        // or none, if the shell dropped a taskbar in between.
                        let dropped_on_primary = target_taskbar.is_primary;
                        {
                            let mut state = lock_state();
                            if let Some(s) = state.as_mut() {
                                s.pin_to_primary_taskbar = dropped_on_primary;
                            }
                        }
                        if attach_to_taskbar(hwnd, target_index, None, false) {
                            position_at_taskbar();
                            render_layered();
                        }
                    }
                }
                save_state_settings();
            }
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            show_context_menu(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = wparam.0 as u16;
            match id {
                1 => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.session_text = "...".to_string();
            s.scoped_text = "...".to_string();
                            s.weekly_text = "...".to_string();
                            s.codex_session_text = "...".to_string();
                            s.codex_weekly_text = "...".to_string();
                            s.force_notify_auth_error = true;
                        }
                    }
                    render_layered();
                    let sh = SendHwnd::from_hwnd(hwnd);
                    std::thread::spawn(move || {
                        do_poll(sh);
                    });
                }
                IDM_VERSION_ACTION => {
                    let (install_channel, release) = {
                        let state = lock_state();
                        match state.as_ref() {
                            Some(s) => (
                                s.install_channel,
                                match &s.update_status {
                                    UpdateStatus::Available(release) => Some(release.clone()),
                                    _ => None,
                                },
                            ),
                            None => (InstallChannel::Portable, None),
                        }
                    };

                    match install_channel {
                        InstallChannel::Winget => {
                            if release.is_some() {
                                begin_winget_update(hwnd);
                            } else {
                                begin_update_check(hwnd, true);
                            }
                        }
                        InstallChannel::Portable => {
                            if let Some(release) = release {
                                begin_update_apply(hwnd, release);
                            } else {
                                begin_update_check(hwnd, true);
                            }
                        }
                    }
                }
                2 => {
                    let hook = {
                        let state = lock_state();
                        state.as_ref().and_then(|s| s.win_event_hook)
                    };
                    if let Some(h) = hook {
                        native_interop::unhook_win_event(h);
                    }
                    PostQuitMessage(0);
                }
                IDM_RESET_POSITION => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.tray_offset = 0;
                        }
                    }
                    save_state_settings();
                    position_at_taskbar();
                }
                IDM_START_WITH_WINDOWS => {
                    set_startup_enabled(!is_startup_enabled());
                }
                IDM_FREQ_1MIN | IDM_FREQ_5MIN | IDM_FREQ_15MIN | IDM_FREQ_1HOUR => {
                    let new_interval = match id {
                        IDM_FREQ_1MIN => POLL_1_MIN,
                        IDM_FREQ_5MIN => POLL_5_MIN,
                        IDM_FREQ_15MIN => POLL_15_MIN,
                        IDM_FREQ_1HOUR => POLL_1_HOUR,
                        _ => POLL_15_MIN,
                    };
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.poll_interval_ms = new_interval;
                        }
                    }
                    save_state_settings();
                    // Reset the poll timer with the new interval
                    SetTimer(hwnd, TIMER_POLL, new_interval, None);
                }
                IDM_AUTO_INSTALL_UPDATES => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            s.auto_install_updates = !s.auto_install_updates;
                        }
                    }
                    save_state_settings();
                    // Turning it on should not wait for the next interval to
                    // elapse before it ever acts.
                    schedule_auto_update_check(hwnd);
                }
                IDM_DETAILED_TIME => {
                    DETAILED_TIME.store(!detailed_time(), Ordering::Relaxed);
                    save_state_settings();
                    // The countdown text, the column it sits in and how often it
                    // ticks all change together.
                    update_display();
                    position_at_taskbar();
                    render_layered();
                    schedule_countdown_timer();
                }
                IDM_PACE_COLORS | IDM_SCOPED_WEEKLY_ROW => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            match id {
                                IDM_PACE_COLORS => s.pace.enabled = !s.pace.enabled,
                                IDM_SCOPED_WEEKLY_ROW => {
                                    s.show_scoped_weekly = !s.show_scoped_weekly;
                                }
                                _ => {}
                            }
                        }
                    }
                    save_state_settings();
                    position_at_taskbar();
                    render_layered();
                    sync_tray_icons(hwnd);
                }
                _ if providers::from_menu_id(id).is_some() => {
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            if let Some(provider) = providers::from_menu_id(id) {
                                // Whatever the user clicks, one provider stays
                                // on screen: an empty widget helps nobody.
                                let shown = s.is_shown(provider);
                                if !shown || s.active_providers().len() > 1 {
                                    s.set_shown(provider, !shown);
                                }
                            }
                            s.mark_texts_pending();
                        }
                    }
                    save_state_settings();
                    position_at_taskbar();
                    render_layered();
                    sync_tray_icons(hwnd);
                    let sh = SendHwnd::from_hwnd(hwnd);
                    std::thread::spawn(move || {
                        do_poll(sh);
                    });
                }
                IDM_LANG_SYSTEM
                | IDM_LANG_ENGLISH
                | IDM_LANG_DUTCH
                | IDM_LANG_SPANISH
                | IDM_LANG_FRENCH
                | IDM_LANG_GERMAN
                | IDM_LANG_JAPANESE
                | IDM_LANG_KOREAN
                | IDM_LANG_TRADITIONAL_CHINESE
                | IDM_LANG_SIMPLIFIED_CHINESE
                | IDM_LANG_RUSSIAN
                | IDM_LANG_PORTUGUESE_BRAZIL => {
                    let language_override = match id {
                        IDM_LANG_SYSTEM => None,
                        IDM_LANG_ENGLISH => Some(LanguageId::English),
                        IDM_LANG_DUTCH => Some(LanguageId::Dutch),
                        IDM_LANG_SPANISH => Some(LanguageId::Spanish),
                        IDM_LANG_FRENCH => Some(LanguageId::French),
                        IDM_LANG_GERMAN => Some(LanguageId::German),
                        IDM_LANG_JAPANESE => Some(LanguageId::Japanese),
                        IDM_LANG_KOREAN => Some(LanguageId::Korean),
                        IDM_LANG_TRADITIONAL_CHINESE => Some(LanguageId::TraditionalChinese),
                        IDM_LANG_SIMPLIFIED_CHINESE => Some(LanguageId::SimplifiedChinese),
                        IDM_LANG_RUSSIAN => Some(LanguageId::Russian),
                        IDM_LANG_PORTUGUESE_BRAZIL => Some(LanguageId::PortugueseBrazil),
                        _ => None,
                    };
                    {
                        let mut state = lock_state();
                        if let Some(s) = state.as_mut() {
                            apply_language_to_state(s, language_override);
                        }
                    }
                    save_state_settings();
                    render_layered();
                }
                id if id == tray_icon::IDM_TOGGLE_WIDGET => {
                    toggle_widget_visibility(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        _ if msg == WM_APP_TRAY => {
            match tray_icon::handle_message(lparam) {
                tray_icon::TrayAction::ToggleWidget => {
                    toggle_widget_visibility(hwnd);
                }
                tray_icon::TrayAction::ShowContextMenu => {
                    show_context_menu(hwnd);
                }
                tray_icon::TrayAction::None => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let hook = {
                let state = lock_state();
                state.as_ref().and_then(|s| s.win_event_hook)
            };
            if let Some(h) = hook {
                native_interop::unhook_win_event(h);
            }
            tray_icon::remove_all(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

pub(crate) fn show_context_menu(hwnd: HWND) {
    unsafe {
        let (
            current_interval,
            strings,
            language,
            language_override,
            install_channel,
            update_status,
            widget_visible,
            show_claude_code,
            show_codex,
            show_antigravity,
            pace_colors_enabled,
            show_scoped_weekly,
            scoped_label,
            auto_install_updates,
        ) = {
            let state = lock_state();
            match state.as_ref() {
                Some(s) => (
                    s.poll_interval_ms,
                    s.language.strings(),
                    s.language,
                    s.language_override,
                    s.install_channel,
                    s.update_status.clone(),
                    s.widget_visible,
                    s.show_claude_code,
                    s.show_codex,
                    s.show_antigravity,
                    s.pace.enabled,
                    s.show_scoped_weekly,
                    s.scoped_label.clone(),
                    s.auto_install_updates,
                ),
                None => (
                    POLL_15_MIN,
                    LanguageId::English.strings(),
                    LanguageId::English,
                    None,
                    InstallChannel::Portable,
                    UpdateStatus::Idle,
                    true,
                    true,
                    false,
                    false,
                    true,
                    true,
                    String::new(),
                    true,
                ),
            }
        };

        let menu = CreatePopupMenu().unwrap();

        let refresh_str = native_interop::wide_str(strings.refresh);
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(0),
            1,
            PCWSTR::from_raw(refresh_str.as_ptr()),
        );

        // Update Frequency submenu
        let freq_menu = CreatePopupMenu().unwrap();
        let freq_items: [(u16, u32, &str); 4] = [
            (IDM_FREQ_1MIN, POLL_1_MIN, strings.one_minute),
            (IDM_FREQ_5MIN, POLL_5_MIN, strings.five_minutes),
            (IDM_FREQ_15MIN, POLL_15_MIN, strings.fifteen_minutes),
            (IDM_FREQ_1HOUR, POLL_1_HOUR, strings.one_hour),
        ];
        for (id, interval, label) in freq_items {
            let label_str = native_interop::wide_str(label);
            let flags = if interval == current_interval {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                freq_menu,
                flags,
                id as usize,
                PCWSTR::from_raw(label_str.as_ptr()),
            );
        }

        let freq_label = native_interop::wide_str(strings.update_frequency);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            freq_menu.0 as usize,
            PCWSTR::from_raw(freq_label.as_ptr()),
        );

        // Models submenu, generated from the provider table: a new provider
        // appears here without another hand-written block.
        let models_menu = CreatePopupMenu().unwrap();
        let provider_is_shown = |provider: ProviderId| match provider {
            ProviderId::ClaudeCode => show_claude_code,
            ProviderId::Codex => show_codex,
            ProviderId::Antigravity => show_antigravity,
        };
        for provider in providers::PROVIDERS {
            let label = native_interop::wide_str(provider.label(strings));
            let flags = if provider_is_shown(provider) {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                models_menu,
                flags,
                provider.menu_id() as usize,
                PCWSTR::from_raw(label.as_ptr()),
            );
        }

        let models_label = native_interop::wide_str(strings.models);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            models_menu.0 as usize,
            PCWSTR::from_raw(models_label.as_ptr()),
        );

        // Settings submenu
        let settings_menu = CreatePopupMenu().unwrap();

        let startup_str = native_interop::wide_str(strings.start_with_windows);
        let startup_flags = if is_startup_enabled() {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            startup_flags,
            IDM_START_WITH_WINDOWS as usize,
            PCWSTR::from_raw(startup_str.as_ptr()),
        );

        let pace_colors_str = native_interop::wide_str(strings.pace_colors);
        let pace_colors_flags = if pace_colors_enabled {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            pace_colors_flags,
            IDM_PACE_COLORS as usize,
            PCWSTR::from_raw(pace_colors_str.as_ptr()),
        );

        let scoped_row_label = match scoped_label.as_str() {
            "" => strings.scoped_weekly_row.to_string(),
            label => format!("{} ({})", strings.scoped_weekly_row, label),
        };
        let scoped_row_str = native_interop::wide_str(&scoped_row_label);
        let scoped_row_flags = if show_scoped_weekly {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            scoped_row_flags,
            IDM_SCOPED_WEEKLY_ROW as usize,
            PCWSTR::from_raw(scoped_row_str.as_ptr()),
        );

        let detailed_time_str = native_interop::wide_str(strings.detailed_time);
        let detailed_time_flags = if detailed_time() {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            detailed_time_flags,
            IDM_DETAILED_TIME as usize,
            PCWSTR::from_raw(detailed_time_str.as_ptr()),
        );

        let auto_updates_str = native_interop::wide_str(strings.auto_install_updates);
        let auto_updates_flags = if auto_install_updates {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            auto_updates_flags,
            IDM_AUTO_INSTALL_UPDATES as usize,
            PCWSTR::from_raw(auto_updates_str.as_ptr()),
        );

        let reset_pos_str = native_interop::wide_str(strings.reset_position);
        let _ = AppendMenuW(
            settings_menu,
            MENU_ITEM_FLAGS(0),
            IDM_RESET_POSITION as usize,
            PCWSTR::from_raw(reset_pos_str.as_ptr()),
        );

        let language_menu = CreatePopupMenu().unwrap();
        let system_label = native_interop::wide_str(strings.system_default);
        let system_flags = if language_override.is_none() {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            language_menu,
            system_flags,
            IDM_LANG_SYSTEM as usize,
            PCWSTR::from_raw(system_label.as_ptr()),
        );

        for language in LanguageId::ALL {
            let id = match language {
                LanguageId::English => IDM_LANG_ENGLISH,
                LanguageId::Dutch => IDM_LANG_DUTCH,
                LanguageId::Spanish => IDM_LANG_SPANISH,
                LanguageId::French => IDM_LANG_FRENCH,
                LanguageId::German => IDM_LANG_GERMAN,
                LanguageId::Japanese => IDM_LANG_JAPANESE,
                LanguageId::Korean => IDM_LANG_KOREAN,
                LanguageId::TraditionalChinese => IDM_LANG_TRADITIONAL_CHINESE,
                LanguageId::SimplifiedChinese => IDM_LANG_SIMPLIFIED_CHINESE,
                LanguageId::Russian => IDM_LANG_RUSSIAN,
                LanguageId::PortugueseBrazil => IDM_LANG_PORTUGUESE_BRAZIL,
            };
            let label_str = native_interop::wide_str(language.native_name());
            let flags = if language_override == Some(language) {
                MF_CHECKED
            } else {
                MENU_ITEM_FLAGS(0)
            };
            let _ = AppendMenuW(
                language_menu,
                flags,
                id as usize,
                PCWSTR::from_raw(label_str.as_ptr()),
            );
        }

        let language_label = native_interop::wide_str(strings.language);
        let _ = AppendMenuW(
            settings_menu,
            MF_POPUP,
            language_menu.0 as usize,
            PCWSTR::from_raw(language_label.as_ptr()),
        );

        let _ = AppendMenuW(settings_menu, MF_SEPARATOR, 0, PCWSTR::null());

        let version_label =
            version_action_label(strings, language, install_channel, &update_status);
        let version_str = native_interop::wide_str(&version_label);
        let version_flags = if matches!(
            update_status,
            UpdateStatus::Checking | UpdateStatus::Applying
        ) {
            MF_GRAYED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            settings_menu,
            version_flags,
            IDM_VERSION_ACTION as usize,
            PCWSTR::from_raw(version_str.as_ptr()),
        );

        let settings_label = native_interop::wide_str(strings.settings);
        let _ = AppendMenuW(
            menu,
            MF_POPUP,
            settings_menu.0 as usize,
            PCWSTR::from_raw(settings_label.as_ptr()),
        );

        let widget_label = native_interop::wide_str(strings.show_widget);
        let widget_flags = if widget_visible {
            MF_CHECKED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        let _ = AppendMenuW(
            menu,
            widget_flags,
            tray_icon::IDM_TOGGLE_WIDGET as usize,
            PCWSTR::from_raw(widget_label.as_ptr()),
        );

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        let exit_str = native_interop::wide_str(strings.exit);
        let _ = AppendMenuW(
            menu,
            MENU_ITEM_FLAGS(0),
            2,
            PCWSTR::from_raw(exit_str.as_ptr()),
        );

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

/// Paint for the non-embedded fallback (normal WM_PAINT path): the same plate,
/// drawn over an opaque taskbar-coloured ground, since this window has no
/// per-pixel alpha to composite with.
fn paint(hdc: HDC, hwnd: HWND) {
    let (model, ground) = {
        let state = lock_state();
        match state.as_ref() {
            Some(s) => (
                widget_model(s),
                if s.is_dark { 0xFF1C_1C1C_u32 } else { 0xFFF3_F3F3_u32 },
            ),
            None => return,
        }
    };

    unsafe {
        let mut client_rect = RECT::default();
        let _ = GetClientRect(hwnd, &mut client_rect);
        let width = client_rect.right - client_rect.left;
        let height = client_rect.bottom - client_rect.top;
        if width <= 0 || height <= 0 {
            return;
        }

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let mem_dc = CreateCompatibleDC(hdc);
        let Ok(dib) = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem_dc);
            return;
        };
        let old_bmp = SelectObject(mem_dc, dib);
        std::slice::from_raw_parts_mut(bits as *mut u32, (width * height) as usize).fill(ground);

        if let Some(painter) = cockpit::Painter::new(bits, width, height) {
            let k = scale();
            let layout = cockpit::widget_layout(&painter, &model, k);
            cockpit::paint_widget(&painter, &model, &layout, k, height);
        }
        let _ = BitBlt(hdc, 0, 0, width, height, mem_dc, 0, 0, SRCCOPY);

        SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Foundation::RECT;

    #[test]
    fn every_provider_has_a_unique_menu_id() {
        let mut ids: Vec<u16> = providers::PROVIDERS
            .into_iter()
            .map(|provider| provider.menu_id())
            .collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(count, ids.len(), "menu ids must not collide");
    }

    #[test]
    fn a_menu_id_resolves_back_to_its_provider() {
        for provider in providers::PROVIDERS {
            assert_eq!(providers::from_menu_id(provider.menu_id()), Some(provider));
        }
        assert_eq!(providers::from_menu_id(9999), None);
    }

    #[test]
    fn an_old_settings_file_keeps_the_provider_it_enabled() {
        // Written by a version that only ever knew about Codex.
        let mut settings: SettingsFile =
            serde_json::from_str(r#"{"show_codex": true, "show_claude_code": false}"#)
                .expect("an existing settings file should still parse");

        keep_one_provider_visible(&mut settings);

        assert!(settings.show_codex, "the stored choice must survive");
        assert!(!settings.show_claude_code);
        assert!(!settings.show_antigravity);
    }

    #[test]
    fn a_settings_file_with_every_provider_off_falls_back_to_one() {
        let mut settings: SettingsFile = serde_json::from_str(
            r#"{"show_claude_code": false, "show_codex": false, "show_antigravity": false}"#,
        )
        .expect("the file should parse");

        keep_one_provider_visible(&mut settings);

        assert!(settings.show_claude_code);
    }

    #[test]
    fn an_unknown_settings_key_does_not_break_parsing() {
        let settings: SettingsFile = serde_json::from_str(
            r#"{"show_codex": true, "somethingFromTheFuture": {"a": 1}}"#,
        )
        .expect("unknown keys must not stop the file from being read");

        assert!(settings.show_codex);
    }

    #[test]
    fn only_the_hottest_red_value_is_reversed() {
        assert_eq!(hottest(&[Some(120.0), None, Some(266.0)]), Some(2));
        assert_eq!(hottest(&[None, None]), None, "no red, no reversal");
        assert_eq!(hottest(&[]), None);
    }

    #[test]
    fn a_rollover_needs_the_reset_to_move_on_and_the_use_to_drop() {
        let at = |secs: u64, percentage: f64| UsageSection {
            percentage,
            resets_at: Some(UNIX_EPOCH + Duration::from_secs(secs)),
        };
        assert!(rolled_over(&at(18_000, 80.0), &at(36_000, 2.0)));
        assert!(!rolled_over(&at(18_000, 80.0), &at(18_030, 2.0)), "reset jitter is not a rollover");
        assert!(!rolled_over(&at(18_000, 80.0), &at(36_000, 85.0)));
    }

    fn taskbar(device: &str, left: i32) -> native_interop::TaskbarWindow {
        screen(device, left, false)
    }

    fn screen(device: &str, left: i32, is_primary: bool) -> native_interop::TaskbarWindow {
        native_interop::TaskbarWindow {
            hwnd: HWND::default(),
            rect: RECT {
                left,
                top: 1032,
                right: left + 1920,
                bottom: 1080,
            },
            device: Some(device.to_string()),
            is_primary,
        }
    }

    #[test]
    fn the_remembered_monitor_wins_over_a_stale_index() {
        let taskbars = vec![taskbar(r"\.\DISPLAY2", 0), taskbar(r"\.\DISPLAY1", 1920)];
        let (index, matched) = select_taskbar(&taskbars, 0, Some(r"\.\DISPLAY1"), false);
        assert_eq!(index, 1);
        assert!(matched);
    }

    /// The reported bug: the shell rebuilt the chosen screen's taskbar, so the
    /// enumeration briefly held only the other screen and index 0 moved the
    /// widget there. The pick must be flagged as a fallback so it cannot be
    /// persisted over the real preference.
    #[test]
    fn an_incomplete_enumeration_is_reported_as_a_fallback() {
        let only_the_other_screen = vec![taskbar(r"\.\DISPLAY2", 1920)];
        let (index, matched) = select_taskbar(&only_the_other_screen, 0, Some(r"\.\DISPLAY1"), false);
        assert_eq!(index, 0);
        assert!(
            !matched,
            "picking another monitor must not count as matching the remembered one"
        );
    }

    #[test]
    fn an_explicit_choice_uses_the_index_and_counts_as_deliberate() {
        let taskbars = vec![taskbar(r"\.\DISPLAY1", 0), taskbar(r"\.\DISPLAY2", 1920)];
        let (index, matched) = select_taskbar(&taskbars, 1, None, false);
        assert_eq!(index, 1);
        assert!(!matched);
    }

    /// What the user asked for: the widget stays on the primary screen whatever
    /// the enumeration order, the stale index or the remembered device say.
    #[test]
    fn pinning_beats_a_stale_index_and_a_stale_device() {
        let taskbars = vec![
            screen(r"\.\DISPLAY2", 0, false),
            screen(r"\.\DISPLAY1", 1920, true),
        ];
        let (index, matched) = select_taskbar(&taskbars, 0, Some(r"\.\DISPLAY2"), true);
        assert_eq!(index, 1, "must land on the primary monitor");
        assert!(matched);
    }

    #[test]
    fn pinning_falls_back_when_no_primary_is_reported() {
        let taskbars = vec![screen(r"\.\DISPLAY2", 0, false)];
        let (index, matched) = select_taskbar(&taskbars, 0, None, true);
        assert_eq!(index, 0);
        assert!(!matched, "a fallback pick must not be recorded as the preference");
    }

    /// The reported scenario: locking the session rebuilds the taskbars and can
    /// leave the pinned widget on the secondary screen.
    #[test]
    fn a_pinned_widget_stranded_on_the_secondary_screen_moves_back() {
        let secondary = screen("SECOND", 1920, false);
        let taskbars = vec![screen("MAIN", 0, true), secondary.clone()];
        assert!(should_move_back_to_primary(&secondary, &taskbars, true));
    }

    /// Mid-transition Windows may report no primary at all. Moving then would
    /// pick an arbitrary screen, so nothing should happen.
    #[test]
    fn no_primary_reported_means_stay_put() {
        let secondary = screen("SECOND", 1920, false);
        let taskbars = vec![secondary.clone()];
        assert!(!should_move_back_to_primary(&secondary, &taskbars, true));
    }

    #[test]
    fn already_on_the_primary_screen_is_left_alone() {
        let main = screen("MAIN", 0, true);
        let taskbars = vec![main.clone(), screen("SECOND", 1920, false)];
        assert!(!should_move_back_to_primary(&main, &taskbars, true));
    }

    #[test]
    fn an_unpinned_widget_is_never_moved_back() {
        let secondary = screen("SECOND", 1920, false);
        let taskbars = vec![screen("MAIN", 0, true), secondary.clone()];
        assert!(
            !should_move_back_to_primary(&secondary, &taskbars, false),
            "a deliberate drag to another screen must be respected"
        );
    }

    /// Observed live: a relaunch inside a session lock found only the secondary
    /// taskbar and attached there in plain sight.
    #[test]
    fn startup_waits_while_the_primary_taskbar_is_missing() {
        let only_secondary = vec![screen("SECOND", 1920, false)];
        assert!(should_wait_for_primary(&only_secondary, true));
    }

    #[test]
    fn startup_does_not_wait_once_the_primary_is_there() {
        let both = vec![screen("MAIN", 0, true), screen("SECOND", 1920, false)];
        assert!(!should_wait_for_primary(&both, true));
    }

    #[test]
    fn startup_never_waits_for_an_unpinned_widget() {
        let only_secondary = vec![screen("SECOND", 1920, false)];
        assert!(
            !should_wait_for_primary(&only_secondary, false),
            "a widget the user put on another screen has nothing to wait for"
        );
    }

    #[test]
    fn an_out_of_range_index_is_clamped() {
        let taskbars = vec![taskbar(r"\.\DISPLAY1", 0)];
        assert_eq!(select_taskbar(&taskbars, 5, None, false), (0, false));
    }
}
