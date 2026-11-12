//! Native Win32 front-end: toolbar, virtual list view, status bar, settings
//! dialog, clipboard capture and the notification-area icon.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, FillRect, GetSysColorBrush, SetBkMode, UpdateWindow,
    COLOR_BTNFACE, FW_NORMAL, LOGFONTW, TRANSPARENT,
};
use windows_sys::Win32::System::DataExchange::{
    AddClipboardFormatListener, RemoveClipboardFormatListener, COPYDATASTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{
    InitCommonControlsEx, SetWindowTheme, BST_CHECKED, BST_UNCHECKED, CCS_BOTTOM, EM_SETCUEBANNER,
    ICC_BAR_CLASSES, ICC_LISTVIEW_CLASSES, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES,
    INITCOMMONCONTROLSEX, LVCFMT_LEFT, LVCFMT_RIGHT, LVCF_SUBITEM, LVCF_TEXT, LVCF_WIDTH,
    LVCOLUMNW, LVIF_TEXT, LVIS_FOCUSED, LVIS_SELECTED, LVITEMW, LVM_INSERTCOLUMNW,
    LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMCOUNT, LVM_SETITEMSTATE,
    LVN_GETDISPINFO, LVN_ITEMCHANGED, LVN_KEYDOWN, LVSICF_NOINVALIDATEALL, LVSICF_NOSCROLL,
    LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP, LVS_OWNERDATA, LVS_REPORT,
    LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMITEMACTIVATE, NMLISTVIEW, NMLVDISPINFOW, NMLVKEYDOWN,
    NM_DBLCLK, NM_RCLICK, PBM_SETPOS, PBM_SETRANGE32, PBS_SMOOTH, PROGRESS_CLASSW, SBARS_SIZEGRIP,
    SB_SETPARTS, SB_SETTEXT, STATUSCLASSNAMEW, WC_LISTVIEWW,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, SetFocus, VK_DELETE, VK_RETURN,
};
use windows_sys::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, AppendMenuW, BringWindowToTop, CreatePopupMenu, CreateWindowExW,
    DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW, EnumChildWindows, FindWindowW,
    GetClientRect, GetCursorPos, GetMessageW, GetParent, GetSystemMetrics, GetWindow,
    GetWindowRect, GetWindowTextLengthW, GetWindowTextW, IsIconic, IsZoomed, KillTimer,
    LoadCursorW, LoadImageW, MessageBoxW, MoveWindow, PostMessageW, PostQuitMessage,
    RegisterClassW, SendMessageW, SetForegroundWindow, SetMenuDefaultItem, SetTimer, SetWindowPos,
    SetWindowTextW, ShowWindow, SystemParametersInfoW, TrackPopupMenu, TranslateMessage,
    BM_GETCHECK, BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW,
    ES_AUTOHSCROLL, ES_NUMBER, GW_OWNER, ICON_BIG, ICON_SMALL, IDCANCEL, IDC_ARROW, IDOK,
    IMAGE_ICON, LR_DEFAULTCOLOR, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MF_SEPARATOR,
    MF_STRING, MINMAXINFO, MSG, NONCLIENTMETRICSW, SM_CXICON, SM_CXSMICON, SM_CYICON, SM_CYSMICON,
    SPI_GETNONCLIENTMETRICS, SPI_GETWORKAREA, SWP_NOACTIVATE, SWP_NOZORDER, SW_HIDE, SW_SHOW,
    SW_SHOWMAXIMIZED, SW_SHOWNA, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_CLIPBOARDUPDATE, WM_CLOSE,
    WM_COMMAND, WM_COPYDATA, WM_CREATE, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED,
    WM_ERASEBKGND, WM_GETMINMAXINFO, WM_KEYDOWN, WM_NCDESTROY, WM_NOTIFY, WM_NULL, WM_SETFONT,
    WM_SIZE, WM_TIMER, WNDCLASSW, WS_CAPTION, WS_CHILD, WS_CLIPCHILDREN, WS_EX_CLIENTEDGE,
    WS_EX_DLGMODALFRAME, WS_OVERLAPPED, WS_OVERLAPPEDWINDOW, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
};

use crate::autostart;
use crate::download::{DownloadManager, TaskStatus, TaskView};
use crate::settings::{Settings, WindowState};
use crate::tr;
use crate::tray::{
    MenuState, Tray, IDM_AUTOSTART, IDM_EXIT, IDM_MONITOR_CLIPBOARD, IDM_OPEN_FOLDER,
    IDM_PAUSE_ALL, IDM_RESUME_ALL, IDM_SETTINGS, IDM_SHOW, WM_TRAYICON,
};
use crate::util::{self, format_eta, format_speed, wide, APP_ICON_ID};

const CLASS_NAME: &str = "FlowDownMainWindow";
const SETTINGS_CLASS_NAME: &str = "FlowDownSettingsWindow";

const ID_URL_LABEL: usize = 1000;
const ID_URL: usize = 1001;
const ID_ADD: usize = 1002;
const ID_PAUSE: usize = 1003;
const ID_RESUME: usize = 1004;
const ID_REMOVE: usize = 1005;
const ID_FOLDER: usize = 1006;
const ID_SETTINGS: usize = 1007;
const ID_LIST: usize = 1008;
const ID_PROGRESS: usize = 1009;
const ID_STATUS: usize = 1010;
const ID_EXIT: usize = 1011;

const ID_SET_DIR: usize = 2000;
const ID_SET_BROWSE: usize = 2001;
const ID_SET_CONN: usize = 2002;
const ID_SET_MONITOR: usize = 2003;
const ID_SET_AUTO_START: usize = 2004;
const ID_SET_AUTOSTART: usize = 2005;
const ID_SET_CLOSE_TO_TRAY: usize = 2006;
const ID_SET_START_MIN: usize = 2007;

const IDM_OPEN: u32 = 2101;
const IDM_SHOW_FOLDER: u32 = 2102;
const IDM_COPY_LINK: u32 = 2103;
const IDM_PAUSE: u32 = 2104;
const IDM_RESUME: u32 = 2105;
const IDM_REMOVE: u32 = 2106;
const IDM_CLEAR_FINISHED: u32 = 2107;

const TIMER_POLL: usize = 1;

#[derive(Default)]
pub struct Options {
    pub settings: Settings,
    pub minimized: bool,
    pub links: Vec<String>,
}

struct App {
    hwnd: HWND,
    dpi: u32,
    font: *mut core::ffi::c_void,

    h_label: HWND,
    h_url: HWND,
    h_add: HWND,
    h_pause: HWND,
    h_resume: HWND,
    h_remove: HWND,
    h_folder: HWND,
    h_settings: HWND,
    h_exit: HWND,
    h_list: HWND,
    h_progress: HWND,
    h_status: HWND,

    manager: Arc<DownloadManager>,
    settings: Settings,

    tasks: Vec<TaskView>,
    speeds: HashMap<u64, u64>,
    last_done: HashMap<u64, u64>,
    last_tick: Instant,
    prev_status: HashMap<u64, TaskStatus>,

    selected_id: Option<u64>,
    list_count: usize,
    disp_buf: Vec<u16>,

    normal_rect: Option<(i32, i32, i32, i32)>,
    maximized: bool,

    tray: Tray,
    tray_tip: String,
    told_tray: bool,
    quitting: bool,

    last_clipboard: String,
    message: String,
    message_until: Instant,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    static READY: Cell<bool> = const { Cell::new(false) };
    static OPTS: RefCell<Option<Options>> = const { RefCell::new(None) };
    static DIALOG: RefCell<Option<SettingsDialog>> = const { RefCell::new(None) };
}

// ---------------------------------------------------------------- entry points

pub fn run(options: Options) -> i32 {
    let _ = options.settings.ensure_download_dir();
    let window = options.settings.window.clone();
    let minimized = options.minimized || options.settings.start_minimized;
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let controls = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES
                | ICC_PROGRESS_CLASS
                | ICC_BAR_CLASSES
                | ICC_STANDARD_CLASSES,
        };
        let _ = InitCommonControlsEx(&controls);

        let instance = GetModuleHandleW(std::ptr::null());
        let icon = load_icon(instance, SM_CXICON, SM_CYICON);
        for (name, proc_) in [
            (
                CLASS_NAME,
                wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            ),
            (
                SETTINGS_CLASS_NAME,
                settings_wndproc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
            ),
        ] {
            let class = wide(name);
            let wc = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(proc_),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: icon,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
            };
            if RegisterClassW(&wc) == 0 {
                return 1;
            }
        }

        let (x, y, width, height) = placement(window.as_ref());
        let class = wide(CLASS_NAME);
        let title = wide("FlowDown");
        OPTS.with(|cell| *cell.borrow_mut() = Some(options));
        let hwnd = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x,
            y,
            width,
            height,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        if hwnd.is_null() {
            return 1;
        }

        if minimized {
            ShowWindow(hwnd, SW_HIDE);
        } else if window.as_ref().map(|w| w.maximized).unwrap_or(false) {
            ShowWindow(hwnd, SW_SHOWMAXIMIZED);
        } else {
            ShowWindow(hwnd, SW_SHOW);
        }
        UpdateWindow(hwnd);

        let mut message: MSG = std::mem::zeroed();
        loop {
            let result = GetMessageW(&mut message, std::ptr::null_mut(), 0, 0);
            if result == 0 {
                break;
            }
            if result == -1 {
                break;
            }
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        message.wParam as i32
    }
}

/// Bring a running instance to the front, optionally handing it links.
pub fn notify_existing(links: &[String]) -> bool {
    let class = wide(CLASS_NAME);
    let hwnd = unsafe { FindWindowW(class.as_ptr(), std::ptr::null()) };
    if hwnd.is_null() {
        return false;
    }
    if links.is_empty() {
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        }
        return true;
    }
    for link in links {
        let bytes: Vec<u8> = link
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        let data = COPYDATASTRUCT {
            dwData: 1,
            cbData: bytes.len() as u32,
            lpData: bytes.as_ptr() as *mut core::ffi::c_void,
        };
        unsafe {
            SendMessageW(
                hwnd,
                WM_COPYDATA,
                0,
                &data as *const COPYDATASTRUCT as isize,
            );
        }
    }
    unsafe {
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
    }
    true
}

fn placement(saved: Option<&WindowState>) -> (i32, i32, i32, i32) {
    unsafe {
        let mut work = RECT::default();
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut work as *mut _ as *mut core::ffi::c_void,
            0,
        );
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 1040,
            bottom: 680,
        };
        AdjustWindowRectEx(&mut rect, WS_OVERLAPPEDWINDOW, 0, 0);
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        if let Some(state) = saved {
            let visible = state.x < work.right - 100
                && state.x + state.w > work.left + 100
                && state.y < work.bottom - 60
                && state.y + state.h > work.top + 40
                && state.w >= 700
                && state.h >= 420;
            if visible {
                return (state.x, state.y, state.w, state.h);
            }
        }
        let x = work.left + ((work.right - work.left) - width) / 2;
        let y = work.top + ((work.bottom - work.top) - height) / 2;
        (x, y, width, height)
    }
}

// ---------------------------------------------------------------------- window

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_CREATE {
        return on_create(hwnd);
    }
    if !READY.with(|ready| ready.get()) {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    match msg {
        WM_SIZE => {
            on_size(hwnd);
            0
        }
        WM_ERASEBKGND => {
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect);
            FillRect(
                wparam as *mut core::ffi::c_void,
                &rect,
                GetSysColorBrush(COLOR_BTNFACE),
            );
            1
        }
        WM_CTLCOLORSTATIC => {
            SetBkMode(wparam as *mut core::ffi::c_void, TRANSPARENT as i32);
            GetSysColorBrush(COLOR_BTNFACE) as LRESULT
        }
        WM_GETMINMAXINFO => {
            let dpi = with_app(|app| app.dpi).unwrap_or(96);
            let info = &mut *(lparam as *mut MINMAXINFO);
            info.ptMinTrackSize.x = scale(dpi, 900);
            info.ptMinTrackSize.y = scale(dpi, 480);
            0
        }
        WM_DPICHANGED => {
            on_dpi_changed(hwnd, wparam, lparam);
            0
        }
        WM_TIMER => {
            if wparam == TIMER_POLL {
                tick(hwnd);
            }
            0
        }
        WM_COMMAND => {
            on_command(hwnd, wparam);
            0
        }
        WM_NOTIFY => on_notify(hwnd, lparam),
        WM_CLIPBOARDUPDATE => {
            on_clipboard(hwnd);
            0
        }
        WM_COPYDATA => {
            on_copydata(hwnd, lparam);
            0
        }
        WM_TRAYICON => {
            on_tray(hwnd, lparam);
            0
        }
        WM_CLOSE => {
            let to_tray = with_app(|app| app.settings.close_to_tray).unwrap_or(false);
            if to_tray {
                hide_to_tray(hwnd);
            } else {
                quit(hwnd);
            }
            0
        }
        WM_DESTROY => {
            on_destroy(hwnd);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn on_create(hwnd: HWND) -> LRESULT {
    let options = OPTS
        .with(|cell| cell.borrow_mut().take())
        .unwrap_or_default();
    let links = options.links;
    let settings = options.settings;
    let maximized = settings
        .window
        .as_ref()
        .map(|w| w.maximized)
        .unwrap_or(false);
    let dpi = GetDpiForWindow(hwnd).max(96);
    let font = make_font(dpi);
    let manager = Arc::new(DownloadManager::new(
        settings.download_dir.clone(),
        settings.connections,
    ));
    let mut tray = Tray::new(hwnd);
    tray.add(tr!("FlowDown — starting…", "FlowDown — 正在启动…"));

    let mut rect = RECT::default();
    GetWindowRect(hwnd, &mut rect);
    let normal_rect = Some((
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
    ));
    let last_clipboard = util::clipboard_text().unwrap_or_default();

    let mut app = App {
        hwnd,
        dpi,
        font,
        h_label: std::ptr::null_mut(),
        h_url: std::ptr::null_mut(),
        h_add: std::ptr::null_mut(),
        h_pause: std::ptr::null_mut(),
        h_resume: std::ptr::null_mut(),
        h_remove: std::ptr::null_mut(),
        h_folder: std::ptr::null_mut(),
        h_settings: std::ptr::null_mut(),
        h_exit: std::ptr::null_mut(),
        h_list: std::ptr::null_mut(),
        h_progress: std::ptr::null_mut(),
        h_status: std::ptr::null_mut(),
        manager,
        settings,
        tasks: Vec::new(),
        speeds: HashMap::new(),
        last_done: HashMap::new(),
        last_tick: Instant::now(),
        prev_status: HashMap::new(),
        selected_id: None,
        list_count: 0,
        disp_buf: Vec::new(),
        normal_rect,
        maximized,
        tray,
        tray_tip: String::new(),
        told_tray: false,
        quitting: false,
        last_clipboard,
        message: String::new(),
        message_until: Instant::now(),
    };
    app.create_children();

    let monitor = app.settings.monitor_clipboard;
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    if monitor {
        AddClipboardFormatListener(hwnd);
    }
    READY.with(|ready| ready.set(true));

    set_icon(hwnd);
    layout(hwnd);
    SetTimer(hwnd, TIMER_POLL, 500, None);

    for link in links {
        let _ = add_links(hwnd, &[link], true);
    }
    tick(hwnd);
    0
}

unsafe fn on_destroy(hwnd: HWND) {
    KillTimer(hwnd, TIMER_POLL);
    RemoveClipboardFormatListener(hwnd);
    save_state(hwnd);
    if let Some(app) = APP.with(|cell| cell.borrow_mut().take()) {
        app.manager.shutdown();
        if !app.font.is_null() {
            DeleteObject(app.font);
        }
        drop(app);
    }
    PostQuitMessage(0);
}

unsafe fn on_size(hwnd: HWND) {
    if IsIconic(hwnd) == 0 && IsZoomed(hwnd) == 0 {
        let mut rect = RECT::default();
        if GetWindowRect(hwnd, &mut rect) != 0 {
            with_app_mut(|app| {
                app.normal_rect = Some((
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                ));
            });
        }
    }
    let zoomed = IsZoomed(hwnd) != 0;
    with_app_mut(|app| app.maximized = zoomed);
    layout(hwnd);
}

unsafe fn on_dpi_changed(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
    let dpi = (wparam & 0xffff) as u32;
    let rect = &*(lparam as *const RECT);
    SetWindowPos(
        hwnd,
        std::ptr::null_mut(),
        rect.left,
        rect.top,
        rect.right - rect.left,
        rect.bottom - rect.top,
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
    let (old, new) = with_app_mut(|app| {
        app.dpi = dpi.max(48);
        let old = app.font;
        app.font = make_font(app.dpi);
        (old, app.font)
    })
    .unwrap_or((std::ptr::null_mut(), std::ptr::null_mut()));
    if !old.is_null() {
        DeleteObject(old);
    }
    apply_font_to_children(hwnd, new);
    layout(hwnd);
}

unsafe fn on_command(hwnd: HWND, wparam: WPARAM) {
    let id = wparam & 0xffff;
    let code = ((wparam >> 16) & 0xffff) as u32;
    if code != BN_CLICKED {
        return;
    }
    match id {
        ID_ADD => add_from_edit_or_clipboard(hwnd),
        ID_PAUSE => pause_action(hwnd, true),
        ID_RESUME => resume_action(hwnd, true),
        ID_REMOVE => remove_selected(hwnd),
        ID_FOLDER => open_folder_for_selection(hwnd),
        ID_SETTINGS => open_settings(hwnd),
        ID_EXIT => quit(hwnd),
        _ => {}
    }
}

unsafe fn on_notify(hwnd: HWND, lparam: LPARAM) -> LRESULT {
    let header = lparam as *const windows_sys::Win32::UI::Controls::NMHDR;
    if header.is_null() {
        return 0;
    }
    match (*header).code {
        LVN_GETDISPINFO => {
            let info = lparam as *mut NMLVDISPINFOW;
            try_with_app_mut(|app| app.fill_dispinfo(info));
            0
        }
        LVN_ITEMCHANGED => {
            let change = &*(lparam as *const NMLISTVIEW);
            let was = change.uOldState & LVIS_SELECTED != 0;
            let is = change.uNewState & LVIS_SELECTED != 0;
            if was != is {
                let index = if is && change.iItem >= 0 {
                    Some(change.iItem as usize)
                } else {
                    None
                };
                with_app_mut(|app| {
                    app.selected_id = index.and_then(|i| app.tasks.get(i)).map(|task| task.id);
                });
                refresh_status(hwnd);
            }
            0
        }
        LVN_KEYDOWN => {
            let key = &*(lparam as *const NMLVKEYDOWN);
            match key.wVKey as u32 {
                k if k == VK_RETURN as u32 => open_selected(hwnd),
                k if k == VK_DELETE as u32 => remove_selected(hwnd),
                _ => {}
            }
            0
        }
        NM_DBLCLK => {
            let item = &*(lparam as *const NMITEMACTIVATE);
            if item.iItem >= 0 {
                select_index(item.iItem as usize);
                open_selected(hwnd);
            }
            0
        }
        NM_RCLICK => {
            let item = &*(lparam as *const NMITEMACTIVATE);
            if item.iItem >= 0 {
                select_index(item.iItem as usize);
                list_menu(hwnd);
            }
            0
        }
        _ => 0,
    }
}

unsafe fn on_copydata(hwnd: HWND, lparam: LPARAM) {
    let data = &*(lparam as *const COPYDATASTRUCT);
    if data.dwData != 1 || data.lpData.is_null() || data.cbData < 2 {
        return;
    }
    let len = data.cbData as usize / 2;
    let units = std::slice::from_raw_parts(data.lpData as *const u16, len);
    let text = String::from_utf16_lossy(units);
    let links = util::extract_links(&text);
    if links.is_empty() {
        return;
    }
    let _ = add_links(hwnd, &links, true);
}

unsafe fn on_clipboard(hwnd: HWND) {
    let Some(text) = util::clipboard_text() else {
        return;
    };
    let changed = with_app_mut(|app| {
        let changed = app.last_clipboard != text;
        app.last_clipboard = text.clone();
        changed
    })
    .unwrap_or(false);
    if !changed {
        return;
    }
    let (monitor, start) = with_app(|app| {
        (
            app.settings.monitor_clipboard,
            app.settings.auto_start_downloads,
        )
    })
    .unwrap_or((false, false));
    if !monitor {
        return;
    }
    let links = util::extract_links(&text);
    if links.is_empty() {
        return;
    }
    match add_links(hwnd, &links, start) {
        Ok(0) => {}
        Ok(count) => {
            let message = tr!(
                format!(
                    "Added {} link{} from the clipboard",
                    count,
                    if count == 1 { "" } else { "s" }
                ),
                format!("已从剪贴板添加 {count} 个链接")
            );
            set_message(hwnd, &message);
            with_app_mut(|app| {
                app.tray.notify("FlowDown", &message);
            });
        }
        Err(error) => set_message(hwnd, &error),
    }
}

unsafe fn on_tray(hwnd: HWND, lparam: LPARAM) {
    let event = lparam as u32;
    const WM_LBUTTONDBLCLK: u32 = 0x0203;
    const WM_RBUTTONUP: u32 = 0x0205;
    const WM_CONTEXTMENU: u32 = 0x007B;
    if event == WM_LBUTTONDBLCLK {
        show_window(hwnd);
        return;
    }
    if event != WM_RBUTTONUP && event != WM_CONTEXTMENU {
        return;
    }
    let state = with_app(|app| {
        let has_active = app.tasks.iter().any(|task| task.status.is_active());
        MenuState {
            monitor_clipboard: app.settings.monitor_clipboard,
            autostart: app.settings.autostart,
            has_tasks: !app.tasks.is_empty(),
            has_active,
        }
    });
    let Some(state) = state else {
        return;
    };
    // TrackPopupMenu runs a nested message loop, so don't hold the APP
    // RefCell borrow while displaying the menu.
    let command = Tray::show_menu(hwnd, &state);
    match command {
        IDM_SHOW => show_window(hwnd),
        IDM_PAUSE_ALL => pause_action(hwnd, false),
        IDM_RESUME_ALL => resume_action(hwnd, false),
        IDM_OPEN_FOLDER => {
            let dir = with_app(|app| app.manager.download_dir()).unwrap_or_default();
            util::open_folder(&dir);
        }
        IDM_MONITOR_CLIPBOARD => toggle_monitor(hwnd),
        IDM_AUTOSTART => toggle_autostart(hwnd),
        IDM_SETTINGS => {
            show_window(hwnd);
            open_settings(hwnd);
        }
        IDM_EXIT => quit(hwnd),
        _ => {}
    }
}

// ------------------------------------------------------------------------ app

impl App {
    unsafe fn create_children(&mut self) {
        let instance = GetModuleHandleW(std::ptr::null());
        let edit = wide("EDIT");
        let button = wide("BUTTON");
        let stat = wide("STATIC");

        let child = |class: *const u16, text: &str, style: u32, ex: u32, id: usize| -> HWND {
            let text = wide(text);
            CreateWindowExW(
                ex,
                class,
                text.as_ptr(),
                style,
                0,
                0,
                10,
                10,
                self.hwnd,
                id as *mut core::ffi::c_void,
                instance,
                std::ptr::null(),
            )
        };

        self.h_label = child(
            stat.as_ptr(),
            tr!("Link", "链接"),
            WS_CHILD | WS_VISIBLE,
            0,
            ID_URL_LABEL,
        );
        self.h_url = child(
            edit.as_ptr(),
            "",
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE,
            ID_URL,
        );
        for (id, text) in [
            (ID_ADD, tr!("Add", "添加")),
            (ID_PAUSE, tr!("Pause", "暂停")),
            (ID_RESUME, tr!("Resume", "继续")),
            (ID_REMOVE, tr!("Remove", "移除")),
            (ID_FOLDER, tr!("Folder", "文件夹")),
            (ID_SETTINGS, tr!("Settings", "设置")),
            (ID_EXIT, tr!("Exit", "退出")),
        ] {
            let control = child(
                button.as_ptr(),
                text,
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
                0,
                id,
            );
            match id {
                ID_ADD => self.h_add = control,
                ID_PAUSE => self.h_pause = control,
                ID_RESUME => self.h_resume = control,
                ID_REMOVE => self.h_remove = control,
                ID_FOLDER => self.h_folder = control,
                ID_SETTINGS => self.h_settings = control,
                _ => self.h_exit = control,
            }
        }
        self.h_list = child(
            WC_LISTVIEWW,
            "",
            WS_CHILD
                | WS_VISIBLE
                | WS_TABSTOP
                | LVS_REPORT
                | LVS_OWNERDATA
                | LVS_SINGLESEL
                | LVS_SHOWSELALWAYS,
            WS_EX_CLIENTEDGE,
            ID_LIST,
        );
        self.h_progress = child(PROGRESS_CLASSW, "", WS_CHILD | PBS_SMOOTH, 0, ID_PROGRESS);
        self.h_status = child(
            STATUSCLASSNAMEW,
            "",
            WS_CHILD | WS_VISIBLE | SBARS_SIZEGRIP | CCS_BOTTOM as u32,
            0,
            ID_STATUS,
        );

        let theme = wide("Explorer");
        let _ = SetWindowTheme(self.h_list, theme.as_ptr(), std::ptr::null());
        let extended = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP;
        SendMessageW(
            self.h_list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            extended as usize,
            extended as isize,
        );

        let columns: [(&str, i32, i32); 6] = [
            (tr!("Name", "名称"), scale(self.dpi, 250), LVCFMT_LEFT),
            (tr!("Size", "大小"), scale(self.dpi, 90), LVCFMT_RIGHT),
            (tr!("Progress", "进度"), scale(self.dpi, 82), LVCFMT_RIGHT),
            (tr!("Status", "状态"), scale(self.dpi, 100), LVCFMT_LEFT),
            (tr!("Speed", "速度"), scale(self.dpi, 92), LVCFMT_RIGHT),
            (tr!("Link", "链接"), scale(self.dpi, 330), LVCFMT_LEFT),
        ];
        for (index, (name, width, align)) in columns.iter().enumerate() {
            let mut text: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let mut column = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM,
                fmt: *align,
                cx: *width,
                pszText: text.as_mut_ptr(),
                cchTextMax: text.len() as i32,
                iSubItem: index as i32,
                ..Default::default()
            };
            SendMessageW(
                self.h_list,
                LVM_INSERTCOLUMNW,
                index,
                &mut column as *mut _ as isize,
            );
        }

        let cue = wide(tr!(
            "Paste an HTTP, FTP or magnet link, then press Enter",
            "粘贴 HTTP、FTP 或磁力链接，然后按 Enter"
        ));
        SendMessageW(self.h_url, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);
        SetWindowSubclass(self.h_url, Some(url_subclass), 1, 0);
        apply_font_to_children(self.hwnd, self.font);
    }

    fn fill_dispinfo(&mut self, info: *mut NMLVDISPINFOW) {
        unsafe {
            let item = &mut (*info).item;
            if item.mask & LVIF_TEXT == 0 {
                return;
            }
            let row = item.iItem.max(0) as usize;
            let column = item.iSubItem.max(0) as usize;
            let Some(task) = self.tasks.get(row) else {
                return;
            };
            let text = match column {
                0 => task.name.clone(),
                1 => task.size_text(),
                2 => task.progress_text(),
                3 => task.status.label().to_owned(),
                4 => match self.speeds.get(&task.id).copied() {
                    Some(speed) if speed > 0 && task.status.is_active() => format_speed(speed),
                    _ => String::new(),
                },
                5 => task.display_url.clone(),
                _ => String::new(),
            };
            self.disp_buf.clear();
            self.disp_buf.extend(text.encode_utf16());
            self.disp_buf.push(0);
            item.pszText = self.disp_buf.as_mut_ptr();
        }
    }
}

// ---------------------------------------------------------------------- layout

fn scale(dpi: u32, value: i32) -> i32 {
    ((value as i64 * dpi as i64 + 48) / 96) as i32
}

unsafe fn layout(hwnd: HWND) {
    let Some((dpi, controls)) = with_app(|app| {
        (
            app.dpi,
            [
                app.h_label,
                app.h_url,
                app.h_add,
                app.h_pause,
                app.h_resume,
                app.h_remove,
                app.h_folder,
                app.h_settings,
                app.h_exit,
                app.h_list,
                app.h_progress,
                app.h_status,
            ],
        )
    }) else {
        return;
    };
    let h_status = controls[11];
    let h_progress = controls[10];
    let h_list = controls[9];
    let h_exit = controls[8];
    let h_settings = controls[7];
    let h_folder = controls[6];
    let h_remove = controls[5];
    let h_resume = controls[4];
    let h_pause = controls[3];
    let h_add = controls[2];
    let h_url = controls[1];
    let h_label = controls[0];

    let mut client = RECT::default();
    GetClientRect(hwnd, &mut client);
    let width = client.right.max(scale(dpi, 600));
    let height = client.bottom.max(scale(dpi, 300));

    let pad = scale(dpi, 10);
    let gap = scale(dpi, 6);
    let row = scale(dpi, 26);
    let label_w = scale(dpi, 34);
    let button_w = scale(dpi, 70);
    let buttons = [
        h_add, h_pause, h_resume, h_remove, h_folder, h_settings, h_exit,
    ];
    let total_buttons = buttons.len() as i32 * button_w + (buttons.len() as i32 - 1) * gap;
    let url_x = pad + label_w + gap;
    let url_w = (width - pad - url_x - total_buttons - gap).max(scale(dpi, 120));

    MoveWindow(h_label, pad, pad + scale(dpi, 3), label_w, row, 1);
    MoveWindow(h_url, url_x, pad, url_w, row, 1);
    let mut x = url_x + url_w + gap;
    for button in buttons {
        MoveWindow(button, x, pad, button_w, row, 1);
        x += button_w + gap;
    }

    SendMessageW(h_status, WM_SIZE, 0, 0);
    let parts = [scale(dpi, 340), width - scale(dpi, 210), -1];
    SendMessageW(h_status, SB_SETPARTS, parts.len(), parts.as_ptr() as isize);

    let status_h = scale(dpi, 24);
    let progress_h = scale(dpi, 12);
    let list_y = pad + row + pad;
    let list_h = (height - status_h - progress_h - gap - list_y).max(scale(dpi, 90));
    MoveWindow(
        h_list,
        pad,
        list_y,
        (width - 2 * pad).max(scale(dpi, 200)),
        list_h,
        1,
    );
    MoveWindow(
        h_progress,
        pad,
        list_y + list_h + gap,
        (width - 2 * pad).max(scale(dpi, 200)),
        progress_h,
        1,
    );

    let link_w = (width
        - 2 * pad
        - scale(dpi, 250)
        - scale(dpi, 90)
        - scale(dpi, 82)
        - scale(dpi, 100)
        - scale(dpi, 92))
    .max(scale(dpi, 140));
    let widths = [
        scale(dpi, 250),
        scale(dpi, 90),
        scale(dpi, 82),
        scale(dpi, 100),
        scale(dpi, 92),
        link_w,
    ];
    for (index, width) in widths.iter().enumerate() {
        SendMessageW(h_list, LVM_SETCOLUMNWIDTH, index, *width as isize);
    }
}

unsafe fn apply_font_to_children(hwnd: HWND, font: *mut core::ffi::c_void) {
    if font.is_null() {
        return;
    }
    unsafe extern "system" fn set_font(child: HWND, data: LPARAM) -> i32 {
        SendMessageW(child, WM_SETFONT, data as usize, 1);
        1
    }
    EnumChildWindows(hwnd, Some(set_font), font as isize);
}

unsafe fn make_font(dpi: u32) -> *mut core::ffi::c_void {
    let mut metrics: NONCLIENTMETRICSW = std::mem::zeroed();
    metrics.cbSize = std::mem::size_of_val(&metrics) as u32;
    SystemParametersInfoW(
        SPI_GETNONCLIENTMETRICS,
        metrics.cbSize,
        &mut metrics as *mut _ as *mut core::ffi::c_void,
        0,
    );
    let mut logical: LOGFONTW = metrics.lfMessageFont;
    logical.lfHeight = -((9 * dpi as i32 + 36) / 72);
    logical.lfWeight = FW_NORMAL as i32;
    CreateFontIndirectW(&logical)
}

unsafe fn load_icon(instance: *mut core::ffi::c_void, cx: i32, cy: i32) -> *mut core::ffi::c_void {
    LoadImageW(
        instance,
        APP_ICON_ID,
        IMAGE_ICON,
        GetSystemMetrics(cx),
        GetSystemMetrics(cy),
        LR_DEFAULTCOLOR,
    )
}

unsafe fn set_icon(hwnd: HWND) {
    let instance = GetModuleHandleW(std::ptr::null());
    for (kind, cx, cy) in [
        (ICON_BIG, SM_CXICON, SM_CYICON),
        (ICON_SMALL, SM_CXSMICON, SM_CYSMICON),
    ] {
        let icon = load_icon(instance, cx, cy);
        if !icon.is_null() {
            SendMessageW(
                hwnd,
                windows_sys::Win32::UI::WindowsAndMessaging::WM_SETICON,
                kind as usize,
                icon as isize,
            );
        }
    }
}

// ----------------------------------------------------------------------- tick

unsafe fn tick(hwnd: HWND) {
    let mut notifications: Vec<(String, String)> = Vec::new();
    let mut count = 0usize;

    with_app_mut(|app| {
        let now = Instant::now();
        let delta = now.duration_since(app.last_tick).as_secs_f64().max(0.05);
        let tasks = app.manager.snapshot();
        let mut speeds = HashMap::new();
        let mut done_map = HashMap::new();
        for task in &tasks {
            let previous = app.last_done.get(&task.id).copied().unwrap_or(task.done);
            let bytes = task.done.saturating_sub(previous);
            if task.status.is_active() && bytes > 0 {
                speeds.insert(task.id, (bytes as f64 / delta) as u64);
            }
            done_map.insert(task.id, task.done);
            let previous_status = app.prev_status.get(&task.id).copied();
            if task.status == TaskStatus::Completed
                && previous_status != Some(TaskStatus::Completed)
            {
                notifications.push((
                    tr!("Download complete", "下载完成").to_owned(),
                    task.name.clone(),
                ));
            } else if task.status == TaskStatus::Error
                && previous_status != Some(TaskStatus::Error)
                && previous_status.is_some()
            {
                notifications.push((
                    tr!("Download failed", "下载失败").to_owned(),
                    task.error.clone().unwrap_or_else(|| task.name.clone()),
                ));
            }
        }
        app.speeds = speeds;
        app.last_done = done_map;
        app.last_tick = now;
        app.prev_status = tasks.iter().map(|task| (task.id, task.status)).collect();
        app.tasks = tasks;
        count = app.tasks.len();
        if let Some(id) = app.selected_id {
            if !app.tasks.iter().any(|task| task.id == id) {
                app.selected_id = None;
            }
        }
    });

    let changed = with_app_mut(|app| {
        let changed = app.list_count != count;
        app.list_count = count;
        changed
    })
    .unwrap_or(false);
    if let Some(list) = with_app(|app| app.h_list) {
        if changed {
            SendMessageW(
                list,
                LVM_SETITEMCOUNT,
                count,
                (LVSICF_NOINVALIDATEALL | LVSICF_NOSCROLL) as isize,
            );
            sync_selection();
        }
        windows_sys::Win32::Graphics::Gdi::InvalidateRect(list, std::ptr::null(), 0);
    }

    for (title, body) in notifications.into_iter().take(3) {
        with_app_mut(|app| app.tray.notify(&title, &body));
    }

    refresh_status(hwnd);
    refresh_progress();
    refresh_tray_tip();

    with_app_mut(|app| {
        if app.message_until.elapsed().as_secs() > 0 && Instant::now() >= app.message_until {
            app.message.clear();
        }
    });
}

unsafe fn refresh_progress() {
    let (handle, position, visible) = with_app(|app| {
        let mut total = 0u64;
        let mut done = 0u64;
        for task in &app.tasks {
            if task.status.is_active() {
                total += task.total;
                done += task.done;
            }
        }
        let visible = total > 0;
        let position = if total > 0 {
            ((done as f64 / total as f64) * 1000.0).clamp(0.0, 1000.0) as u32
        } else {
            0
        };
        (app.h_progress, position, visible)
    })
    .unwrap_or((std::ptr::null_mut(), 0, false));
    if handle.is_null() {
        return;
    }
    SendMessageW(handle, PBM_SETRANGE32, 0, 1000);
    SendMessageW(handle, PBM_SETPOS, position as usize, 0);
    ShowWindow(handle, if visible { SW_SHOWNA } else { SW_HIDE });
}

unsafe fn refresh_status(_hwnd: HWND) {
    let (handle, left, middle, right) = with_app(|app| {
        let active = app
            .tasks
            .iter()
            .filter(|task| task.status.is_active())
            .count();
        let speed: u64 = app.speeds.values().copied().sum();
        let left = if !app.message.is_empty() && Instant::now() < app.message_until {
            app.message.clone()
        } else if let Some(task) = app
            .selected_id
            .and_then(|id| app.tasks.iter().find(|task| task.id == id))
        {
            let mut text = format!("{} — {}", task.name, task.detail());
            let task_speed = app.speeds.get(&task.id).copied().unwrap_or(0);
            if task.status.is_active() && task_speed > 0 && task.total > task.done {
                let seconds = (task.total - task.done) / task_speed;
                text.push_str(&tr!(
                    format!(" · ETA {}", format_eta(Some(seconds))),
                    format!(" · 剩余 {}", format_eta(Some(seconds)))
                ));
            }
            text
        } else if active > 0 {
            tr!(
                format!("{active} active · {}", format_speed(speed)),
                format!("{active} 个任务进行中 · {}", format_speed(speed))
            )
        } else if app.tasks.is_empty() {
            tr!(
                "Copy any HTTP, FTP or magnet link to get started",
                "复制 HTTP、FTP 或磁力链接即可开始下载"
            )
            .to_owned()
        } else {
            tr!("No active downloads", "没有正在下载的任务").to_owned()
        };
        let middle = tr!(
            format!("Saving to {}", app.manager.download_dir().display()),
            format!("保存到 {}", app.manager.download_dir().display())
        );
        let right = if app.settings.monitor_clipboard {
            tr!("Clipboard: on", "剪贴板：开").to_owned()
        } else {
            tr!("Clipboard: off", "剪贴板：关").to_owned()
        };
        (app.h_status, left, middle, right)
    })
    .unwrap_or((
        std::ptr::null_mut(),
        String::new(),
        String::new(),
        String::new(),
    ));
    if handle.is_null() {
        return;
    }
    let left = wide(&left);
    SendMessageW(handle, SB_SETTEXT, 0, left.as_ptr() as isize);
    let middle = wide(&middle);
    SendMessageW(handle, SB_SETTEXT, 1, middle.as_ptr() as isize);
    let right = wide(&right);
    SendMessageW(handle, SB_SETTEXT, 2, right.as_ptr() as isize);
}

unsafe fn refresh_tray_tip() {
    with_app_mut(|app| {
        let active = app
            .tasks
            .iter()
            .filter(|task| task.status.is_active())
            .count();
        let tip = if active > 0 {
            tr!(
                format!("FlowDown — {active} active, {} items", app.tasks.len()),
                format!("FlowDown — {active} 个下载中，共 {} 项", app.tasks.len())
            )
        } else if app.tasks.is_empty() {
            tr!("FlowDown — idle", "FlowDown — 空闲").to_owned()
        } else {
            tr!(
                format!("FlowDown — {} items", app.tasks.len()),
                format!("FlowDown — {} 项", app.tasks.len())
            )
        };
        if app.tray_tip != tip {
            app.tray_tip = tip.clone();
            app.tray.update_tip(&tip);
        }
    });
}

unsafe fn sync_selection() {
    let Some((list, index)) = with_app(|app| {
        let index = app
            .selected_id
            .and_then(|id| app.tasks.iter().position(|task| task.id == id));
        (app.h_list, index)
    }) else {
        return;
    };
    if let Some(index) = index {
        let mut item = LVITEMW {
            state: LVIS_SELECTED | LVIS_FOCUSED,
            stateMask: LVIS_SELECTED | LVIS_FOCUSED,
            ..Default::default()
        };
        SendMessageW(list, LVM_SETITEMSTATE, index, &mut item as *mut _ as isize);
    } else {
        let mut item = LVITEMW {
            state: 0,
            stateMask: LVIS_SELECTED | LVIS_FOCUSED,
            ..Default::default()
        };
        SendMessageW(
            list,
            LVM_SETITEMSTATE,
            usize::MAX,
            &mut item as *mut _ as isize,
        );
    }
}

// -------------------------------------------------------------------- actions

unsafe fn set_message(hwnd: HWND, text: &str) {
    with_app_mut(|app| {
        app.message = text.to_owned();
        app.message_until = Instant::now() + Duration::from_secs(5);
    });
    refresh_status(hwnd);
}

unsafe fn add_links(hwnd: HWND, links: &[String], start: bool) -> Result<usize, String> {
    let (manager, auto_start) =
        with_app(|app| (app.manager.clone(), app.settings.auto_start_downloads))
            .ok_or_else(|| tr!("FlowDown is shutting down.", "FlowDown 正在退出。").to_owned())?;
    let existing: HashSet<String> = manager
        .snapshot()
        .into_iter()
        .map(|task| task.url)
        .collect();
    let mut seen = HashSet::new();
    let mut last_error = None;
    let mut added = 0usize;
    for link in links {
        let link = link.trim();
        if link.is_empty() || existing.contains(link) || !seen.insert(link.to_owned()) {
            continue;
        }
        match manager.add_link(link, start && auto_start) {
            Ok(_) => added += 1,
            Err(error) => last_error = Some(error),
        }
    }
    if added == 0 {
        if let Some(error) = last_error {
            return Err(error);
        }
        return Err(tr!("That link is already in the list.", "该链接已在列表中。").to_owned());
    }
    tick(hwnd);
    Ok(added)
}

unsafe fn add_from_edit_or_clipboard(hwnd: HWND) {
    let edit = with_app(|app| app.h_url);
    let mut text = edit.map(get_text).unwrap_or_default();
    if text.trim().is_empty() {
        text = util::clipboard_text().unwrap_or_default();
    }
    let links = util::extract_links(&text);
    let links = if links.is_empty() && !text.trim().is_empty() {
        vec![text.trim().to_owned()]
    } else {
        links
    };
    if links.is_empty() {
        message_box(
            hwnd,
            tr!("Enter or copy a link first.", "请先输入或复制链接。"),
        );
        return;
    }
    match add_links(hwnd, &links, true) {
        Ok(count) => {
            if let Some(edit) = edit.filter(|handle| !handle.is_null()) {
                SetWindowTextW(edit, wide("").as_ptr());
                SetFocus(edit);
            }
            set_message(
                hwnd,
                &tr!(
                    format!("Added {} link{}", count, if count == 1 { "" } else { "s" }),
                    format!("已添加 {count} 个链接")
                ),
            );
        }
        Err(error) => {
            message_box(hwnd, &error);
        }
    }
}

fn manager_and_selection() -> Option<(Arc<DownloadManager>, Option<u64>)> {
    with_app(|app| (app.manager.clone(), app.selected_id))
}

unsafe fn pause_action(hwnd: HWND, selected_only: bool) {
    let Some((manager, id)) = manager_and_selection() else {
        return;
    };
    match (selected_only, id) {
        (true, Some(id)) => manager.pause(id),
        _ => manager.pause_all(),
    }
    tick(hwnd);
}

unsafe fn resume_action(hwnd: HWND, selected_only: bool) {
    let Some((manager, id)) = manager_and_selection() else {
        return;
    };
    match (selected_only, id) {
        (true, Some(id)) => manager.resume(id),
        _ => manager.resume_all(),
    }
    tick(hwnd);
}

unsafe fn remove_selected(hwnd: HWND) {
    let Some((manager, id)) = manager_and_selection() else {
        return;
    };
    if let Some(id) = id {
        manager.remove(id);
        with_app_mut(|app| app.selected_id = None);
        tick(hwnd);
    }
}

unsafe fn open_selected(hwnd: HWND) {
    let Some(task) = with_app(|app| {
        app.selected_id
            .and_then(|id| app.tasks.iter().find(|task| task.id == id).cloned())
    })
    .flatten() else {
        return;
    };
    if let Some(path) = task.dest_path {
        if PathBuf::from(&path).exists() {
            if path.is_dir() {
                util::open_folder(&path);
            } else {
                util::open_path(&path);
            }
            return;
        }
    }
    if task.status == TaskStatus::Completed {
        util::open_folder(&task.dest_dir);
    } else {
        message_box(
            hwnd,
            tr!("This download has not finished yet.", "该下载尚未完成。"),
        );
    }
}

unsafe fn open_folder_for_selection(_hwnd: HWND) {
    let target = with_app(|app| {
        app.selected_id
            .and_then(|id| app.tasks.iter().find(|task| task.id == id))
            .and_then(|task| task.dest_path.clone())
    })
    .flatten();
    if let Some(path) = target {
        util::reveal_path(&path);
    } else if let Some(dir) = with_app(|app| app.manager.download_dir()) {
        util::open_folder(&dir);
    }
}

unsafe fn toggle_monitor(hwnd: HWND) {
    let enable = with_app(|app| !app.settings.monitor_clipboard).unwrap_or(true);
    if enable {
        AddClipboardFormatListener(hwnd);
        let current = util::clipboard_text().unwrap_or_default();
        with_app_mut(|app| app.last_clipboard = current);
    } else {
        RemoveClipboardFormatListener(hwnd);
    }
    with_app_mut(|app| {
        app.settings.monitor_clipboard = enable;
        let _ = app.settings.save();
    });
    refresh_status(hwnd);
    set_message(
        hwnd,
        if enable {
            tr!("Clipboard monitoring enabled", "已开启剪贴板监视")
        } else {
            tr!("Clipboard monitoring disabled", "已关闭剪贴板监视")
        },
    );
}

unsafe fn toggle_autostart(hwnd: HWND) {
    let enable = with_app(|app| !app.settings.autostart).unwrap_or(false);
    match autostart::set_enabled(enable) {
        Ok(()) => {
            with_app_mut(|app| {
                app.settings.autostart = enable;
                let _ = app.settings.save();
            });
            set_message(
                hwnd,
                if enable {
                    tr!(
                        "FlowDown will start with Windows",
                        "FlowDown 将随 Windows 启动"
                    )
                } else {
                    tr!(
                        "FlowDown will no longer start with Windows",
                        "FlowDown 将不再随 Windows 启动"
                    )
                },
            );
        }
        Err(error) => message_box(
            hwnd,
            &tr!(
                format!("Could not update autostart: {error}"),
                format!("无法更新开机启动设置：{error}")
            ),
        ),
    }
}

unsafe fn show_window(hwnd: HWND) {
    ShowWindow(hwnd, SW_SHOW);
    BringWindowToTop(hwnd);
    SetForegroundWindow(hwnd);
}

unsafe fn hide_to_tray(hwnd: HWND) {
    save_state(hwnd);
    ShowWindow(hwnd, SW_HIDE);
    let Some(told) = with_app(|app| app.told_tray) else {
        return;
    };
    if !told {
        with_app_mut(|app| {
            app.told_tray = true;
            app.tray.notify(
                tr!("FlowDown is still running", "FlowDown 仍在运行"),
                tr!(
                    "Downloads continue in the background. Use the tray icon to reopen or exit.",
                    "下载会在后台继续。使用托盘图标重新打开或退出。"
                ),
            );
        });
    }
}

unsafe fn quit(hwnd: HWND) {
    with_app_mut(|app| app.quitting = true);
    save_state(hwnd);
    DestroyWindow(hwnd);
}

unsafe fn save_state(hwnd: HWND) {
    let mut rect = RECT::default();
    if GetWindowRect(hwnd, &mut rect) != 0 && IsIconic(hwnd) == 0 && IsZoomed(hwnd) == 0 {
        with_app_mut(|app| {
            app.normal_rect = Some((
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
            ));
        });
    }
    with_app_mut(|app| {
        let mut settings = app.settings.clone();
        if let Some((x, y, w, h)) = app.normal_rect {
            settings.window = Some(WindowState {
                x,
                y,
                w,
                h,
                maximized: app.maximized,
            });
        }
        let _ = settings.save();
        app.settings = settings;
    });
}

unsafe fn message_box(hwnd: HWND, text: &str) {
    let text = wide(text);
    let title = wide("FlowDown");
    MessageBoxW(
        hwnd,
        text.as_ptr(),
        title.as_ptr(),
        MB_OK | MB_ICONINFORMATION,
    );
}

// ---------------------------------------------------------------- list menu

unsafe fn select_index(index: usize) {
    let Some(list) = with_app(|app| app.h_list) else {
        return;
    };
    let mut item = LVITEMW {
        state: LVIS_SELECTED | LVIS_FOCUSED,
        stateMask: LVIS_SELECTED | LVIS_FOCUSED,
        ..Default::default()
    };
    SendMessageW(list, LVM_SETITEMSTATE, index, &mut item as *mut _ as isize);
    SetFocus(list);
    with_app_mut(|app| {
        app.selected_id = app.tasks.get(index).map(|task| task.id);
    });
}

unsafe fn list_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let open = wide(tr!("Open", "打开"));
    let folder = wide(tr!("Show in folder", "在文件夹中显示"));
    let copy = wide(tr!("Copy link", "复制链接"));
    let pause = wide(tr!("Pause", "暂停"));
    let resume = wide(tr!("Resume", "继续"));
    let remove = wide(tr!("Remove", "移除"));
    let clear = wide(tr!("Clear finished", "清除已完成"));
    AppendMenuW(menu, MF_STRING, IDM_OPEN as usize, open.as_ptr());
    AppendMenuW(menu, MF_STRING, IDM_SHOW_FOLDER as usize, folder.as_ptr());
    AppendMenuW(menu, MF_STRING, IDM_COPY_LINK as usize, copy.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, IDM_PAUSE as usize, pause.as_ptr());
    AppendMenuW(menu, MF_STRING, IDM_RESUME as usize, resume.as_ptr());
    AppendMenuW(menu, MF_STRING, IDM_REMOVE as usize, remove.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    AppendMenuW(menu, MF_STRING, IDM_CLEAR_FINISHED as usize, clear.as_ptr());
    SetMenuDefaultItem(menu, IDM_OPEN, 0);

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
    PostMessageW(hwnd, WM_NULL, 0, 0);

    match command as u32 {
        IDM_OPEN => open_selected(hwnd),
        IDM_SHOW_FOLDER => open_folder_for_selection(hwnd),
        IDM_COPY_LINK => {
            if let Some(link) = with_app(|app| {
                app.selected_id
                    .and_then(|id| app.tasks.iter().find(|task| task.id == id))
                    .map(|task| task.url.clone())
            })
            .flatten()
            {
                match util::set_clipboard_text(&link) {
                    Ok(()) => set_message(
                        hwnd,
                        tr!("Link copied to the clipboard", "链接已复制到剪贴板"),
                    ),
                    Err(error) => message_box(
                        hwnd,
                        &tr!(
                            format!("Could not copy: {error}"),
                            format!("无法复制：{error}")
                        ),
                    ),
                }
            }
        }
        IDM_PAUSE => pause_action(hwnd, true),
        IDM_RESUME => resume_action(hwnd, true),
        IDM_REMOVE => remove_selected(hwnd),
        IDM_CLEAR_FINISHED => {
            if let Some(manager) = with_app(|app| app.manager.clone()) {
                manager.clear_finished();
            }
            tick(hwnd);
        }
        _ => {}
    }
}

// -------------------------------------------------------------- edit subclass

unsafe extern "system" fn url_subclass(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_KEYDOWN if wparam == VK_RETURN as usize => {
            let parent = GetParent(hwnd);
            add_from_edit_or_clipboard(parent);
            return 0;
        }
        windows_sys::Win32::UI::WindowsAndMessaging::WM_CHAR
            if wparam == VK_RETURN as usize || wparam == 10 =>
        {
            return 0;
        }
        WM_NCDESTROY => {}
        _ => {}
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}

// ------------------------------------------------------------ settings dialog

struct SettingsDialog {
    owner: HWND,
    dpi: u32,
    font: *mut core::ffi::c_void,
    h_dir: HWND,
    h_browse: HWND,
    h_conn: HWND,
    h_monitor: HWND,
    h_auto_start: HWND,
    h_autostart: HWND,
    h_close_to_tray: HWND,
    h_start_min: HWND,
    h_ok: HWND,
    h_cancel: HWND,
    h_intro: HWND,
    h_conn_label: HWND,
    h_options: HWND,
}

unsafe fn open_settings(owner: HWND) {
    if DIALOG.with(|cell| cell.borrow().is_some()) {
        return;
    }
    let instance = GetModuleHandleW(std::ptr::null());
    let class = wide(SETTINGS_CLASS_NAME);
    let title = wide(tr!("FlowDown Settings", "FlowDown 设置"));
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN;
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 540,
        bottom: 360,
    };
    AdjustWindowRectEx(&mut rect, style, 0, WS_EX_DLGMODALFRAME);
    let mut owner_rect = RECT::default();
    GetWindowRect(owner, &mut owner_rect);
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let x = owner_rect.left + ((owner_rect.right - owner_rect.left) - width) / 2;
    let y = owner_rect.top + ((owner_rect.bottom - owner_rect.top) - height) / 2;
    EnableWindow(owner, 0);
    let dialog = CreateWindowExW(
        WS_EX_DLGMODALFRAME,
        class.as_ptr(),
        title.as_ptr(),
        style,
        x,
        y,
        width,
        height,
        owner,
        std::ptr::null_mut(),
        instance,
        std::ptr::null(),
    );
    if dialog.is_null() {
        EnableWindow(owner, 1);
        return;
    }
    ShowWindow(dialog, SW_SHOW);
}

unsafe extern "system" fn settings_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            if let Some(dialog) = SettingsDialog::create(hwnd) {
                DIALOG.with(|cell| *cell.borrow_mut() = Some(dialog));
                settings_layout(hwnd);
                0
            } else {
                -1
            }
        }
        WM_SIZE => {
            settings_layout(hwnd);
            0
        }
        WM_CTLCOLORSTATIC => {
            SetBkMode(wparam as *mut core::ffi::c_void, TRANSPARENT as i32);
            GetSysColorBrush(COLOR_BTNFACE) as LRESULT
        }
        WM_ERASEBKGND => {
            let mut rect = RECT::default();
            GetClientRect(hwnd, &mut rect);
            FillRect(
                wparam as *mut core::ffi::c_void,
                &rect,
                GetSysColorBrush(COLOR_BTNFACE),
            );
            1
        }
        WM_COMMAND => {
            settings_command(hwnd, wparam);
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            let dialog = DIALOG.with(|cell| cell.borrow_mut().take());
            if let Some(dialog) = dialog {
                EnableWindow(dialog.owner, 1);
                SetForegroundWindow(dialog.owner);
                if !dialog.font.is_null() {
                    DeleteObject(dialog.font);
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

impl SettingsDialog {
    unsafe fn create(hwnd: HWND) -> Option<Self> {
        let settings = with_app(|app| app.settings.clone())?;
        let owner = GetWindow(hwnd, GW_OWNER);
        let dpi = GetDpiForWindow(hwnd).max(96);
        let font = make_font(dpi);
        let instance = GetModuleHandleW(std::ptr::null());
        let edit = wide("EDIT");
        let button = wide("BUTTON");
        let stat = wide("STATIC");

        let child = |class: *const u16, text: &str, style: u32, ex: u32, id: usize| -> HWND {
            let text = wide(text);
            CreateWindowExW(
                ex,
                class,
                text.as_ptr(),
                style,
                0,
                0,
                10,
                10,
                hwnd,
                id as *mut core::ffi::c_void,
                instance,
                std::ptr::null(),
            )
        };

        let h_intro = child(
            stat.as_ptr(),
            tr!("Download folder", "下载文件夹"),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
        );
        let h_dir = child(
            edit.as_ptr(),
            &settings.download_dir.display().to_string(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE,
            ID_SET_DIR,
        );
        let h_browse = child(
            button.as_ptr(),
            tr!("Browse…", "浏览…"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            ID_SET_BROWSE,
        );
        let h_conn_label = child(
            stat.as_ptr(),
            tr!("Connections per download", "每个任务的连接数"),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
        );
        let h_conn = child(
            edit.as_ptr(),
            &settings.connections.to_string(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32 | ES_NUMBER as u32,
            WS_EX_CLIENTEDGE,
            ID_SET_CONN,
        );
        let h_options = child(
            stat.as_ptr(),
            tr!("Behavior", "行为"),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
        );
        let checkbox = |text: &str, checked: bool, id: usize| -> HWND {
            let handle = child(
                button.as_ptr(),
                text,
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_AUTOCHECKBOX as u32,
                0,
                id,
            );
            SendMessageW(
                handle,
                BM_SETCHECK,
                if checked {
                    BST_CHECKED as usize
                } else {
                    BST_UNCHECKED as usize
                },
                0,
            );
            handle
        };
        let h_monitor = checkbox(
            tr!("Watch the clipboard for links", "监视剪贴板中的链接"),
            settings.monitor_clipboard,
            ID_SET_MONITOR,
        );
        let h_auto_start = checkbox(
            tr!("Start downloads automatically", "自动开始下载"),
            settings.auto_start_downloads,
            ID_SET_AUTO_START,
        );
        let h_autostart = checkbox(
            tr!("Start FlowDown with Windows", "FlowDown 随 Windows 启动"),
            settings.autostart,
            ID_SET_AUTOSTART,
        );
        let h_close_to_tray = checkbox(
            tr!(
                "Closing the window keeps FlowDown in the tray",
                "关闭窗口时保留在托盘"
            ),
            settings.close_to_tray,
            ID_SET_CLOSE_TO_TRAY,
        );
        let h_start_min = checkbox(
            tr!("Start minimized to the tray", "启动时最小化到托盘"),
            settings.start_minimized,
            ID_SET_START_MIN,
        );
        let h_ok = child(
            button.as_ptr(),
            tr!("Save", "保存"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            IDOK as usize,
        );
        let h_cancel = child(
            button.as_ptr(),
            tr!("Cancel", "取消"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            IDCANCEL as usize,
        );

        let dialog = Self {
            owner,
            dpi,
            font,
            h_dir,
            h_browse,
            h_conn,
            h_monitor,
            h_auto_start,
            h_autostart,
            h_close_to_tray,
            h_start_min,
            h_ok,
            h_cancel,
            h_intro,
            h_conn_label,
            h_options,
        };
        apply_font_to_children(hwnd, font);
        SetFocus(h_dir);
        Some(dialog)
    }

    unsafe fn collect(&self) -> Result<Settings, String> {
        let mut settings = with_app(|app| app.settings.clone())
            .ok_or_else(|| tr!("FlowDown is closing.", "FlowDown 正在关闭。").to_owned())?;
        let dir = get_text(self.h_dir).trim().to_owned();
        if dir.is_empty() {
            return Err(tr!("Choose a download folder.", "请选择下载文件夹。").to_owned());
        }
        settings.download_dir = PathBuf::from(dir);
        std::fs::create_dir_all(&settings.download_dir).map_err(|error| {
            tr!(
                format!("Could not create that folder: {error}"),
                format!("无法创建该文件夹：{error}")
            )
        })?;
        let connections = get_text(self.h_conn)
            .trim()
            .parse::<usize>()
            .map_err(|_| tr!("Connections must be a number.", "连接数必须是数字。").to_owned())?;
        if !(1..=32).contains(&connections) {
            return Err(tr!(
                "Connections must be between 1 and 32.",
                "连接数必须在 1 到 32 之间。"
            )
            .to_owned());
        }
        settings.connections = connections;
        settings.monitor_clipboard = is_checked(self.h_monitor);
        settings.auto_start_downloads = is_checked(self.h_auto_start);
        settings.autostart = is_checked(self.h_autostart);
        settings.close_to_tray = is_checked(self.h_close_to_tray);
        settings.start_minimized = is_checked(self.h_start_min);
        Ok(settings)
    }
}

unsafe fn settings_layout(hwnd: HWND) {
    let Some(dialog) = DIALOG.with(|cell| {
        let borrow = cell.borrow();
        borrow.as_ref().map(|dialog| {
            (
                dialog.dpi,
                dialog.h_intro,
                dialog.h_dir,
                dialog.h_browse,
                dialog.h_conn_label,
                dialog.h_conn,
                dialog.h_options,
                dialog.h_monitor,
                dialog.h_auto_start,
                dialog.h_autostart,
                dialog.h_close_to_tray,
                dialog.h_start_min,
                dialog.h_ok,
                dialog.h_cancel,
            )
        })
    }) else {
        return;
    };
    let (
        dpi,
        h_intro,
        h_dir,
        h_browse,
        h_conn_label,
        h_conn,
        h_options,
        h_monitor,
        h_auto_start,
        h_autostart,
        h_close,
        h_min,
        h_ok,
        h_cancel,
    ) = dialog;

    let mut client = RECT::default();
    GetClientRect(hwnd, &mut client);
    let width = client.right.max(scale(dpi, 400));
    let height = client.bottom.max(scale(dpi, 280));
    let pad = scale(dpi, 14);
    let row = scale(dpi, 26);
    let gap = scale(dpi, 8);
    let browse_w = scale(dpi, 84);

    let mut y = pad;
    MoveWindow(h_intro, pad, y, width - 2 * pad, scale(dpi, 18), 1);
    y += scale(dpi, 20);
    MoveWindow(
        h_dir,
        pad,
        y,
        (width - 2 * pad - browse_w - gap).max(scale(dpi, 120)),
        row,
        1,
    );
    MoveWindow(h_browse, width - pad - browse_w, y, browse_w, row, 1);
    y += row + gap + scale(dpi, 4);
    MoveWindow(
        h_conn_label,
        pad,
        y + scale(dpi, 5),
        scale(dpi, 170),
        row,
        1,
    );
    MoveWindow(h_conn, pad + scale(dpi, 176), y, scale(dpi, 64), row, 1);
    y += row + gap + scale(dpi, 4);
    MoveWindow(h_options, pad, y, width - 2 * pad, scale(dpi, 18), 1);
    y += scale(dpi, 22);
    for checkbox in [h_monitor, h_auto_start, h_autostart, h_close, h_min] {
        MoveWindow(checkbox, pad, y, width - 2 * pad, row, 1);
        y += row;
    }
    let button_w = scale(dpi, 84);
    MoveWindow(
        h_cancel,
        width - pad - button_w,
        height - pad - row,
        button_w,
        row,
        1,
    );
    MoveWindow(
        h_ok,
        width - pad - button_w * 2 - gap,
        height - pad - row,
        button_w,
        row,
        1,
    );
}

unsafe fn settings_command(dialog_hwnd: HWND, wparam: WPARAM) {
    let id = wparam & 0xffff;
    match id as u32 {
        x if x == IDOK as u32 => {
            let Some((owner, result)) = DIALOG.with(|cell| {
                cell.borrow()
                    .as_ref()
                    .map(|dialog| (dialog.owner, dialog.collect()))
            }) else {
                return;
            };
            match result {
                Ok(settings) => {
                    DestroyWindow(dialog_hwnd);
                    apply_settings(owner, settings);
                }
                Err(error) => {
                    let text = wide(&error);
                    let title = wide(tr!("FlowDown Settings", "FlowDown 设置"));
                    MessageBoxW(
                        dialog_hwnd,
                        text.as_ptr(),
                        title.as_ptr(),
                        MB_OK | MB_ICONWARNING,
                    );
                }
            }
        }
        x if x == IDCANCEL as u32 => {
            DestroyWindow(dialog_hwnd);
        }
        x if x == ID_SET_BROWSE as u32 => {
            let picked = util::pick_folder(
                dialog_hwnd,
                tr!(
                    "Choose the FlowDown download folder",
                    "选择 FlowDown 下载文件夹"
                ),
            );
            if let Some(path) = picked {
                if let Some(dir) =
                    DIALOG.with(|cell| cell.borrow().as_ref().map(|dialog| dialog.h_dir))
                {
                    SetWindowTextW(dir, wide(&path).as_ptr());
                }
            }
        }
        _ => {}
    }
}

unsafe fn apply_settings(owner: HWND, new_settings: Settings) {
    let Some((old_monitor, manager, old_dir)) = with_app(|app| {
        (
            app.settings.monitor_clipboard,
            app.manager.clone(),
            app.manager.download_dir(),
        )
    }) else {
        return;
    };

    if new_settings.download_dir != old_dir {
        manager.set_download_dir(new_settings.download_dir.clone());
    }
    manager.set_connections(new_settings.connections);
    let autostart_changed = new_settings.autostart;
    let autostart_ok = autostart::set_enabled(autostart_changed).is_ok();
    if old_monitor != new_settings.monitor_clipboard {
        if new_settings.monitor_clipboard {
            AddClipboardFormatListener(owner);
        } else {
            RemoveClipboardFormatListener(owner);
        }
    }
    with_app_mut(|app| {
        app.settings = new_settings;
        if !autostart_ok {
            app.settings.autostart = !autostart_changed;
        }
        let _ = app.settings.save();
    });
    tick(owner);
    refresh_status(owner);
    if !autostart_ok {
        message_box(
            owner,
            tr!(
                "The autostart registry entry could not be updated.",
                "无法更新开机启动注册表项。"
            ),
        );
    }
}

unsafe fn is_checked(hwnd: HWND) -> bool {
    SendMessageW(hwnd, BM_GETCHECK, 0, 0) as usize == BST_CHECKED as usize
}

// --------------------------------------------------------------------- helpers

fn get_text(hwnd: HWND) -> String {
    if hwnd.is_null() {
        return String::new();
    }
    unsafe {
        let length = GetWindowTextLengthW(hwnd);
        if length <= 0 {
            return String::new();
        }
        let mut buffer = vec![0u16; length as usize + 1];
        let written = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32);
        String::from_utf16_lossy(&buffer[..written.max(0) as usize])
    }
}

fn with_app<R>(f: impl FnOnce(&App) -> R) -> Option<R> {
    APP.with(|cell| cell.borrow().as_ref().map(f))
}

fn with_app_mut<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| cell.borrow_mut().as_mut().map(f))
}

fn try_with_app_mut<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|cell| {
        cell.try_borrow_mut()
            .ok()
            .and_then(|mut borrow| borrow.as_mut().map(f))
    })
}
