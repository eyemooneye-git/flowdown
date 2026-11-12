//! Notification-area icon and its context menu.

use windows_sys::Win32::Foundation::{HWND, POINT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, NOTIFYICONDATAW_0,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, GetSystemMetrics,
    LoadImageW, PostMessageW, SetForegroundWindow, TrackPopupMenu, HICON, IMAGE_ICON,
    LR_DEFAULTCOLOR, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, SM_CXSMICON, SM_CYSMICON,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_NULL,
};

use crate::tr;
use crate::util::{wide, APP_ICON_ID};

pub const WM_TRAYICON: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 10;

pub const IDM_SHOW: u32 = 4001;
pub const IDM_PAUSE_ALL: u32 = 4002;
pub const IDM_RESUME_ALL: u32 = 4003;
pub const IDM_OPEN_FOLDER: u32 = 4004;
pub const IDM_MONITOR_CLIPBOARD: u32 = 4005;
pub const IDM_AUTOSTART: u32 = 4006;
pub const IDM_SETTINGS: u32 = 4007;
pub const IDM_EXIT: u32 = 4008;

const TRAY_UID: u32 = 1;

pub struct MenuState {
    pub monitor_clipboard: bool,
    pub autostart: bool,
    pub has_tasks: bool,
    pub has_active: bool,
}

pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    added: bool,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Self {
        let icon = unsafe {
            LoadImageW(
                GetModuleHandleW(std::ptr::null()),
                APP_ICON_ID,
                IMAGE_ICON,
                GetSystemMetrics(SM_CXSMICON),
                GetSystemMetrics(SM_CYSMICON),
                LR_DEFAULTCOLOR,
            )
        };
        Self {
            hwnd,
            icon,
            added: false,
        }
    }

    fn base(&self, flags: u32) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_UID,
            uFlags: flags,
            uCallbackMessage: WM_TRAYICON,
            ..Default::default()
        }
    }

    pub fn add(&mut self, tip: &str) {
        if self.added {
            self.update_tip(tip);
            return;
        }
        let mut data = self.base(NIF_MESSAGE | NIF_ICON | NIF_TIP);
        data.hIcon = self.icon;
        copy_into(&mut data.szTip, tip);
        if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0 {
            self.added = true;
        }
    }

    pub fn remove(&mut self) {
        if !self.added {
            return;
        }
        let data = self.base(0);
        let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
        self.added = false;
    }

    pub fn update_tip(&mut self, tip: &str) {
        if !self.added {
            return;
        }
        let mut data = self.base(NIF_TIP);
        copy_into(&mut data.szTip, tip);
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }

    /// A balloon tip. Notifications are best-effort: Focus Assist and the
    /// "quiet hours" setting legitimately suppress them.
    pub fn notify(&mut self, title: &str, body: &str) {
        if !self.added {
            return;
        }
        let mut data = self.base(NIF_INFO);
        copy_into(&mut data.szInfoTitle, title);
        copy_into(&mut data.szInfo, body);
        data.dwInfoFlags = NIIF_INFO;
        data.Anonymous = NOTIFYICONDATAW_0 { uTimeout: 6000 };
        let _ = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) };
    }

    pub fn show_menu(hwnd: HWND, state: &MenuState) -> u32 {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return 0;
            }
            let show = wide(tr!("Open FlowDown", "打开 FlowDown"));
            let pause = wide(tr!("Pause all", "全部暂停"));
            let resume = wide(tr!("Resume all", "全部继续"));
            let folder = wide(tr!("Open downloads folder", "打开下载文件夹"));
            let monitor = wide(tr!("Monitor clipboard", "监视剪贴板"));
            let autostart = wide(tr!("Start with Windows", "开机启动"));
            let settings = wide(tr!("Settings…", "设置…"));
            let exit = wide(tr!("Exit", "退出"));

            AppendMenuW(menu, MF_STRING, IDM_SHOW as usize, show.as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            AppendMenuW(
                menu,
                MF_STRING | if state.has_active { 0 } else { MF_GRAYED },
                IDM_PAUSE_ALL as usize,
                pause.as_ptr(),
            );
            AppendMenuW(
                menu,
                MF_STRING | if state.has_tasks { 0 } else { MF_GRAYED },
                IDM_RESUME_ALL as usize,
                resume.as_ptr(),
            );
            AppendMenuW(menu, MF_STRING, IDM_OPEN_FOLDER as usize, folder.as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            AppendMenuW(
                menu,
                MF_STRING
                    | if state.monitor_clipboard {
                        MF_CHECKED
                    } else {
                        0
                    },
                IDM_MONITOR_CLIPBOARD as usize,
                monitor.as_ptr(),
            );
            AppendMenuW(
                menu,
                MF_STRING | if state.autostart { MF_CHECKED } else { 0 },
                IDM_AUTOSTART as usize,
                autostart.as_ptr(),
            );
            AppendMenuW(menu, MF_STRING, IDM_SETTINGS as usize, settings.as_ptr());
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
            AppendMenuW(menu, MF_STRING, IDM_EXIT as usize, exit.as_ptr());

            let mut point = POINT::default();
            GetCursorPos(&mut point);
            SetForegroundWindow(hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                0,
                hwnd,
                std::ptr::null(),
            );
            DestroyMenu(menu);
            // Without this the menu can stay visible after the click.
            PostMessageW(hwnd, WM_NULL, 0, 0);
            command as u32
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        if !self.icon.is_null() {
            let _ = unsafe { DestroyIcon(self.icon) };
            self.icon = std::ptr::null_mut();
        }
    }
}

fn copy_into(destination: &mut [u16], text: &str) {
    if destination.is_empty() {
        return;
    }
    let limit = destination.len() - 1;
    let mut written = 0;
    for unit in text.encode_utf16().take(limit) {
        destination[written] = unit;
        written += 1;
    }
    destination[written] = 0;
}
