//! "Start with Windows": a value under HKCU\...\CurrentVersion\Run.

use std::ptr::null_mut;
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE_NAME: &str = "gitstat";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn open_run_key(access: u32) -> Option<HKEY> {
    let mut key: HKEY = null_mut();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(RUN_KEY).as_ptr(),
            0,
            access,
            &mut key,
        )
    };
    (status == ERROR_SUCCESS).then_some(key)
}

fn command_line() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\"", exe.display()))
}

/// True when the Run entry exists and points at this executable.
pub fn is_enabled() -> bool {
    let (Some(key), Some(expected)) = (open_run_key(KEY_QUERY_VALUE), command_line()) else {
        return false;
    };
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let status = unsafe {
        RegQueryValueExW(
            key,
            wide(VALUE_NAME).as_ptr(),
            null_mut(),
            null_mut(),
            buf.as_mut_ptr().cast(),
            &mut len,
        )
    };
    unsafe { RegCloseKey(key) };
    if status != ERROR_SUCCESS {
        return false;
    }
    let value = String::from_utf16_lossy(&buf[..(len as usize / 2)]);
    value.trim_end_matches('\0').eq_ignore_ascii_case(&expected)
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let key = open_run_key(KEY_SET_VALUE).ok_or("can't open the Run registry key")?;
    let status = if enabled {
        let value = wide(&command_line().ok_or("can't find gitstat.exe")?);
        unsafe {
            RegSetValueExW(
                key,
                wide(VALUE_NAME).as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                (value.len() * 2) as u32,
            )
        }
    } else {
        unsafe { RegDeleteValueW(key, wide(VALUE_NAME).as_ptr()) }
    };
    unsafe { RegCloseKey(key) };
    if status == ERROR_SUCCESS || !enabled {
        Ok(())
    } else {
        Err(format!("registry error {status}"))
    }
}
