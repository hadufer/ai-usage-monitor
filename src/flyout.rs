//! The panel a click on the widget opens: every window as a vertical tape, its
//! exact reset, and a projection when the limit would land before it. It
//! replaces the hover tooltip, and closes the way a shell flyout does: Escape,
//! a click elsewhere, or a second click on the widget.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWINDOWATTRIBUTE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::cockpit;
use crate::native_interop::{self, WM_APP, WM_MOUSELEAVE};

/// Posted to itself on deactivation: hiding from inside WM_ACTIVATE would
/// re-enter the window procedure through ShowWindow.
const WM_APP_FLYOUT_HIDE: u32 = WM_APP + 10;
/// The click on the widget that closed the flyout (by taking the focus away)
/// must not open it again on its way up.
const REOPEN_GUARD: Duration = Duration::from_millis(300);
const GAP: i32 = 12;

// Windows 11 only; older systems ignore both and keep square corners.
const DWMWA_WINDOW_CORNER_PREFERENCE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(33);
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const DWMWCP_ROUND: u32 = 2;

struct Flyout {
    hwnd: isize,
    owner: isize,
    hidden_at: Option<Instant>,
    hover: Option<usize>,
    pressed: Option<usize>,
    /// Keyboard focus among the buttons, drawn as a ring. `None` until a key
    /// is used, as Windows only shows focus to keyboard users.
    focus: Option<usize>,
    buttons: [RECT; 2],
    tracking: bool,
}

const NO_RECT: RECT = RECT {
    left: 0,
    top: 0,
    right: 0,
    bottom: 0,
};

static FLYOUT: Mutex<Flyout> = Mutex::new(Flyout {
    hwnd: 0,
    owner: 0,
    hidden_at: None,
    hover: None,
    pressed: None,
    focus: None,
    buttons: [NO_RECT; 2],
    tracking: false,
});

fn lock() -> std::sync::MutexGuard<'static, Flyout> {
    FLYOUT.lock().unwrap_or_else(|e| e.into_inner())
}

fn handle() -> Option<HWND> {
    let hwnd = lock().hwnd;
    (hwnd != 0).then(|| HWND(hwnd as *mut _))
}

pub fn is_visible() -> bool {
    handle().is_some_and(|hwnd| unsafe { IsWindowVisible(hwnd).as_bool() })
}

/// What a click on the widget does.
pub fn toggle(owner: HWND) {
    let recently_hidden = lock()
        .hidden_at
        .is_some_and(|at| at.elapsed() < REOPEN_GUARD);
    if is_visible() {
        hide();
    } else if !recently_hidden {
        show(owner);
    }
}

pub fn hide() {
    let Some(hwnd) = handle() else { return };
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return;
        }
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
    {
        let mut flyout = lock();
        flyout.hidden_at = Some(Instant::now());
        flyout.hover = None;
        flyout.pressed = None;
        flyout.focus = None;
    }
    // The widget draws itself lit while its flyout is open.
    crate::window::render_layered();
}

/// New data, a new minute or a new theme: resize and repaint if open.
pub fn refresh() {
    if !is_visible() {
        return;
    }
    let (hwnd, owner) = {
        let flyout = lock();
        (HWND(flyout.hwnd as *mut _), HWND(flyout.owner as *mut _))
    };
    place(hwnd, owner);
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn show(owner: HWND) {
    let Some(hwnd) = handle().or_else(create) else {
        return;
    };
    lock().owner = owner.0 as isize;
    place(hwnd, owner);
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    crate::window::render_layered();
}

fn create() -> Option<HWND> {
    let class_name = native_interop::wide_str("ClaudeCodeUsageMonitorFlyout");
    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null()).ok()?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: HINSTANCE(hinstance.0),
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&class);

        // Unowned, like the tooltip was: the widget is a child of another
        // process's taskbar, and a popup cannot be owned by a child window.
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            PCWSTR::from_raw(class_name.as_ptr()),
            PCWSTR::from_raw(class_name.as_ptr()),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            hinstance,
            None,
        )
        .ok()?;
        let corner = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const u32 as *const _,
            4,
        );
        lock().hwnd = hwnd.0 as isize;
        Some(hwnd)
    }
}

/// Sizes the window to its content and puts it above the widget, inside the
/// monitor's work area.
fn place(hwnd: HWND, owner: HWND) {
    let Some(model) = crate::window::flyout_model() else {
        return;
    };
    let k = crate::window::scale();
    let Some(measure) = cockpit::Painter::measuring() else {
        return;
    };
    let layout = cockpit::flyout_layout(&measure, &model, k);
    drop(measure);
    lock().buttons = layout.buttons;

    unsafe {
        // What Narrator announces for the window.
        let title = native_interop::wide_str(&model.title);
        let _ = SetWindowTextW(hwnd, PCWSTR::from_raw(title.as_ptr()));
        let border = model.palette.bezel.to_colorref();
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &border as *const u32 as *const _, 4);

        let mut widget = RECT::default();
        let _ = GetWindowRect(owner, &mut widget);
        let monitor = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let work = info.rcWork;
        let gap = (GAP as f32 * k).round() as i32;
        let (width, height) = (layout.width, layout.height);

        let centre = (widget.left + widget.right) / 2;
        let x = (centre - width / 2).clamp(work.left + gap, (work.right - width - gap).max(work.left + gap));
        let y = if widget.top >= work.bottom - 1 {
            work.bottom - height - gap
        } else if widget.bottom <= work.top + 1 {
            work.top + gap
        } else {
            (widget.top - height - gap).max(work.top + gap)
        };
        let _ = SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE);
    }
}

fn button_at(lparam: LPARAM) -> Option<usize> {
    let x = (lparam.0 & 0xFFFF) as i16 as i32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
    lock()
        .buttons
        .iter()
        .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom)
}

fn press(index: usize) {
    let owner = HWND(lock().owner as *mut _);
    match index {
        // The widget's own Refresh command; the panel stays open and updates
        // when the answer lands.
        0 => unsafe {
            let _ = PostMessageW(owner, WM_COMMAND, WPARAM(1), LPARAM(0));
        },
        _ => {
            hide();
            crate::window::show_context_menu(owner);
        }
    }
}

fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let (width, height) = (client.right, client.bottom);
        let (hover, pressed, focus) = {
            let flyout = lock();
            (flyout.hover, flyout.pressed, flyout.focus)
        };

        if let (Some(model), true) = (crate::window::flyout_model(), width > 0 && height > 0) {
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
            let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
            let mem_dc = CreateCompatibleDC(hdc);
            if let Ok(dib) = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                let previous = SelectObject(mem_dc, dib);
                if let Some(painter) = cockpit::Painter::new(bits, width, height) {
                    let k = crate::window::scale();
                    let layout = cockpit::flyout_layout(&painter, &model, k);
                    cockpit::paint_flyout(&painter, &model, &layout, k, hover, pressed, focus);
                }
                let _ = BitBlt(hdc, 0, 0, width, height, mem_dc, 0, 0, SRCCOPY);
                SelectObject(mem_dc, previous);
                let _ = DeleteObject(dib);
            }
            let _ = DeleteDC(mem_dc);
        }
        let _ = EndPaint(hwnd, &ps);
    }
}

fn set_hover(hwnd: HWND, hover: Option<usize>) {
    let changed = {
        let mut flyout = lock();
        let changed = flyout.hover != hover;
        flyout.hover = hover;
        changed
    };
    if changed {
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint(hwnd);
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                let _ = PostMessageW(hwnd, WM_APP_FLYOUT_HIDE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_APP_FLYOUT_HIDE => {
            hide();
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let key = wparam.0 as u16;
            if key == VK_ESCAPE.0 {
                hide();
            } else if key == VK_RETURN.0 || key == VK_SPACE.0 {
                let focus = lock().focus;
                if let Some(index) = focus {
                    press(index);
                }
            } else if [VK_TAB.0, VK_LEFT.0, VK_RIGHT.0, VK_UP.0, VK_DOWN.0].contains(&key) {
                // Two buttons: every direction key moves to the other one,
                // and the first key press lands on Refresh.
                let backwards = key == VK_LEFT.0
                    || key == VK_UP.0
                    || (key == VK_TAB.0 && GetKeyState(VK_SHIFT.0 as i32) < 0);
                {
                    let mut flyout = lock();
                    flyout.focus = Some(match flyout.focus {
                        None if backwards => 1,
                        None => 0,
                        Some(index) => (index + 1) % 2,
                    });
                }
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let start_tracking = {
                let mut flyout = lock();
                !std::mem::replace(&mut flyout.tracking, true)
            };
            if start_tracking {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
            }
            set_hover(hwnd, button_at(lparam));
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            lock().tracking = false;
            set_hover(hwnd, None);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let hit = button_at(lparam);
            lock().pressed = hit;
            if hit.is_some() {
                SetCapture(hwnd);
            }
            let _ = InvalidateRect(hwnd, None, false);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let hit = button_at(lparam);
            let pressed = lock().pressed.take();
            let _ = ReleaseCapture();
            let _ = InvalidateRect(hwnd, None, false);
            if let Some(index) = pressed.filter(|index| hit == Some(*index)) {
                press(index);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
