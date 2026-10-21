//! Windows integration that needs explicit native semantics.
use crate::error::{AppError, Result};
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HWND, RECT},
    System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY_CURRENT_USER,
        KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    },
    UI::WindowsAndMessaging::{
        GetClientRect, GetWindowRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    },
};

pub fn set_autostart(enabled: bool) -> Result<()> {
    let failed = || AppError::new("autostart_error", "无法更新开机启动设置，请检查系统权限。");
    let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run\0"
        .encode_utf16()
        .collect();
    let name: Vec<u16> = "Qwen Desk\0".encode_utf16().collect();
    // Windows command-line parsing requires quotes around executable paths with spaces.
    let mut command = vec![b'"' as u16];
    command.extend(
        std::env::current_exe()
            .map_err(|_| failed())?
            .as_os_str()
            .encode_wide(),
    );
    command.extend("\" --autostart\0".encode_utf16());
    unsafe {
        let mut key = std::ptr::null_mut();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            subkey.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            std::ptr::null(),
            &mut key,
            std::ptr::null_mut(),
        ) != ERROR_SUCCESS
        {
            return Err(failed());
        }
        let status = if enabled {
            RegSetValueExW(
                key,
                name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(key, name.as_ptr())
        };
        RegCloseKey(key);
        if status != ERROR_SUCCESS && !(status == ERROR_FILE_NOT_FOUND && !enabled) {
            return Err(failed());
        }
    }
    Ok(())
}

/// Correct the actual client rectangle after the framework restores a frameless
/// window. This avoids cumulative caption-height growth on Windows 11.
pub fn restore_client_size(handle: isize, width: u32, height: u32) -> Result<()> {
    unsafe {
        let window = handle as HWND;
        let mut client: RECT = std::mem::zeroed();
        let mut outer: RECT = std::mem::zeroed();
        if GetClientRect(window, &mut client) == 0 || GetWindowRect(window, &mut outer) == 0 {
            return Err(AppError::new("window_error", "无法恢复窗口大小。"));
        }
        let outer_width = outer.right - outer.left + width as i32 - (client.right - client.left);
        let outer_height = outer.bottom - outer.top + height as i32 - (client.bottom - client.top);
        if SetWindowPos(
            window,
            std::ptr::null_mut(),
            0,
            0,
            outer_width,
            outer_height,
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        ) == 0
        {
            return Err(AppError::new("window_error", "无法恢复窗口大小。"));
        }
    }
    Ok(())
}
