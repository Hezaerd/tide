use crate::icon::{self, Rgba};
use rune::Live;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    ptr,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Devices::FunctionDiscovery::PKEY_Device_FriendlyName,
        Foundation::{CloseHandle, SIZE},
        Media::Audio::{Endpoints::*, *},
        System::{Com::*, Threading::*},
        UI::Shell::{
            IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY, SIIGBF_SCALEUP,
        },
    },
    core::{Interface, PCWSTR, PWSTR, Result},
};

const REFRESH: Duration = Duration::from_secs(1);
const PEAK_INTERVAL: Duration = Duration::from_millis(33);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct AppKey(String);

impl AppKey {
    pub fn for_process(pid: u32, path: Option<&Path>) -> Self {
        let key = match (pid, path) {
            (0, _) => String::from("system:"),
            (_, Some(path)) => path.to_string_lossy().to_lowercase(),
            (_, None) => format!("pid:{pid}"),
        };
        Self(key)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone)]
pub enum Channel {
    Master,
    App(AppKey),
}

pub struct AppInfo {
    pub key: AppKey,
    pub name: String,
    pub icon: Option<Arc<Rgba>>,
    pub accent: [u8; 3],
    pub volume: f32,
    pub muted: bool,
    pub streams: usize,
}

pub struct Snapshot {
    pub device: String,
    pub master_volume: f32,
    pub master_muted: bool,
    pub apps: Vec<AppInfo>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            device: String::from("No output device"),
            master_volume: 0.0,
            master_muted: false,
            apps: Vec::new(),
        }
    }
}

pub type Peaks = HashMap<AppKey, Live<f32>>;

pub struct Update {
    pub snapshot: Snapshot,
    pub peaks: Peaks,
    pub generation: u64,
    pub revision: u64,
}

pub enum Command {
    Open,
    Close,
    SetVolume(Channel, f32),
    SetMute(Channel, bool),
}

pub fn start(
    mut publish: impl FnMut(Update) + Send + 'static,
) -> std::io::Result<(mpsc::Sender<Command>, Live<f32>)> {
    let (tx, rx) = mpsc::channel();
    let master_peak = Live::new(0.0);
    let peak = master_peak.clone();
    std::thread::Builder::new()
        .name("tide-audio".into())
        .spawn(move || {
            let Ok(_apartment) = Apartment::start() else {
                let mut generation = 0;
                while let Ok(command) = rx.recv() {
                    if matches!(command, Command::Open) {
                        generation += 1;
                        publish(Update {
                            snapshot: Snapshot::default(),
                            peaks: HashMap::new(),
                            generation,
                            revision: 0,
                        });
                    }
                }
                return;
            };
            Worker::new(peak).run(rx, publish);
        })?;
    Ok((tx, master_peak))
}

struct Apartment;

impl Apartment {
    fn start() -> Result<Self> {
        // SAFETY: this thread balances its successful MTA initialization in Drop.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: this is the initializing thread and its interfaces have been dropped.
        unsafe { CoUninitialize() };
    }
}

#[derive(Clone, Copy)]
enum SessionState {
    Active,
    Inactive,
    Expired,
}

struct SessionRow {
    key: AppKey,
    name: String,
    state: SessionState,
    volume: f32,
    muted: bool,
}

fn app_name(pid: u32, display: &str, description: Option<&str>, path: Option<&Path>) -> String {
    if pid == 0 {
        return String::from("System sounds");
    }
    let display = display.trim();
    if !display.is_empty() && !display.starts_with('@') {
        return display.into();
    }
    if let Some(description) = description.map(str::trim).filter(|name| !name.is_empty()) {
        return description.into();
    }
    path.and_then(Path::file_stem)
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("Process {pid}"))
}

#[derive(Default)]
struct AppOrder {
    next: usize,
    rank: HashMap<AppKey, usize>,
}

impl AppOrder {
    fn group(&mut self, rows: impl IntoIterator<Item = SessionRow>) -> Vec<AppInfo> {
        let mut groups = HashMap::<AppKey, (AppInfo, bool)>::new();
        for row in rows {
            if matches!(row.state, SessionState::Expired) {
                continue;
            }
            self.rank.entry(row.key.clone()).or_insert_with(|| {
                let rank = self.next;
                self.next += 1;
                rank
            });
            let active = matches!(row.state, SessionState::Active);
            let (app, has_active) = groups.entry(row.key.clone()).or_insert_with(|| {
                (
                    AppInfo {
                        key: row.key,
                        name: row.name.clone(),
                        volume: row.volume,
                        muted: true,
                        icon: None,
                        accent: [0; 3],
                        streams: 0,
                    },
                    active,
                )
            });
            if active && !*has_active {
                app.name = row.name;
                app.volume = row.volume;
                *has_active = true;
            }
            app.muted &= row.muted;
            app.streams += 1;
        }
        let mut apps: Vec<_> = groups.into_values().map(|(app, _)| app).collect();
        apps.sort_unstable_by_key(|app| self.rank[&app.key]);
        apps
    }
}

struct Session {
    key: AppKey,
    volume: ISimpleAudioVolume,
    meter: Option<IAudioMeterInformation>,
}

struct Output {
    endpoint: IAudioEndpointVolume,
    meter: Option<IAudioMeterInformation>,
    sessions: Vec<Session>,
}

#[derive(Default)]
struct ProcessDetails {
    description: Option<String>,
    icon: Option<Arc<Rgba>>,
    accent: Option<[u8; 3]>,
}

struct Worker {
    enumerator: Option<IMMDeviceEnumerator>,
    output: Option<Output>,
    order: AppOrder,
    details: HashMap<AppKey, ProcessDetails>,
    peaks: Peaks,
    master_peak: Live<f32>,
    revision: u64,
}

impl Worker {
    fn new(master_peak: Live<f32>) -> Self {
        Self {
            enumerator: None,
            output: None,
            order: AppOrder::default(),
            details: HashMap::new(),
            peaks: HashMap::new(),
            master_peak,
            revision: 0,
        }
    }

    fn run(&mut self, rx: mpsc::Receiver<Command>, mut publish: impl FnMut(Update)) {
        let mut next_tick: Option<Instant> = None;
        let mut refresh_at = Instant::now();
        let mut generation = 0;
        loop {
            let command = match next_tick {
                Some(tick) => match rx.recv_timeout(tick - Instant::now()) {
                    Ok(command) => Some(command),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                },
                None => match rx.recv() {
                    Ok(command) => Some(command),
                    Err(_) => break,
                },
            };
            let opening = matches!(command, Some(Command::Open));
            match command {
                Some(Command::Close) => {
                    next_tick = None;
                    self.output = None;
                    continue;
                }
                Some(Command::SetVolume(channel, volume)) => {
                    self.revision += 1;
                    self.set_volume(&channel, volume);
                }
                Some(Command::SetMute(channel, muted)) => {
                    self.revision += 1;
                    self.set_mute(&channel, muted);
                }
                Some(Command::Open) => {
                    generation += 1;
                    self.revision = 0;
                }
                None => {}
            }
            let now = Instant::now();
            if opening || next_tick.is_some() && now >= refresh_at {
                let snapshot = self.refresh();
                self.poll_peaks();
                let peaks = snapshot
                    .apps
                    .iter()
                    .map(|app| (app.key.clone(), self.peaks[&app.key].clone()))
                    .collect();
                publish(Update {
                    snapshot,
                    peaks,
                    generation,
                    revision: self.revision,
                });
                refresh_at = Instant::now() + REFRESH;
                next_tick = Some(Instant::now() + PEAK_INTERVAL);
            } else if next_tick.is_some_and(|tick| now >= tick) {
                self.poll_peaks();
                next_tick = Some(now + PEAK_INTERVAL);
            }
        }
    }

    fn refresh(&mut self) -> Snapshot {
        match self.read_output() {
            Ok((snapshot, output)) => {
                self.output = Some(output);
                snapshot
            }
            Err(_) => {
                self.output = None;
                self.master_peak.set(0.0);
                Snapshot::default()
            }
        }
    }

    fn read_output(&mut self) -> Result<(Snapshot, Output)> {
        // SAFETY: the audio worker owns this MTA and every WASAPI interface here.
        unsafe {
            if self.enumerator.is_none() {
                self.enumerator = Some(CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?);
            }
            let enumerator = self.enumerator.as_ref().unwrap();
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
            let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            let meter = device.Activate(CLSCTX_ALL, None).ok();
            let device_name = device
                .OpenPropertyStore(STGM_READ)
                .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
                .map(|value| value.to_string())
                .unwrap_or_else(|_| String::from("Default output"));
            let master_volume = endpoint.GetMasterVolumeLevelScalar()?;
            let master_muted = endpoint.GetMute()?.as_bool();
            let manager: IAudioSessionManager2 = device.Activate(CLSCTX_ALL, None)?;
            let enumerator = manager.GetSessionEnumerator()?;
            let mut rows = Vec::new();
            let mut sessions = Vec::new();
            let mut paths = HashMap::new();
            for index in 0..enumerator.GetCount()? {
                let Ok(control) = enumerator.GetSession(index) else {
                    continue;
                };
                let Ok(state) = control.GetState() else {
                    continue;
                };
                let state = match state {
                    state if state == AudioSessionStateActive => SessionState::Active,
                    state if state == AudioSessionStateInactive => SessionState::Inactive,
                    _ => SessionState::Expired,
                };
                if matches!(state, SessionState::Expired) {
                    continue;
                }
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                let Ok(pid) = control2.GetProcessId() else {
                    continue;
                };
                let Ok(volume) = control.cast::<ISimpleAudioVolume>() else {
                    continue;
                };
                let (Ok(level), Ok(mute)) = (volume.GetMasterVolume(), volume.GetMute()) else {
                    continue;
                };
                let path = paths
                    .entry(pid)
                    .or_insert_with(|| process_path(pid))
                    .as_deref();
                let key = AppKey::for_process(pid, path);
                let details = self
                    .details
                    .entry(key.clone())
                    .or_insert_with(|| path.map_or_else(ProcessDetails::default, process_details));
                let display = control
                    .GetDisplayName()
                    .map(take_string)
                    .unwrap_or_default();
                rows.push(SessionRow {
                    key: key.clone(),
                    name: app_name(pid, &display, details.description.as_deref(), path),
                    state,
                    volume: level,
                    muted: mute.as_bool(),
                });
                sessions.push(Session {
                    key,
                    volume,
                    meter: control.cast().ok(),
                });
            }
            let mut apps = self.order.group(rows);
            for app in &mut apps {
                app.icon = self.details[&app.key].icon.clone();
                app.accent = self.details[&app.key]
                    .accent
                    .unwrap_or_else(|| icon::fallback_accent(&app.name));
                self.peaks
                    .entry(app.key.clone())
                    .or_insert_with(|| Live::new(0.0));
            }
            Ok((
                Snapshot {
                    device: device_name,
                    master_volume,
                    master_muted,
                    apps,
                },
                Output {
                    endpoint,
                    meter,
                    sessions,
                },
            ))
        }
    }

    fn set_volume(&self, channel: &Channel, value: f32) {
        let Some(output) = &self.output else { return };
        // SAFETY: these interfaces belong to the current MTA, and the event GUID is optional.
        unsafe {
            match channel {
                Channel::Master => {
                    let _ = output
                        .endpoint
                        .SetMasterVolumeLevelScalar(value, ptr::null());
                }
                Channel::App(key) => {
                    for session in output.sessions.iter().filter(|session| &session.key == key) {
                        let _ = session.volume.SetMasterVolume(value, ptr::null());
                    }
                }
            }
        }
    }

    fn set_mute(&self, channel: &Channel, muted: bool) {
        let Some(output) = &self.output else { return };
        // SAFETY: these interfaces belong to the current MTA, and the event GUID is optional.
        unsafe {
            match channel {
                Channel::Master => {
                    let _ = output.endpoint.SetMute(muted, ptr::null());
                }
                Channel::App(key) => {
                    for session in output.sessions.iter().filter(|session| &session.key == key) {
                        let _ = session.volume.SetMute(muted, ptr::null());
                    }
                }
            }
        }
    }

    fn poll_peaks(&self) {
        let Some(output) = &self.output else { return };
        self.master_peak.set(peak(output.meter.as_ref()));
        let mut levels = HashMap::<&AppKey, f32>::new();
        for session in &output.sessions {
            let level = levels.entry(&session.key).or_default();
            *level = level.max(peak(session.meter.as_ref()));
        }
        for (key, level) in levels {
            self.peaks[key].set(level);
        }
    }
}

fn peak(meter: Option<&IAudioMeterInformation>) -> f32 {
    // SAFETY: the audio worker calls meters within their owning MTA.
    meter
        .and_then(|meter| unsafe { meter.GetPeakValue().ok() })
        .unwrap_or(0.0)
}

fn take_string(value: PWSTR) -> String {
    // SAFETY: GetDisplayName returns a NUL-terminated task allocation or a null pointer.
    unsafe {
        let name = if value.is_null() {
            String::new()
        } else {
            value.to_string().unwrap_or_default()
        };
        CoTaskMemFree(Some(value.0.cast()));
        name
    }
}

fn process_path(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    let mut buffer = vec![0u16; 32768];
    let mut len = buffer.len() as u32;
    // SAFETY: the authorized process handle is closed, and len describes the UTF-16 buffer.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(handle);
        result.ok()?;
    }
    use std::os::windows::ffi::OsStringExt;
    Some(std::ffi::OsString::from_wide(&buffer[..len as usize]).into())
}

fn process_details(path: &Path) -> ProcessDetails {
    let name = icon::wide(path);
    let size = 56;
    // SAFETY: this runs inside the audio MTA. The NUL-terminated path lives through the call
    // and read_bitmap consumes the returned HBITMAP.
    let icon = unsafe {
        SHCreateItemFromParsingName::<_, _, IShellItemImageFactory>(PCWSTR(name.as_ptr()), None)
            .and_then(|item| {
                item.GetImage(
                    SIZE { cx: size, cy: size },
                    SIIGBF_ICONONLY | SIIGBF_SCALEUP,
                )
            })
            .ok()
            .and_then(icon::read_bitmap)
    };
    ProcessDetails {
        description: icon::file_description(path),
        accent: icon.as_ref().and_then(icon::accent),
        icon: icon.map(Arc::new),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_prefer_display_then_description_then_exe_stem() {
        let path = Some(Path::new(r"C:\Apps\player.exe"));
        assert_eq!(app_name(123, " Player ", Some("Other"), path), "Player");
        assert_eq!(
            app_name(123, "@resource", Some("Media Player"), path),
            "Media Player"
        );
        assert_eq!(app_name(123, "", Some(" "), path), "player");
        assert_eq!(app_name(123, "", None, None), "Process 123");
        assert_eq!(app_name(0, "Another name", None, None), "System sounds");
    }

    fn row(pid: u32, path: &str, state: SessionState, volume: f32, muted: bool) -> SessionRow {
        SessionRow {
            key: AppKey::for_process(pid, Some(Path::new(path))),
            name: path.into(),
            state,
            volume,
            muted,
        }
    }

    #[test]
    fn groups_streams_by_image_and_preserves_first_seen_order() {
        use SessionState::*;
        let a = r"C:\Player.exe";
        let b = r"C:\Other.exe";
        let mut order = AppOrder::default();
        let apps = order.group([
            row(7, a, Inactive, 1.0, true),
            row(8, b, Inactive, 0.5, true),
            row(7, a, Active, 0.8, false),
            row(9, r"c:\PLAYER.EXE", Active, 0.8, true),
            row(7, a, Expired, 0.2, false),
            row(10, r"C:\Gone.exe", Expired, 1.0, false),
        ]);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].name, a);
        assert_eq!(apps[0].streams, 3);
        assert_eq!(apps[0].volume, 0.8);
        assert!(!apps[0].muted);
        assert!(apps[1].muted);
        let apps = order.group([row(8, b, Active, 0.5, true)]);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, b);
        let apps = order.group([row(8, b, Active, 0.5, true), row(7, a, Active, 0.8, false)]);
        assert_eq!(
            apps.iter().map(|app| app.name.as_str()).collect::<Vec<_>>(),
            [a, b]
        );
    }

    #[test]
    #[ignore = "requires an audio session on the real default output"]
    fn real_default_output_restores_app_volume() -> Result<()> {
        let _apartment = Apartment::start()?;
        let mut worker = Worker::new(Live::new(0.0));
        let (snapshot, output) = worker.read_output()?;
        let app = snapshot
            .apps
            .iter()
            .find(|app| app.key != AppKey::for_process(0, None))
            .expect("play audio in an app on the default device before running this test");
        let channel = Channel::App(app.key.clone());
        worker.output = Some(output);
        let sessions: Vec<_> = worker
            .output
            .iter()
            .flat_map(|output| &output.sessions)
            .filter(|session| session.key == app.key)
            .collect();
        // SAFETY: the test owns the COM apartment and all session interfaces. Restore each
        // original scalar before checking the round trip, even when a read fails.
        unsafe {
            let before: Vec<_> = sessions
                .iter()
                .map(|s| s.volume.GetMasterVolume())
                .collect::<Result<_>>()?;
            let target = if before[0] >= 0.5 {
                before[0] - 0.01
            } else {
                before[0] + 0.01
            };
            worker.set_volume(&channel, target);
            let written: Vec<_> = sessions
                .iter()
                .map(|s| s.volume.GetMasterVolume())
                .collect();
            let restores: Vec<_> = sessions
                .iter()
                .zip(&before)
                .map(|(session, previous)| session.volume.SetMasterVolume(*previous, ptr::null()))
                .collect();
            restores.into_iter().collect::<Result<Vec<_>>>()?;
            for ((session, previous), current) in sessions.iter().zip(before).zip(written) {
                let current = current?;
                let restored = session.volume.GetMasterVolume()?;
                println!(
                    "{} on {}: {previous:.3} -> set {target:.3} -> read {current:.3} -> restored {restored:.3}",
                    app.name, snapshot.device
                );
                assert!((current - target).abs() < 0.001);
                assert!((restored - previous).abs() < 0.001);
            }
        }
        Ok(())
    }
}
