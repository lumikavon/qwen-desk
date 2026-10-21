//! WebView2 cannot query or close DevTools through wry on Windows.
//! Only debug builds compile this module. Close only a visible DevTools window
//! owned by a descendant of this application, never another browser's window.
use crate::error::{AppError, Result};
use std::collections::HashSet;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HWND, INVALID_HANDLE_VALUE, LPARAM},
    System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
        WM_CLOSE,
    },
};

struct Search {
    descendants: HashSet<u32>,
    closed: bool,
}
unsafe extern "system" fn inspect_window(window: HWND, context: LPARAM) -> i32 {
    let search = &mut *(context as *mut Search);
    let mut process = 0;
    GetWindowThreadProcessId(window, &mut process);
    if !search.descendants.contains(&process) || IsWindowVisible(window) == 0 {
        return 1;
    }
    let mut title = [0u16; 512];
    let size = GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32);
    if size > 0 && String::from_utf16_lossy(&title[..size as usize]).starts_with("DevTools - ") {
        if PostMessageW(window, WM_CLOSE, 0, 0) != 0 {
            search.closed = true;
        }
    }
    1
}

pub fn close_if_open() -> Result<bool> {
    unsafe {
        let handle = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if handle == INVALID_HANDLE_VALUE {
            return Err(AppError::new("devtools_error", "无法检查开发工具窗口。"));
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut processes = vec![];
        if Process32FirstW(handle, &mut entry) != 0 {
            loop {
                processes.push((entry.th32ProcessID, entry.th32ParentProcessID));
                if Process32NextW(handle, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(handle);
        let mut descendants = HashSet::from([std::process::id()]);
        loop {
            let before = descendants.len();
            for (child, parent) in &processes {
                if descendants.contains(parent) {
                    descendants.insert(*child);
                }
            }
            if descendants.len() == before {
                break;
            }
        }
        descendants.remove(&std::process::id());
        let mut search = Search {
            descendants,
            closed: false,
        };
        EnumWindows(Some(inspect_window), &mut search as *mut Search as LPARAM);
        Ok(search.closed)
    }
}
