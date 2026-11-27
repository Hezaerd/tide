use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use rune::{AppHandle, Tray, TrayError, TrayEvent, TrayIcon, TrayItem, TraySpec};
use windows::Win32::{
    Foundation::{POINT, RECT},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint},
    UI::{
        HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        WindowsAndMessaging::GetCursorPos,
    },
};
use winshell::{Autostart, Mark, Taskbar};

use crate::ui::State;

const AUTOSTART: Autostart = Autostart::new("Tide");
const MARKS: &[Mark] = &[
    Mark {
        px: 16,
        black: include_bytes!("../assets/wave-black-16.png"),
        white: include_bytes!("../assets/wave-white-16.png"),
    },
    Mark {
        px: 24,
        black: include_bytes!("../assets/wave-black-24.png"),
        white: include_bytes!("../assets/wave-white-24.png"),
    },
    Mark {
        px: 32,
        black: include_bytes!("../assets/wave-black-32.png"),
        white: include_bytes!("../assets/wave-white-32.png"),
    },
    Mark {
        px: 64,
        black: include_bytes!("../assets/wave-black-64.png"),
        white: include_bytes!("../assets/wave-white-64.png"),
    },
];

fn mark((size, bytes): (u32, &[u8])) -> TrayIcon {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
        .expect("embedded Tide tray mark is a PNG")
        .into_rgba8();
    TrayIcon::rgba(size, size, image.into_raw())
}

fn menu() -> Vec<TrayItem> {
    vec![
        TrayItem::check("autostart", "Start with Windows", AUTOSTART.enabled()),
        TrayItem::action("quit", "Quit"),
    ]
}

pub fn install(handle: AppHandle<State>) -> Result<Arc<Tray>, TrayError> {
    let mut current = Taskbar::current().mark(MARKS);
    let slot = Arc::new(OnceLock::<std::sync::Weak<Tray>>::new());
    let callback_tray = slot.clone();
    let tray = Arc::new(Tray::new(
        TraySpec::new("com.hezaerd.tide", "Tide", mark(current))
            .tooltip("Tide")
            .menu(menu()),
        move |event| match event {
            TrayEvent::Activate { .. } => {
                let time = Instant::now();
                let placement = cursor_monitor();
                handle.update(move |s, tx| s.toggle(placement, time, tx));
            }
            TrayEvent::Menu(id) if id == "autostart" => {
                if let Err(error) = AUTOSTART.set(!AUTOSTART.enabled()) {
                    eprintln!("Tide could not update Start with Windows: {error}");
                }
                if let Some(tray) = callback_tray.get().and_then(std::sync::Weak::upgrade) {
                    tray.set_menu(menu());
                }
            }
            TrayEvent::Menu(id) if id == "quit" => handle.quit(),
            _ => {}
        },
    )?);
    let _ = slot.set(Arc::downgrade(&tray));
    let refresh = Arc::downgrade(&tray);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(2));
            let Some(tray) = refresh.upgrade() else { break };
            let next = Taskbar::current().mark(MARKS);
            if next != current && tray.set_icon(mark(next)).is_ok() {
                current = next;
            }
        }
    });
    Ok(tray)
}

#[derive(Clone, Copy, Debug)]
pub struct Placement {
    work: RECT,
    scale: f32,
    right: bool,
    bottom: bool,
}

impl Placement {
    fn new(monitor: RECT, work: RECT, cursor: POINT, scale: f32) -> Self {
        let insets = [
            monitor.bottom - work.bottom,
            monitor.right - work.right,
            work.top - monitor.top,
            work.left - monitor.left,
        ];
        let edge = (0..4)
            .max_by_key(|&edge| (insets[edge], std::cmp::Reverse(edge)))
            .unwrap();
        let right = match edge {
            1 => true,
            3 => false,
            _ => cursor.x >= work.left + (work.right - work.left) / 2,
        };
        let bottom = match edge {
            0 => true,
            2 => false,
            _ => cursor.y >= work.top + (work.bottom - work.top) / 2,
        };
        Self {
            work,
            scale,
            right,
            bottom,
        }
    }

    pub fn max_height(self) -> f32 {
        ((self.work.bottom - self.work.top) as f32 / self.scale - 24.0).floor()
    }

    pub fn position(self, width: f32, height: f32) -> (i32, i32) {
        let px = |logical: f32| (logical * self.scale).round() as i32;
        let margin = px(12.0);
        let x = if self.right {
            self.work.right - margin - px(width)
        } else {
            self.work.left + margin
        };
        let y = if self.bottom {
            self.work.bottom - margin - px(height)
        } else {
            self.work.top + margin
        };
        (x.max(self.work.left), y.max(self.work.top))
    }
}

fn cursor() -> POINT {
    let mut point = POINT::default();
    // SAFETY: point is writable for the call.
    let _ = unsafe { GetCursorPos(&mut point) };
    point
}

pub fn pointer_on_icon(tray: &Tray) -> Option<bool> {
    let [x, y, width, height] = tray.rect()?;
    let point = cursor();
    Some(point.x >= x && point.x < x + width && point.y >= y && point.y < y + height)
}

fn cursor_monitor() -> Option<Placement> {
    let cursor = cursor();
    // SAFETY: this monitor is selected by a physical cursor point and all out pointers are valid.
    unsafe {
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return None;
        }
        let (mut x, mut y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        Some(Placement::new(
            info.rcMonitor,
            info.rcWork,
            cursor,
            x.max(96) as f32 / 96.0,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_inside_the_work_area_at_the_taskbar_edge_and_cursor_corner() {
        let monitor = RECT {
            left: -1920,
            top: -120,
            right: 0,
            bottom: 960,
        };
        let rect = |left, top, right, bottom| RECT {
            left,
            top,
            right,
            bottom,
        };
        for (work, cursor, expected) in [
            (
                rect(-1920, -120, 0, 900),
                POINT { x: -100, y: 935 },
                (-498, 582),
            ),
            (
                rect(-1920, -60, 0, 960),
                POINT { x: -1800, y: -90 },
                (-1902, -42),
            ),
            (
                rect(-1860, -120, 0, 960),
                POINT { x: -1900, y: 800 },
                (-1842, 642),
            ),
            (
                rect(-1920, -120, -60, 960),
                POINT { x: -20, y: -100 },
                (-558, -102),
            ),
        ] {
            let placement = Placement::new(monitor, work, cursor, 1.5);
            assert_eq!(placement.position(320.0, 200.0), expected);
            assert_eq!(
                placement.max_height(),
                ((work.bottom - work.top) as f32 / 1.5 - 24.0).floor()
            );
        }
    }
}
