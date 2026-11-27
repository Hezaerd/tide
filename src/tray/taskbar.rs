use std::{
    sync::{OnceLock, mpsc},
    thread::{self, JoinHandle},
};

use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE, SetThreadDpiAwarenessContext},
            WindowsAndMessaging::{
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
                PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW, WM_APP,
                WM_CLOSE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_SETTINGCHANGE, WNDCLASSW,
                WS_EX_TOOLWINDOW, WS_POPUP,
            },
        },
    },
    core::{Error, PCWSTR, w},
};

const CLASS: PCWSTR = w!("TideTaskbarWatcher");
const REFRESH: u32 = WM_APP;
static WINDOW_CLASS: OnceLock<windows::core::Result<u32>> = OnceLock::new();

pub struct TaskbarWatcher {
    window: isize,
    worker: Option<JoinHandle<()>>,
}

impl TaskbarWatcher {
    pub fn start(
        mut refresh: impl FnMut() -> bool + Send + 'static,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (ready, startup) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("tide-taskbar".into())
            .spawn(move || {
                let window = create_window();
                let sent = ready.send(window.clone().map(|hwnd| hwnd.0 as isize));
                let Ok(window) = window else { return };

                // SAFETY: This thread created window and owns its message queue. msg is initialized
                // and dispatched only after GetMessageW reports success.
                unsafe {
                    if sent.is_ok() && refresh() {
                        let mut msg = MSG::default();
                        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                            if msg.hwnd == window && msg.message == REFRESH {
                                if !refresh() {
                                    break;
                                }
                            } else {
                                DispatchMessageW(&msg);
                            }
                        }
                    }
                    let _ = DestroyWindow(window);
                }
            })?;
        let window = startup.recv()??;
        Ok(Self {
            window,
            worker: Some(worker),
        })
    }
}

impl Drop for TaskbarWatcher {
    fn drop(&mut self) {
        // SAFETY: This sends a value-only message to the window created by the worker.
        let _ = unsafe {
            PostMessageW(
                Some(HWND(self.window as *mut _)),
                WM_CLOSE,
                WPARAM(0),
                LPARAM(0),
            )
        };
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn create_window() -> windows::core::Result<HWND> {
    // SAFETY: The module and class belong to this executable. Class and message names are
    // NUL-terminated, and CreateWindowExW gets no caller-owned data.
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None)?.into();
        WINDOW_CLASS
            .get_or_init(|| {
                let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
                if taskbar_created == 0 {
                    return Err(Error::from_thread());
                }
                let class = WNDCLASSW {
                    hInstance: instance,
                    lpszClassName: CLASS,
                    lpfnWndProc: Some(window_proc),
                    ..Default::default()
                };
                if RegisterClassW(&class) == 0 {
                    return Err(Error::from_thread());
                }
                Ok(taskbar_created)
            })
            .clone()?;

        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE);
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CLASS,
            None,
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance),
            None,
        )
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Defer refresh until after the broadcast so tray work never blocks the sender.
    // SAFETY: The OS supplied the window and message arguments. The private message carries no data.
    unsafe {
        match message {
            WM_CLOSE => PostQuitMessage(0),
            message
                if matches!(message, WM_SETTINGCHANGE | WM_DISPLAYCHANGE | WM_DPICHANGED)
                    || matches!(WINDOW_CLASS.get(), Some(Ok(taskbar_created)) if message == *taskbar_created) =>
            {
                let _ = PostMessageW(Some(window), REFRESH, WPARAM(0), LPARAM(0));
            }
            _ => return DefWindowProcW(window, message, wparam, lparam),
        }
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use windows::Win32::{
        Foundation::RECT,
        UI::WindowsAndMessaging::{
            FindWindowW, IsWindow, IsWindowVisible, SMTO_ABORTIFHUNG, SendMessageTimeoutW,
        },
    };

    #[test]
    fn hidden_top_level_window_refreshes_on_taskbar_messages_and_closes_on_drop() {
        let (notify, refreshed) = mpsc::channel();
        let watcher = TaskbarWatcher::start(move || notify.send(()).is_ok()).unwrap();
        let timeout = Duration::from_secs(5);
        refreshed.recv_timeout(timeout).unwrap();

        let dpi_bounds = RECT::default();
        // SAFETY: CLASS and the theme setting name are NUL-terminated. The setting string and DPI
        // bounds live through the synchronous send. The other messages carry no pointers.
        let window = unsafe {
            let window = FindWindowW(CLASS, None).unwrap();
            assert_eq!(window.0 as isize, watcher.window);
            assert!(!IsWindowVisible(window).as_bool());
            for (message, lparam) in [
                (
                    WM_SETTINGCHANGE,
                    LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
                ),
                (WM_DISPLAYCHANGE, LPARAM(0)),
                (WM_DPICHANGED, LPARAM(&dpi_bounds as *const RECT as isize)),
                (*WINDOW_CLASS.get().unwrap().as_ref().unwrap(), LPARAM(0)),
            ] {
                let mut result = 0;
                let sent = SendMessageTimeoutW(
                    window,
                    message,
                    WPARAM(0),
                    lparam,
                    SMTO_ABORTIFHUNG,
                    5000,
                    Some(&mut result),
                );
                assert_ne!(sent.0, 0, "message {message:#x}: {}", Error::from_thread());
                refreshed.recv_timeout(timeout).unwrap();
            }
            window
        };

        drop(watcher);
        // SAFETY: IsWindow accepts a stale handle and checks if the window still exists.
        assert!(!unsafe { IsWindow(Some(window)) }.as_bool());
    }
}
