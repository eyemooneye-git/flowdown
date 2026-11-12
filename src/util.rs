//! Shared helpers: Win32 string conversion, clipboard access, formatting and
//! link / filename parsing.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use url::Url;

use crate::tr;

/// `MAKEINTRESOURCEW(1)`: the icon resource declared in `app.rc`.
pub const APP_ICON_ID: *const u16 = std::ptr::without_provenance(1);

/// NUL terminated UTF-16 buffer for the `*W` Win32 APIs.
pub fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

pub fn utf16_lossy(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn format_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if v >= 100.0 {
        format!("{v:.0} {}", UNITS[u])
    } else if v >= 10.0 {
        format!("{v:.1} {}", UNITS[u])
    } else {
        format!("{v:.2} {}", UNITS[u])
    }
}

pub fn format_speed(bytes_per_sec: u64) -> String {
    format!("{}/s", format_size(bytes_per_sec))
}

pub fn format_eta(seconds: Option<u64>) -> String {
    let Some(secs) = seconds else {
        return "—".to_owned();
    };
    if secs == 0 {
        return "—".to_owned();
    }
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    if h > 0 {
        tr!(format!("{h}h {m:02}m"), format!("{h} 小时 {m:02} 分"))
    } else if m > 0 {
        tr!(format!("{m}m {s:02}s"), format!("{m} 分 {s:02} 秒"))
    } else {
        tr!(format!("{s}s"), format!("{s} 秒"))
    }
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extract every HTTP/HTTPS/FTP/magnet link from arbitrarily formatted text.
/// The clipboard often holds a whole paragraph, so this is deliberately a
/// tokenizer rather than a single URL parser.
pub fn extract_links(text: &str) -> Vec<String> {
    let text = text.replace("&amp;", "&");
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\''
                        | '<'
                        | '>'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | ','
                        | ';'
                        | '.'
                        | '，'
                        | '。'
                        | '；'
                        | '、'
                )
        });
        if token.is_empty() {
            continue;
        }
        let lower = token.to_ascii_lowercase();
        let ok = if lower.starts_with("magnet:?") {
            Url::parse(token).is_ok() || token.contains("xt=urn:btih:")
        } else if lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("ftp://")
        {
            Url::parse(token)
                .map(|u| u.host().is_some())
                .unwrap_or(false)
        } else {
            false
        };
        if ok && seen.insert(token.to_owned()) {
            out.push(token.to_owned());
        }
    }
    out
}

pub fn link_kind(url: &str) -> Option<LinkKind> {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("magnet:?") {
        Some(LinkKind::Torrent)
    } else if lower.starts_with("ftp://") {
        Some(LinkKind::Ftp)
    } else if lower.starts_with("http://") || lower.starts_with("https://") {
        Some(LinkKind::Http)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LinkKind {
    Http,
    Ftp,
    Torrent,
}

/// Strip credentials from a URL before showing it in the UI / logs.
pub fn display_url(url: &str) -> String {
    if let Ok(mut parsed) = Url::parse(url) {
        if !parsed.username().is_empty() {
            let _ = parsed.set_username("***");
        }
        if parsed.password().is_some() {
            let _ = parsed.set_password(Some("***"));
        }
        return parsed.into();
    }
    url.to_owned()
}

pub fn filename_from_url(url: &str) -> String {
    let parsed = Url::parse(url).ok();
    let raw = parsed
        .as_ref()
        .and_then(|u| u.path_segments())
        .and_then(|mut it| it.next_back())
        .filter(|s| !s.is_empty())
        .unwrap_or("download");
    let decoded = percent_decode(raw);
    sanitize_filename(&decoded)
}

pub fn magnet_name(url: &str) -> Option<String> {
    let parsed = Url::parse(url).ok()?;
    for (k, v) in parsed.query_pairs() {
        if k == "dn" {
            let name = sanitize_filename(v.trim());
            if !name.is_empty() && name != "download" {
                return Some(name);
            }
        }
    }
    None
}

pub fn filename_from_disposition(value: &str) -> Option<String> {
    // RFC 5987 form first: filename*=UTF-8''foo%20bar.zip
    let lower = value.to_ascii_lowercase();
    if let Some(pos) = lower.find("filename*=") {
        let rest = &value[pos + "filename*=".len()..];
        let rest = rest.trim_start_matches([' ', '\t']);
        let rest = rest.split(';').next().unwrap_or(rest).trim();
        let encoded = if let Some((_, tail)) = rest.split_once("''") {
            tail
        } else if let Some((_, tail)) = rest.split_once('\'') {
            tail
        } else {
            rest
        };
        let decoded = percent_decode(encoded.trim_matches('"'));
        let name = sanitize_filename(&decoded);
        if !name.is_empty() {
            return Some(name);
        }
    }
    if let Some(pos) = lower.find("filename=") {
        let rest = &value[pos + "filename=".len()..];
        let rest = rest.trim_start();
        let rest = if let Some(inner) = rest.strip_prefix('"') {
            inner.split('"').next().unwrap_or(inner)
        } else {
            rest.split(';').next().unwrap_or(rest).trim()
        };
        let name = sanitize_filename(&percent_decode(rest));
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

/// Make an arbitrary remote name safe for NTFS and for `explorer.exe`.
pub fn sanitize_filename(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    let mut out = out.trim().trim_matches('.').trim().to_owned();
    if out.is_empty() {
        out = "download".to_owned();
    }
    // Device names are invalid even with an extension.
    let stem = out
        .split('.')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if matches!(
        stem.as_str(),
        "con"
            | "prn"
            | "aux"
            | "nul"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "com5"
            | "com6"
            | "com7"
            | "com8"
            | "com9"
            | "lpt1"
            | "lpt2"
            | "lpt3"
            | "lpt4"
            | "lpt5"
            | "lpt6"
            | "lpt7"
            | "lpt8"
            | "lpt9"
    ) {
        out = format!("_{out}");
    }
    if out.chars().count() > 180 {
        let mut truncated = String::new();
        for c in out.chars().take(176) {
            truncated.push(c);
        }
        out = truncated.trim_end().to_owned();
    }
    out
}

/// Pick `name`, `name (1)`, `name (2)`… avoiding existing files and paths
/// already reserved by other rows in the download list.
pub fn unique_path(dir: &Path, name: &str, reserved: &HashSet<PathBuf>) -> PathBuf {
    let safe = sanitize_filename(name);
    let path = dir.join(&safe);
    if !path.exists() && !reserved.contains(&path) {
        return path;
    }
    let (stem, ext) = match safe.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => {
            (stem.to_owned(), format!(".{ext}"))
        }
        _ => (safe, String::new()),
    };
    for n in 1..10_000u32 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() && !reserved.contains(&candidate) {
            return candidate;
        }
    }
    dir.join(format!("{stem} (copy){ext}"))
}

pub fn open_folder(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}

pub fn open_path(path: &Path) {
    let _ = std::process::Command::new("explorer.exe").arg(path).spawn();
}

pub fn reveal_path(path: &Path) {
    if path.is_dir() {
        open_folder(path);
    } else if let Some(parent) = path.parent() {
        // `/select,` and the path must be separate arguments; Explorer handles
        // the comma itself.
        let _ = std::process::Command::new("explorer.exe")
            .arg("/select,")
            .arg(path)
            .spawn();
        let _ = parent;
    }
}

/// Read `CF_UNICODETEXT` from the clipboard, retrying if another process is
/// momentarily holding it.
pub fn clipboard_text() -> Option<String> {
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalUnlock};

    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        let mut opened = 0;
        for _ in 0..12 {
            if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
                return None;
            }
            opened = OpenClipboard(std::ptr::null_mut());
            if opened != 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(12));
        }
        if opened == 0 {
            return None;
        }
        let result = (|| {
            let hmem = GetClipboardData(CF_UNICODETEXT);
            if hmem.is_null() {
                return None;
            }
            let ptr = GlobalLock(hmem) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while len < 8 * 1024 * 1024 && *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            GlobalUnlock(hmem);
            Some(s)
        })();
        CloseClipboard();
        result
    }
}

pub fn set_clipboard_text(text: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };

    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        let mut opened = 0;
        for _ in 0..12 {
            opened = OpenClipboard(std::ptr::null_mut());
            if opened != 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(12));
        }
        if opened == 0 {
            return Err(tr!("could not open the clipboard", "无法打开剪贴板").to_owned());
        }
        let result = (|| -> Result<(), String> {
            if EmptyClipboard() == 0 {
                return Err(tr!("could not clear the clipboard", "无法清空剪贴板").to_owned());
            }
            let wide_text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let hmem = GlobalAlloc(GMEM_MOVEABLE, wide_text.len() * 2);
            if hmem.is_null() {
                return Err(tr!("out of memory", "内存不足").to_owned());
            }
            let ptr = GlobalLock(hmem);
            if ptr.is_null() {
                GlobalFree(hmem);
                return Err(tr!("out of memory", "内存不足").to_owned());
            }
            std::ptr::copy_nonoverlapping(wide_text.as_ptr(), ptr as *mut u16, wide_text.len());
            GlobalUnlock(hmem);
            if SetClipboardData(CF_UNICODETEXT, hmem).is_null() {
                GlobalFree(hmem);
                return Err(tr!("could not set clipboard data", "无法设置剪贴板数据").to_owned());
            }
            Ok(())
        })();
        CloseClipboard();
        result
    }
}

/// A tiny native folder picker, matching the rest of the UI.
pub fn pick_folder(owner: windows_sys::Win32::Foundation::HWND, title: &str) -> Option<String> {
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
    };
    use windows_sys::Win32::UI::Shell::{
        SHBrowseForFolderW, SHGetPathFromIDListW, BIF_EDITBOX, BIF_NEWDIALOGSTYLE,
        BIF_RETURNONLYFSDIRS, BROWSEINFOW,
    };

    unsafe {
        let hr = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        let com_ready = hr >= 0;
        let title_w = wide(title);
        let mut display = [0u16; 512];
        let bi = BROWSEINFOW {
            hwndOwner: owner,
            pidlRoot: std::ptr::null_mut(),
            pszDisplayName: display.as_mut_ptr(),
            lpszTitle: title_w.as_ptr(),
            ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE | BIF_EDITBOX,
            lpfn: None,
            lParam: 0,
            iImage: 0,
        };
        let pidl = SHBrowseForFolderW(&bi);
        let mut picked = None;
        if !pidl.is_null() {
            let mut path = [0u16; 1024];
            if SHGetPathFromIDListW(pidl, path.as_mut_ptr()) != 0 {
                let s = utf16_lossy(&path);
                if !s.is_empty() {
                    picked = Some(s);
                }
            }
            CoTaskMemFree(pidl as *const core::ffi::c_void);
        }
        if com_ready {
            CoUninitialize();
        }
        picked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_links_from_text() {
        let text = "see <https://example.com/a.zip?x=1>, magnet:?xt=urn:btih:abc&dn=hello \
                    ftp://u:p@example.com/file.iso and https://example.com/b.zip.";
        let links = extract_links(text);
        assert_eq!(links.len(), 4);
        assert_eq!(links[0], "https://example.com/a.zip?x=1");
        assert_eq!(links[1], "magnet:?xt=urn:btih:abc&dn=hello");
        assert_eq!(links[2], "ftp://u:p@example.com/file.iso");
        assert_eq!(links[3], "https://example.com/b.zip");
    }

    #[test]
    fn sanitizes_names() {
        assert_eq!(sanitize_filename("a<b>c:d?.zip"), "a_b_c_d_.zip");
        assert_eq!(sanitize_filename("con.txt"), "_con.txt");
        assert_eq!(sanitize_filename("  name.  "), "name");
    }

    #[test]
    fn parses_disposition() {
        assert_eq!(
            filename_from_disposition("attachment; filename=\"hello world.zip\""),
            Some("hello world.zip".to_owned())
        );
        assert_eq!(
            filename_from_disposition("attachment; filename*=UTF-8''hello%20world.zip"),
            Some("hello world.zip".to_owned())
        );
    }

    #[test]
    fn magnet_display_name() {
        assert_eq!(
            magnet_name("magnet:?xt=urn:btih:abc&dn=Ubuntu%2024.04"),
            Some("Ubuntu 24.04".to_owned())
        );
    }
}
