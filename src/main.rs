#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod audio;
mod icon;
mod theme;
mod tray;
mod ui;

use rune::{App, AppSpec, WindowOptions, WindowsWindowOptions};
use ui::State;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let headless = cfg!(debug_assertions)
        && std::env::args().skip(1).any(|arg| {
            matches!(
                arg.as_str(),
                "--snap" | "--tree" | "--script" | "--list-scenes"
            )
        });
    if !headless && !rune::single_instance("com.hezaerd.tide") {
        return Ok(());
    }

    let spec = AppSpec {
        scenes: ui::SCENES,
        on_key: Some(State::key),
        ..AppSpec::new("Tide", (ui::WIDTH, 351.0), State::new, ui::view)
    };
    let app = App::new(spec)
        .window_options(WindowOptions {
            transparent: true,
            borderless: true,
            always_on_top: true,
            resizable: false,
            min_size: Some((0.0, 0.0)),
            ..WindowOptions::default()
        })
        .windows_window(
            WindowsWindowOptions::default()
                .tool_window(true)
                .undecorated_shadow(true),
        )
        .fit_content_height()
        .on_focus(State::focus_changed)
        .on_close(State::closed);

    if headless {
        app.try_run()?;
    } else {
        let app = app.windowless();
        let handle = app.handle();
        let inbox = handle.clone();
        let (command, peak) = audio::start(move |update| {
            inbox.update_if(move |state, tx| state.apply(update, tx));
        })?;
        let (_taskbar_watcher, tray) = tray::install(handle.clone())?;
        let weak_tray = std::sync::Arc::downgrade(&tray);
        app.init(move || State::connected(command, peak, handle, weak_tray))
            .try_run()?;
    }
    Ok(())
}
