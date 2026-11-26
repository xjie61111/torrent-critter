//! Windows desktop integration: tray icon, hiding/showing the main window and
//! a single running instance.
//!
//! The window is shown and hidden with Win32 calls on its HWND rather than
//! egui viewport commands: while a window is hidden eframe does not run frames,
//! so a viewport command sent from the tray would never be applied.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicIsize, Ordering},
        Arc,
    },
    time::Duration,
};

use eframe::egui;
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

use crate::i18n::{app_name, t};

static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
/// Set when the user chose "Exit" from the tray, so closing is not turned into
/// hiding by the close-to-tray setting.
pub static QUIT_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn set_main_window(cc: &eframe::CreationContext<'_>) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    if let Ok(handle) = cc.window_handle() {
        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
            MAIN_HWND.store(win32.hwnd.get(), Ordering::Relaxed);
        }
    }
}

fn hwnd() -> Option<windows_sys::Win32::Foundation::HWND> {
    match MAIN_HWND.load(Ordering::Relaxed) {
        0 => None,
        raw => Some(raw as _),
    }
}

pub fn hide_window() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};
    if let Some(hwnd) = hwnd() { unsafe { ShowWindow(hwnd, SW_HIDE) }; }
}

pub fn show_window(ctx: &egui::Context) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW};
    if let Some(hwnd) = hwnd() {
        unsafe {
            ShowWindow(hwnd, if IsIconic(hwnd) != 0 { SW_RESTORE } else { SW_SHOW });
            SetForegroundWindow(hwnd);
        }
    }
    ctx.request_repaint();
}

/// Visible and not minimized: clicking the tray icon then hides the window,
/// otherwise it brings it back.
pub fn is_window_visible() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible};
    hwnd().is_some_and(|hwnd| unsafe { IsWindowVisible(hwnd) != 0 && IsIconic(hwnd) == 0 })
}

pub struct Tray {
    _icon: TrayIcon,
    show_item: MenuItem,
    exit_item: MenuItem,
}

impl Tray {
    pub fn new(ctx: &egui::Context, rgba: Vec<u8>, size: u32) -> Option<Self> {
        let show_item = MenuItem::new(t("Показать", "Show"), true, None);
        let exit_item = MenuItem::new(t("Выход", "Exit"), true, None);
        let menu = Menu::new();
        menu.append_items(&[&show_item, &PredefinedMenuItem::separator(), &exit_item]).ok()?;
        let icon = tray_icon::Icon::from_rgba(rgba, size, size).ok()?;
        let tray = TrayIconBuilder::new()
            .with_icon(icon)
            .with_tooltip(app_name())
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
            .ok()?;

        let click_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let toggle = matches!(event,
                TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. }
                | TrayIconEvent::DoubleClick { button: MouseButton::Left, .. });
            if toggle {
                if is_window_visible() { hide_window(); } else { show_window(&click_ctx); }
            }
        }));
        let menu_ctx = ctx.clone();
        let (show_id, exit_id) = (show_item.id().clone(), exit_item.id().clone());
        MenuEvent::set_event_handler(Some(Arc::new(move |event: MenuEvent| {
            if event.id == show_id {
                show_window(&menu_ctx);
            } else if event.id == exit_id {
                QUIT_REQUESTED.store(true, Ordering::Relaxed);
                // Frames only run while the window is visible; the next frame
                // then closes the app normally (and runs on_exit).
                show_window(&menu_ctx);
                menu_ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        })));
        Some(Self { _icon: tray, show_item, exit_item })
    }

    /// Re-labels the menu after the UI language changed.
    pub fn refresh_language(&self) {
        self.show_item.set_text(t("Показать", "Show"));
        self.exit_item.set_text(t("Выход", "Exit"));
        let _ = self._icon.set_tooltip(Some(app_name()));
    }
}

// ---- single instance ------------------------------------------------------

fn inbox_dir() -> PathBuf {
    crate::app_data_dir().join("inbox")
}

/// Returns `true` if this is the first instance. Otherwise hands the command
/// line (a .torrent path or magnet link, if any) to the running instance via
/// the inbox folder and returns `false`.
pub fn claim_single_instance() -> bool {
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_ALREADY_EXISTS},
        System::Threading::CreateMutexW,
        UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY},
    };
    let name: Vec<u16> = "Local\\TorrentCritterSingleInstance\0".encode_utf16().collect();
    // The mutex handle is intentionally leaked: it must live as long as the process.
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() || unsafe { GetLastError() } != ERROR_ALREADY_EXISTS { return true; }

    let dir = inbox_dir();
    let _ = std::fs::create_dir_all(&dir);
    let argument = std::env::args_os().nth(1).map(|arg| {
        // Pass absolute paths: the running instance has a different working directory.
        let path = PathBuf::from(&arg);
        if path.is_relative() && path.exists() { std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_default().into_os_string() } else { arg }
    });
    let file = dir.join(format!("{}-{}.txt", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));
    let _ = std::fs::write(file, argument.map(|a| a.to_string_lossy().into_owned()).unwrap_or_default());
    // Let the running instance bring its window to the front.
    unsafe { AllowSetForegroundWindow(ASFW_ANY) };
    false
}

/// Watches the inbox in the background: a second launch shows the window even
/// while it is hidden in the tray (when no frames run to notice it).
pub fn watch_inbox(ctx: egui::Context) {
    let dir = inbox_dir();
    let _ = std::fs::create_dir_all(&dir);
    std::thread::Builder::new().name("instance-inbox".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_millis(400));
        if std::fs::read_dir(&dir).map(|mut entries| entries.next().is_some()).unwrap_or(false) {
            show_window(&ctx);
        }
    }).ok();
}

/// Arguments sent by later launches since the last call (empty strings mean
/// "just show the window").
pub fn take_inbox() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(inbox_dir()) else { return Vec::new() };
    let mut files: Vec<_> = entries.filter_map(Result::ok).map(|entry| entry.path()).collect();
    files.sort();
    files.into_iter().filter_map(|path| {
        let text = std::fs::read_to_string(&path).ok();
        let _ = std::fs::remove_file(&path);
        text
    }).collect()
}
