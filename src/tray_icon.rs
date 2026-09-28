use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::UI::Shell::{
    ExtractIconExW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING,
    NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::cockpit;
use crate::native_interop::WM_APP_TRAY;

const CLAUDE_TRAY_ICON_ID: u32 = 1;
const CODEX_TRAY_ICON_ID: u32 = 2;
const ANTIGRAVITY_TRAY_ICON_ID: u32 = 3;

/// Menu item ID for toggling widget visibility (used by window.rs context menu).
pub const IDM_TOGGLE_WIDGET: u16 = 70;

/// Actions the tray message handler can request from the main window.
pub enum TrayAction {
    None,
    ToggleWidget,
    ShowContextMenu,
}

#[derive(Clone, Copy)]
pub enum TrayIconKind {
    Claude,
    Codex,
    Antigravity,
}

pub struct TrayIconData {
    pub kind: TrayIconKind,
    /// The 5-hour readout; `None` before the first answer, or when the
    /// provider could not be read.
    pub badge: Option<cockpit::Badge>,
    pub palette: cockpit::Palette,
    pub tooltip: String,
}

impl TrayIconKind {
    fn id(self) -> u32 {
        match self {
            Self::Claude => CLAUDE_TRAY_ICON_ID,
            Self::Codex => CODEX_TRAY_ICON_ID,
            Self::Antigravity => ANTIGRAVITY_TRAY_ICON_ID,
        }
    }

    /// What the badge says while there is no number yet: the provider's code,
    /// the same one its widget row carries.
    fn code(self) -> &'static str {
        match self {
            Self::Claude => "CL",
            Self::Codex => "CX",
            Self::Antigravity => "AG",
        }
    }
}

/// The widget's boxed readout at icon size. Claude keeps the app icon as its
/// loading state, so the tray still says which app this is before any data.
pub fn create_icon(icon: &TrayIconData) -> HICON {
    let placeholder;
    let badge = match &icon.badge {
        Some(badge) => badge,
        None => {
            if matches!(icon.kind, TrayIconKind::Claude) {
                let app_icon = load_embedded_app_icon();
                if !app_icon.is_invalid() {
                    return app_icon;
                }
            }
            placeholder = cockpit::Badge {
                text: icon.kind.code().to_string(),
                band: None,
                hot: false,
            };
            &placeholder
        }
    };

    // Drawn at the size the notification area shows, so the outline lands on
    // whole pixels instead of being blurred by the shell's downscale.
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    unsafe {
        let screen_dc = GetDC(HWND::default());
        let mem_dc = CreateCompatibleDC(screen_dc);

        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib =
            CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).unwrap_or_default();

        if dib.is_invalid() || bits.is_null() {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(HWND::default(), screen_dc);
            return HICON::default();
        }

        std::slice::from_raw_parts_mut(bits as *mut u32, (size * size) as usize).fill(0);
        cockpit::paint_badge(bits, size, badge, &icon.palette);

        // Monochrome mask (per-pixel alpha from colour bitmap)
        let mask_bytes = vec![0u8; ((size * size + 7) / 8) as usize];
        let mask_bmp = CreateBitmap(
            size,
            size,
            1,
            1,
            Some(mask_bytes.as_ptr() as *const std::ffi::c_void),
        );

        let icon_info = ICONINFO {
            fIcon: TRUE,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask_bmp,
            hbmColor: dib,
        };
        let hicon = CreateIconIndirect(&icon_info).unwrap_or_default();

        let _ = DeleteObject(mask_bmp);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(HWND::default(), screen_dc);

        hicon
    }
}
fn load_embedded_app_icon() -> HICON {
    unsafe {
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len == 0 {
            return HICON::default();
        }

        let mut small_icon = HICON::default();
        let mut large_icon = HICON::default();
        let extracted = ExtractIconExW(
            PCWSTR::from_raw(exe_buf.as_ptr()),
            0,
            Some(&mut large_icon),
            Some(&mut small_icon),
            1,
        );

        if extracted == 0 {
            HICON::default()
        } else if !small_icon.is_invalid() {
            small_icon
        } else {
            large_icon
        }
    }
}

/// Show a Windows balloon notification from the tray icon.
/// Used to alert the user when re-authentication is required.
pub fn notify_balloon(hwnd: HWND, kind: TrayIconKind, title: &str, message: &str) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = kind.id();
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_WARNING;
        copy_wide(title, &mut nid.szInfoTitle);
        copy_wide_256(message, &mut nid.szInfo);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

/// Copy a string into a fixed-size wide buffer (truncates to fit).
fn copy_wide<const N: usize>(s: &str, buf: &mut [u16; N]) {
    let wide: Vec<u16> = s.encode_utf16().collect();
    let len = wide.len().min(N - 1);
    buf[..len].copy_from_slice(&wide[..len]);
    buf[len] = 0;
}

/// Copy a string into a 256-wide buffer.
fn copy_wide_256(s: &str, buf: &mut [u16; 256]) {
    copy_wide(s, buf)
}

/// Register the tray icon with the shell.
pub fn add(hwnd: HWND, icon: &TrayIconData) {
    let hicon = create_icon(icon);
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = icon.kind.id();
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        nid.hIcon = hicon;
        copy_to_tip(&icon.tooltip, &mut nid.szTip);
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
        if !hicon.is_invalid() {
            let _ = DestroyIcon(hicon);
        }
    }
}

/// Update the tray icon and its tooltip to reflect current usage.
pub fn update(hwnd: HWND, icon: &TrayIconData) {
    let hicon = create_icon(icon);
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = icon.kind.id();
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = hicon;
        copy_to_tip(&icon.tooltip, &mut nid.szTip);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        if !hicon.is_invalid() {
            let _ = DestroyIcon(hicon);
        }
    }
}

/// Remove the tray icon from the shell.
pub fn remove(hwnd: HWND, kind: TrayIconKind) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = kind.id();
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

pub fn sync(hwnd: HWND, icons: &[TrayIconData]) {
    for kind in [TrayIconKind::Claude, TrayIconKind::Codex, TrayIconKind::Antigravity] {
        match icons.iter().find(|icon| icon.kind.id() == kind.id()) {
            Some(icon) => {
                add(hwnd, icon);
                update(hwnd, icon);
            }
            None => remove(hwnd, kind),
        }
    }
}
pub fn remove_all(hwnd: HWND) {
    remove(hwnd, TrayIconKind::Claude);
    remove(hwnd, TrayIconKind::Codex);
    remove(hwnd, TrayIconKind::Antigravity);
}

/// Interpret a tray callback message and return the action to take.
pub fn handle_message(lparam: LPARAM) -> TrayAction {
    let mouse_msg = lparam.0 as u32;
    match mouse_msg {
        WM_LBUTTONUP => TrayAction::ToggleWidget,
        WM_RBUTTONUP => TrayAction::ShowContextMenu,
        _ => TrayAction::None,
    }
}

/// Copy a string into the fixed-size szTip field (max 127 chars + null).
fn copy_to_tip(s: &str, tip: &mut [u16; 128]) {
    let wide: Vec<u16> = s.encode_utf16().collect();
    let mut len = wide.len().min(127);
    // Don't leave a lone high surrogate at the truncation point
    if len > 0 && (0xD800..=0xDBFF).contains(&wide[len - 1]) {
        len -= 1;
    }
    tip[..len].copy_from_slice(&wide[..len]);
    tip[len] = 0;
}
