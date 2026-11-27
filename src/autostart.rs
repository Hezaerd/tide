use std::ffi::OsString;
use windows::core::HSTRING;

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const NAME: &str = "Tide";

fn command() -> std::io::Result<HSTRING> {
    let mut command = OsString::from("\"");
    command.push(std::env::current_exe()?.as_os_str());
    command.push("\"");
    Ok(HSTRING::from(command.as_os_str()))
}

pub fn enabled() -> bool {
    command().ok().is_some_and(|expected| {
        windows_registry::CURRENT_USER
            .open(RUN)
            .and_then(|key| key.get_hstring(NAME))
            .is_ok_and(|value| value == expected)
    })
}

pub fn toggle() -> Result<(), Box<dyn std::error::Error>> {
    let key = windows_registry::CURRENT_USER.create(RUN)?;
    if enabled() {
        key.remove_value(NAME)?;
    } else {
        key.set_hstring(NAME, &command()?)?;
    }
    Ok(())
}
