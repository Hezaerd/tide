# Tide

Tide is a Windows tray volume mixer built with Rune. Clicking the tray icon opens a pop-up with master volume and one row per app that plays sound. It is open source under MIT, so keep README.md short and written for people who might use Tide.

## Layout

- `src/audio.rs` is the only module that calls WASAPI. It runs on its own COM thread, takes `Command`s over a channel, and publishes `Snapshot`s to the UI.
- `src/ui.rs` holds the pop-up state and view. `src/tray.rs` owns the tray icon and its menu. `src/theme.rs` holds the light and dark tokens. `src/icon.rs` extracts app icons and accent colours. `winshell` manages "Start with Windows" and selects the taskbar mark for `src/tray.rs`.
- `assets/` holds the tray marks. Regenerate the PNGs with `scripts/generate-wave.py` rather than editing them by hand.
- `installer/tide.iss` defines the per-user Windows installer.
- Rune is pinned to one git rev in `Cargo.toml`, together with the `[patch.crates-io]` block. Change all of them together.

## Rules

- Never call WASAPI from the UI thread. Listing sessions sometimes takes over 200 ms.
- While the pop-up is closed, the audio thread does no polling. While it is open, it refreshes the app list once a second and reads peaks about 30 times a second.
- One row per program. Group sessions by process image path, and apply volume and mute to every session in the group.
- Apps keep the order in which they were first seen. Don't sort rows by activity.
- Comments only for `// SAFETY:` notes and a non-obvious why.

## Verification

Run these before calling a change done:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

For a change to the audio thread, also run `cargo test -- --ignored`. It changes one app's volume on the real default output device and restores it.

For a change that affects the tray or the pop-up, build the release binary and run `scripts/verify-windows.ps1` with Tide closed. It uses the real mouse and keyboard, and pins the tray icon if Windows hid it in the overflow menu. It checks that the pop-up opens and closes, prints how long each open took, and saves a capture to `target/verify/popup.png`. Look at the capture.

Write commits as Conventional Commits (`feat: …`, `fix: …`, `docs: …`, `chore: …`).

## Releasing

Bump `version` in `Cargo.toml` and run `cargo build` to update `Cargo.lock`.
Commit the version bump and push a tag `vX.Y.Z` that matches the version.
The Release workflow builds and publishes the installer and portable executable.
