//! Listen for Windows logoff/shutdown and suspend even though the tray has no
//! visible window. Don't start status-check subprocesses while Windows is
//! tearing down its desktop and DLL loader (error 0xc0000142).

use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::{
    Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, PBT_APMRESUMEAUTOMATIC,
        PBT_APMRESUMESUSPEND, PBT_APMSUSPEND, PostQuitMessage, RegisterClassW, UnregisterClassW,
        WM_ENDSESSION, WM_POWERBROADCAST, WM_QUERYENDSESSION, WNDCLASSW,
    },
};

static LAUNCHES_PAUSED: AtomicBool = AtomicBool::new(false);

pub fn launches_paused() -> bool {
    LAUNCHES_PAUSED.load(Ordering::SeqCst)
}

fn session_update(message: u32, wparam: WPARAM) -> Option<bool> {
    match message {
        WM_QUERYENDSESSION => Some(true),
        // Windows sends wParam=0 if shutdown was cancelled; resume normal
        // operation rather than permanently disabling the tray.
        WM_ENDSESSION => Some(wparam != 0),
        WM_POWERBROADCAST if wparam == PBT_APMSUSPEND as usize => Some(true),
        WM_POWERBROADCAST
            if wparam == PBT_APMRESUMEAUTOMATIC as usize
                || wparam == PBT_APMRESUMESUSPEND as usize =>
        {
            Some(false)
        }
        _ => None,
    }
}

unsafe extern "system" fn session_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(ending) = session_update(message, wparam) {
        LAUNCHES_PAUSED.store(ending, Ordering::SeqCst);
        if message == WM_QUERYENDSESSION || message == WM_POWERBROADCAST {
            return 1; // Allow logoff/shutdown to proceed without blocking it.
        }
        if ending {
            unsafe { PostQuitMessage(0) };
        }
        return 0;
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

pub struct SessionNotifications {
    window: HWND,
    class_name: Vec<u16>,
    instance: HINSTANCE,
}

impl SessionNotifications {
    pub fn new() -> Result<Self, String> {
        let class_name = "ZodexTrayShutdownNotifications\0"
            .encode_utf16()
            .collect::<Vec<_>>();
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        if instance.is_null() {
            return Err(format!(
                "Cannot obtain Windows module handle: {}",
                std::io::Error::last_os_error()
            ));
        }
        let class = WNDCLASSW {
            lpfnWndProc: Some(session_window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..Default::default()
        };
        if unsafe { RegisterClassW(&class) } == 0 {
            return Err(format!(
                "Cannot register Windows shutdown receiver: {}",
                std::io::Error::last_os_error()
            ));
        }
        // A hidden top-level window receives WM_QUERYENDSESSION/WM_ENDSESSION.
        // Message-only windows (HWND_MESSAGE) do not receive these broadcasts.
        let window = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            )
        };
        if window.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe { UnregisterClassW(class_name.as_ptr(), instance) };
            return Err(format!("Cannot create Windows shutdown receiver: {error}"));
        }
        LAUNCHES_PAUSED.store(false, Ordering::SeqCst);
        Ok(Self {
            window,
            class_name,
            instance,
        })
    }
}

impl Drop for SessionNotifications {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window);
            UnregisterClassW(self.class_name.as_ptr(), self.instance);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_notification_stops_launching_and_cancel_resumes() {
        assert_eq!(session_update(WM_QUERYENDSESSION, 0), Some(true));
        assert_eq!(session_update(WM_ENDSESSION, 1), Some(true));
        assert_eq!(session_update(WM_ENDSESSION, 0), Some(false));
        assert_eq!(
            session_update(WM_POWERBROADCAST, PBT_APMSUSPEND as usize),
            Some(true)
        );
        assert_eq!(
            session_update(WM_POWERBROADCAST, PBT_APMRESUMEAUTOMATIC as usize),
            Some(false)
        );
        assert_eq!(
            session_update(WM_POWERBROADCAST, PBT_APMRESUMESUSPEND as usize),
            Some(false)
        );
        assert_eq!(session_update(0, 0), None);
    }
}
