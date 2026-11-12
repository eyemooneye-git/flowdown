//! "Start with Windows" support through HKCU's Run key.

use std::path::Path;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ,
};

use crate::util::wide;

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "FlowDown";

fn command(exe: &Path) -> String {
    format!("\"{}\" --minimized", exe.display())
}

/// True when the Run value exists. We intentionally do not compare the full
/// command: if the user moves the exe, the setting is still visibly enabled
/// (and can be fixed by toggling it off/on).
pub fn is_enabled() -> bool {
    read_value().is_some()
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let cmd = command(&exe);
        write_value(&cmd)
    } else {
        delete_value()
    }
}

fn read_value() -> Option<String> {
    unsafe {
        let subkey = wide(RUN_KEY);
        let mut key: HKEY = std::ptr::null_mut();
        let rc = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        );
        if rc != ERROR_SUCCESS {
            return None;
        }
        let name = wide(VALUE_NAME);
        let mut ty = 0u32;
        let mut size = 0u32;
        let rc = RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut ty,
            std::ptr::null_mut(),
            &mut size,
        );
        if rc != ERROR_SUCCESS || size == 0 {
            RegCloseKey(key);
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let rc = RegQueryValueExW(
            key,
            name.as_ptr(),
            std::ptr::null(),
            &mut ty,
            buf.as_mut_ptr(),
            &mut size,
        );
        RegCloseKey(key);
        if rc != ERROR_SUCCESS || ty != REG_SZ {
            return None;
        }
        let u16_len = (size as usize / 2).min(buf.len() / 2);
        let u16s = std::slice::from_raw_parts(buf.as_ptr() as *const u16, u16_len);
        let end = u16s.iter().position(|&c| c == 0).unwrap_or(u16_len);
        Some(String::from_utf16_lossy(&u16s[..end]))
    }
}

fn write_value(cmd: &str) -> Result<(), String> {
    unsafe {
        let subkey = wide(RUN_KEY);
        let mut key: HKEY = std::ptr::null_mut();
        let mut disposition = 0u32;
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            0,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            &mut disposition,
        );
        if rc != ERROR_SUCCESS {
            return Err(format!("RegCreateKeyExW failed ({rc})"));
        }
        let name = wide(VALUE_NAME);
        let data: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes = data.len() * 2;
        let rc = RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_SZ,
            data.as_ptr() as *const u8,
            bytes as u32,
        );
        RegCloseKey(key);
        if rc != ERROR_SUCCESS {
            return Err(format!("RegSetValueExW failed ({rc})"));
        }
        Ok(())
    }
}

fn delete_value() -> Result<(), String> {
    unsafe {
        let subkey = wide(RUN_KEY);
        let mut key: HKEY = std::ptr::null_mut();
        let rc = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        );
        if rc != ERROR_SUCCESS {
            // Not having the key is equivalent to being disabled.
            return Ok(());
        }
        let name = wide(VALUE_NAME);
        let rc = RegDeleteValueW(key, name.as_ptr());
        RegCloseKey(key);
        if rc != ERROR_SUCCESS && rc != 2 {
            // 2 = ERROR_FILE_NOT_FOUND
            return Err(format!("RegDeleteValueW failed ({rc})"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn command_quotes_path() {
        let cmd = super::command(std::path::Path::new(
            r"C:\Program Files\FlowDown\flowdown.exe",
        ));
        assert_eq!(
            cmd,
            r#""C:\Program Files\FlowDown\flowdown.exe" --minimized"#
        );
    }
}
