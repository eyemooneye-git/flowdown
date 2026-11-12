//! Application settings, persisted as a tiny hand-written INI file in
//! `%LOCALAPPDATA%\FlowDown\settings.ini`.

use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub maximized: bool,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub download_dir: PathBuf,
    pub connections: usize,
    pub monitor_clipboard: bool,
    pub auto_start_downloads: bool,
    pub autostart: bool,
    pub close_to_tray: bool,
    pub start_minimized: bool,
    pub window: Option<WindowState>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            connections: 8,
            monitor_clipboard: true,
            auto_start_downloads: true,
            autostart: false,
            close_to_tray: true,
            start_minimized: false,
            window: None,
        }
    }
}

pub fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("FlowDown")
}

pub fn settings_path() -> PathBuf {
    app_data_dir().join("settings.ini")
}

pub fn cache_dir() -> PathBuf {
    app_data_dir().join("cache")
}

pub fn default_download_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join("Downloads").join("FlowDown")
}

impl Settings {
    pub fn load() -> Self {
        let mut s = Self::default();
        let Ok(text) = fs::read_to_string(settings_path()) else {
            let _ = fs::create_dir_all(&s.download_dir);
            return s;
        };
        let mut window = WindowState {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            maximized: false,
        };
        let mut have_window = false;
        for line in text.lines() {
            let line = line.trim_start_matches('\u{feff}').trim();
            if line.is_empty()
                || line.starts_with('#')
                || line.starts_with(';')
                || line.starts_with('[')
            {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "download_dir" if !value.is_empty() => s.download_dir = PathBuf::from(value),
                "connections" => {
                    if let Ok(n) = value.parse::<usize>() {
                        s.connections = n.clamp(1, 32);
                    }
                }
                "monitor_clipboard" => s.monitor_clipboard = parse_bool(value, true),
                "auto_start_downloads" => s.auto_start_downloads = parse_bool(value, true),
                "autostart" => s.autostart = parse_bool(value, false),
                "close_to_tray" => s.close_to_tray = parse_bool(value, true),
                "start_minimized" => s.start_minimized = parse_bool(value, false),
                "window_x" => {
                    if let Ok(v) = value.parse() {
                        window.x = v;
                        have_window = true;
                    }
                }
                "window_y" => {
                    if let Ok(v) = value.parse() {
                        window.y = v;
                        have_window = true;
                    }
                }
                "window_w" => {
                    if let Ok(v) = value.parse() {
                        window.w = v;
                        have_window = true;
                    }
                }
                "window_h" => {
                    if let Ok(v) = value.parse() {
                        window.h = v;
                        have_window = true;
                    }
                }
                "window_maximized" => window.maximized = parse_bool(value, false),
                _ => {}
            }
        }
        if have_window && window.w >= 500 && window.h >= 350 {
            s.window = Some(window);
        }
        let _ = fs::create_dir_all(&s.download_dir);
        s
    }

    pub fn save(&self) -> std::io::Result<()> {
        let dir = app_data_dir();
        fs::create_dir_all(&dir)?;
        fs::create_dir_all(&self.download_dir)?;
        let mut text = String::new();
        text.push_str("; FlowDown settings\n[flowdown]\n");
        text.push_str(&format!("download_dir={}\n", self.download_dir.display()));
        text.push_str(&format!("connections={}\n", self.connections.clamp(1, 32)));
        text.push_str(&format!(
            "monitor_clipboard={}\n",
            bool_text(self.monitor_clipboard)
        ));
        text.push_str(&format!(
            "auto_start_downloads={}\n",
            bool_text(self.auto_start_downloads)
        ));
        text.push_str(&format!("autostart={}\n", bool_text(self.autostart)));
        text.push_str(&format!(
            "close_to_tray={}\n",
            bool_text(self.close_to_tray)
        ));
        text.push_str(&format!(
            "start_minimized={}\n",
            bool_text(self.start_minimized)
        ));
        if let Some(w) = &self.window {
            text.push_str(&format!("window_x={}\n", w.x));
            text.push_str(&format!("window_y={}\n", w.y));
            text.push_str(&format!("window_w={}\n", w.w));
            text.push_str(&format!("window_h={}\n", w.h));
            text.push_str(&format!("window_maximized={}\n", bool_text(w.maximized)));
        }
        let path = settings_path();
        let tmp = path.with_extension("ini.tmp");
        fs::write(&tmp, &text)?;
        if fs::rename(&tmp, &path).is_ok() {
            return Ok(());
        }
        // Renaming over an existing file can fail on some filter drivers.
        // Fall back to a direct write.
        let _ = fs::remove_file(&tmp);
        fs::write(&path, text)
    }

    pub fn ensure_download_dir(&self) -> std::io::Result<()> {
        fs::create_dir_all(&self.download_dir)
    }
}

fn parse_bool(s: &str, default: bool) -> bool {
    match s.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => default,
    }
}

fn bool_text(v: bool) -> &'static str {
    if v {
        "1"
    } else {
        "0"
    }
}

pub fn ensure_dirs() {
    let _ = fs::create_dir_all(app_data_dir());
    let _ = fs::create_dir_all(cache_dir());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_parse() {
        assert!(parse_bool("1", false));
        assert!(parse_bool("TRUE", false));
        assert!(!parse_bool("0", true));
        assert!(!parse_bool("nope", false));
    }
}
