# Tide

Tide is a small volume mixer for Windows. Click its tray icon to set the volume of each app that is playing sound, without opening Settings.

<img src="docs/tide.png" alt="Tide's mixer panel in dark mode" width="320">

- One slider per app, plus master volume for the current output device.
- A live level meter in each slider.
- Click an app's icon to mute it. Scroll over a row to adjust it.
- Follows the Windows light or dark theme.
- Stays idle while the panel is closed.

## Build

Tide needs Windows 10 or 11 and a stable Rust toolchain.

```sh
cargo build --release
```

The executable is `target/release/tide.exe`.

Windows 11 hides new tray icons. To keep Tide on the taskbar, open **Settings > Personalization > Taskbar > Other system tray icons** and turn on Tide.

## License

[MIT](LICENSE)
