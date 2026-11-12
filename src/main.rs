//! FlowDown — a native Windows download manager.
//!
//! The binary is a single executable: no installer, no runtime, no bundled
//! Chromium. Clipboard monitoring, multi-connection HTTP/FTP transfers and a
//! BitTorrent session run behind one small Win32 UI.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod download;
mod lang;
mod settings;
mod torrent;
mod tray;
mod ui;
mod util;

use std::time::Duration;

use windows_sys::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;

use crate::settings::Settings;
use crate::util::wide;

fn main() {
    // Resolve the UI language once, before any thread starts building strings.
    let _ = lang::chinese();
    settings::ensure_dirs();

    let mut links = Vec::new();
    let mut minimized = false;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--minimized" | "/min" => minimized = true,
            "--add" => {
                if let Some(link) = arguments.next() {
                    links.push(link);
                }
            }
            value if is_link(value) => links.push(value.to_owned()),
            _ => {}
        }
    }

    // One instance owns the tray icon and clipboard listener. A second
    // instance forwards any links over WM_COPYDATA and exits.
    let name = wide("FlowDown.SingleInstance.v1");
    let mutex = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    let already_running = unsafe { !mutex.is_null() && GetLastError() == ERROR_ALREADY_EXISTS };
    if already_running {
        if minimized && links.is_empty() {
            return;
        }
        for _ in 0..40 {
            if ui::notify_existing(&links) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        return;
    }
    let _instance_mutex = mutex;

    let mut settings = Settings::load();
    // The registry is the source of truth for "start with Windows": the user
    // may have disabled it from Task Manager's Startup tab.
    settings.autostart = autostart::is_enabled();

    let code = ui::run(ui::Options {
        settings,
        minimized,
        links,
    });
    std::process::exit(code);
}

fn is_link(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ftp://")
        || lower.starts_with("magnet:?")
}
