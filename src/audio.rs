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
        Foundation::CloseHandle,
        Media::Audio::{Endpoints::*, *},
        System::{Com::*, Threading::*},
    },
    core::{Interface, PCWSTR, PWSTR, Result},
};

mod policy {
    use std::ffi::c_void;
    use windows::{
        Win32::{
            Foundation::PROPERTYKEY,
            Media::Audio::{ERole, WAVEFORMATEX, WAVEFORMATEXTENSIBLE},
            System::Com::StructuredStorage::PROPVARIANT,
        },
        core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, interface},
    };

    pub const POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);

    #[interface("f8679f50-850a-41cf-9c72-430f290290c8")]
    pub unsafe trait IPolicyConfig: IUnknown {
        fn get_mix_format(&self, id: PCWSTR, format: *mut *mut WAVEFORMATEX) -> HRESULT;
        fn get_device_format(
            &self,
            id: PCWSTR,
            default: i32,
            format: *mut *mut WAVEFORMATEXTENSIBLE,
        ) -> HRESULT;
        fn reset_device_format(&self, id: PCWSTR) -> HRESULT;
        fn set_device_format(
            &self,
            id: PCWSTR,
            endpoint: *mut WAVEFORMATEXTENSIBLE,
            mix: *mut WAVEFORMATEXTENSIBLE,
        ) -> HRESULT;
        fn get_processing_period(
            &self,
            id: PCWSTR,
            default: i32,
            period: *mut i64,
            minimum: *mut i64,
        ) -> HRESULT;
        fn set_processing_period(&self, id: PCWSTR, period: *mut i64) -> HRESULT;
        fn get_share_mode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn set_share_mode(&self, id: PCWSTR, mode: *mut c_void) -> HRESULT;
        fn get_property_value(
            &self,
            id: PCWSTR,
            key: *const PROPERTYKEY,
            value: *mut PROPVARIANT,
        ) -> HRESULT;
        fn set_property_value(
            &self,
            id: PCWSTR,
            key: *const PROPERTYKEY,
            value: *mut PROPVARIANT,
        ) -> HRESULT;
        pub fn set_default_endpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
        fn set_endpoint_visibility(&self, id: PCWSTR, visible: i32) -> HRESULT;
    }
}

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

#[derive(Clone, PartialEq)]
pub enum Channel {
    Master,
    App(AppKey),
}

#[derive(Clone)]
pub struct AppInfo {
    pub key: AppKey,
    pub name: String,
    pub icon: Option<Arc<Rgba>>,
    pub accent: [u8; 3],
    pub volume: f32,
    pub muted: bool,
    pub streams: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceId(pub String);

#[derive(Clone)]
pub struct Device {
    pub id: DeviceId,
    pub name: String,
}

#[derive(Clone)]
pub struct Snapshot {
    pub device: String,
    pub device_id: Option<DeviceId>,
    pub devices: Vec<Device>,
    pub master_volume: f32,
    pub master_muted: bool,
    pub apps: Vec<AppInfo>,
}

impl Snapshot {
    pub fn channel_mut(&mut self, channel: &Channel) -> Option<(&mut f32, &mut bool)> {
        match channel {
            Channel::Master => Some((&mut self.master_volume, &mut self.master_muted)),
            Channel::App(key) => self
                .apps
                .iter_mut()
                .find(|app| &app.key == key)
                .map(|app| (&mut app.volume, &mut app.muted)),
        }
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            device: String::from("No output device"),
            device_id: None,
            devices: Vec::new(),
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
    SetDefaultDevice(DeviceId),
}

enum Message {
    Command(Command),
    IconLoaded(AppKey, AppIcon),
}

pub struct CommandSender(Arc<mpsc::Sender<Message>>);

impl CommandSender {
    pub fn send(&self, command: Command) -> bool {
        self.0.send(Message::Command(command)).is_ok()
    }
}

#[cfg(test)]
pub fn test_channel() -> (CommandSender, impl Fn() -> Option<Command>) {
    let (tx, rx) = mpsc::channel();
    let next = move || match rx.try_recv() {
        Ok(Message::Command(command)) => Some(command),
        _ => None,
    };
    (CommandSender(Arc::new(tx)), next)
}

pub fn start(
    mut publish: impl FnMut(Update) + Send + 'static,
) -> std::io::Result<(CommandSender, Live<f32>)> {
    let (tx, rx) = mpsc::channel();
    let tx = Arc::new(tx);
    let results = Arc::downgrade(&tx);
    let (icon_tx, icon_rx) = mpsc::channel::<(AppKey, PathBuf)>();
    std::thread::Builder::new()
        .name("tide-icons".into())
        .spawn(move || {
            let Ok(_apartment) = Apartment::start() else {
                return;
            };
            for (key, path) in icon_rx {
                let icon = AppIcon::load(&path);
                let Some(results) = results.upgrade() else {
                    break;
                };
                if results.send(Message::IconLoaded(key, icon)).is_err() {
                    break;
                }
            }
        })?;
    let master_peak = Live::new(0.0);
    let peak = master_peak.clone();
    std::thread::Builder::new()
        .name("tide-audio".into())
        .spawn(move || {
            let Ok(_apartment) = Apartment::start() else {
                let mut generation = 0;
                while let Ok(command) = rx.recv() {
                    if matches!(command, Message::Command(Command::Open)) {
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
            Worker::new(peak, icon_tx).run(rx, publish);
        })?;
    Ok((CommandSender(tx), master_peak))
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
struct AppIcon {
    image: Option<Arc<Rgba>>,
    accent: Option<[u8; 3]>,
}

impl AppIcon {
    fn load(path: &Path) -> Self {
        let image = icon::app_icon(path);
        Self {
            accent: image.as_ref().and_then(icon::accent),
            image: image.map(Arc::new),
        }
    }

    fn apply(&self, app: &mut AppInfo) {
        app.icon = self.image.clone();
        app.accent = self
            .accent
            .unwrap_or_else(|| icon::fallback_accent(&app.name));
    }
}

#[derive(Default)]
struct ProcessDetails {
    description: Option<String>,
    icon: AppIcon,
}

struct Worker {
    enumerator: Option<IMMDeviceEnumerator>,
    output: Option<Output>,
    order: AppOrder,
    details: HashMap<AppKey, ProcessDetails>,
    icons: mpsc::Sender<(AppKey, PathBuf)>,
    peaks: Peaks,
    master_peak: Live<f32>,
    revision: u64,
}

impl Worker {
    fn new(master_peak: Live<f32>, icons: mpsc::Sender<(AppKey, PathBuf)>) -> Self {
        Self {
            enumerator: None,
            output: None,
            order: AppOrder::default(),
            details: HashMap::new(),
            icons,
            peaks: HashMap::new(),
            master_peak,
            revision: 0,
        }
    }

    fn run(&mut self, rx: mpsc::Receiver<Message>, mut publish: impl FnMut(Update)) {
        let mut next_tick: Option<Instant> = None;
        let mut refresh_at = Instant::now();
        let mut generation = 0;
        let mut snapshot = Snapshot::default();
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
            let opening = matches!(command, Some(Message::Command(Command::Open)));
            let switching = matches!(
                command,
                Some(Message::Command(Command::SetDefaultDevice(_)))
            );
            match command {
                Some(Message::Command(Command::Close)) => {
                    next_tick = None;
                    self.output = None;
                    snapshot = Snapshot::default();
                    continue;
                }
                Some(Message::Command(Command::SetVolume(channel, volume))) => {
                    self.revision += 1;
                    self.set_volume(&channel, volume);
                    if let Some((current, _)) = snapshot.channel_mut(&channel) {
                        *current = volume;
                    }
                }
                Some(Message::Command(Command::SetMute(channel, muted))) => {
                    self.revision += 1;
                    self.set_mute(&channel, muted);
                    if let Some((_, current)) = snapshot.channel_mut(&channel) {
                        *current = muted;
                    }
                }
                Some(Message::Command(Command::SetDefaultDevice(id))) => {
                    self.revision += 1;
                    let _ = set_default_device(&id);
                }
                Some(Message::Command(Command::Open)) => {
                    generation += 1;
                    self.revision = 0;
                }
                Some(Message::IconLoaded(key, icon)) => {
                    let cached = &mut self.details.get_mut(&key).unwrap().icon;
                    *cached = icon;
                    if let Some(app) = snapshot.apps.iter_mut().find(|app| app.key == key) {
                        cached.apply(app);
                        publish(self.update(&snapshot, generation));
                    }
                }
                None => {}
            }
            let now = Instant::now();
            if opening || next_tick.is_some() && (switching || now >= refresh_at) {
                snapshot = self.refresh();
                self.poll_peaks();
                publish(self.update(&snapshot, generation));
                refresh_at = Instant::now() + REFRESH;
                next_tick = Some(Instant::now() + PEAK_INTERVAL);
            } else if next_tick.is_some_and(|tick| now >= tick) {
                self.poll_peaks();
                next_tick = Some(now + PEAK_INTERVAL);
            }
        }
    }

    fn update(&self, snapshot: &Snapshot, generation: u64) -> Update {
        let peaks = snapshot
            .apps
            .iter()
            .map(|app| (app.key.clone(), self.peaks[&app.key].clone()))
            .collect();
        Update {
            snapshot: snapshot.clone(),
            peaks,
            generation,
            revision: self.revision,
        }
    }

    fn refresh(&mut self) -> Snapshot {
        let mut snapshot = Snapshot::default();
        self.output = self.read_output(&mut snapshot).ok();
        if self.output.is_none() {
            self.master_peak.set(0.0);
        }
        snapshot
    }

    fn read_output(&mut self, snapshot: &mut Snapshot) -> Result<Output> {
        // SAFETY: the audio worker owns this MTA and every WASAPI interface here.
        unsafe {
            if self.enumerator.is_none() {
                self.enumerator = Some(CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?);
            }
            let enumerator = self.enumerator.as_ref().unwrap();
            snapshot.devices = active_devices(enumerator).unwrap_or_default();
            let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
            snapshot.device_id = device_id(&device).ok();
            snapshot.device = snapshot
                .devices
                .iter()
                .find(|entry| Some(&entry.id) == snapshot.device_id.as_ref())
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| device_name(&device, "Default output"));
            let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
            let meter = device.Activate(CLSCTX_ALL, None).ok();
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
                let details = self.details.entry(key.clone()).or_insert_with(|| {
                    let Some(path) = path else {
                        return ProcessDetails::default();
                    };
                    let _ = self.icons.send((key.clone(), path.to_owned()));
                    ProcessDetails {
                        description: icon::file_description(path),
                        icon: AppIcon::default(),
                    }
                });
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
                self.details[&app.key].icon.apply(app);
                self.peaks
                    .entry(app.key.clone())
                    .or_insert_with(|| Live::new(0.0));
            }
            snapshot.master_volume = master_volume;
            snapshot.master_muted = master_muted;
            snapshot.apps = apps;
            Ok(Output {
                endpoint,
                meter,
                sessions,
            })
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

fn active_devices(enumerator: &IMMDeviceEnumerator) -> Result<Vec<Device>> {
    // SAFETY: the enumeration and its endpoint interfaces stay in the owning MTA.
    unsafe {
        let devices = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
        Ok((0..devices.GetCount()?)
            .filter_map(|index| {
                let device = devices.Item(index).ok()?;
                Some(Device {
                    id: device_id(&device).ok()?,
                    name: device_name(&device, "Output device"),
                })
            })
            .collect())
    }
}

fn device_id(device: &IMMDevice) -> Result<DeviceId> {
    // SAFETY: GetId returns a NUL-terminated task allocation on success.
    unsafe { device.GetId().map(|id| DeviceId(take_string(id))) }
}

fn device_name(device: &IMMDevice, fallback: &str) -> String {
    // SAFETY: the property store belongs to the current MTA.
    unsafe {
        device
            .OpenPropertyStore(STGM_READ)
            .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName))
            .map(|value| value.to_string())
            .ok()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| fallback.into())
    }
}

fn set_endpoint_role(config: &policy::IPolicyConfig, id: &DeviceId, role: ERole) -> Result<()> {
    let id: Vec<_> = id.0.encode_utf16().chain([0]).collect();
    // SAFETY: the ID buffer is NUL-terminated and lives through the COM call.
    unsafe { config.set_default_endpoint(PCWSTR(id.as_ptr()), role).ok() }
}

fn set_default_device(id: &DeviceId) -> Result<()> {
    // SAFETY: the audio worker creates and uses the policy client in its MTA.
    let config = unsafe { CoCreateInstance(&policy::POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)? };
    let console = set_endpoint_role(&config, id, eConsole);
    let multimedia = set_endpoint_role(&config, id, eMultimedia);
    console.and(multimedia)
}

fn peak(meter: Option<&IAudioMeterInformation>) -> f32 {
    // SAFETY: the audio worker calls meters within their owning MTA.
    meter
        .and_then(|meter| unsafe { meter.GetPeakValue().ok() })
        .unwrap_or(0.0)
}

fn take_string(value: PWSTR) -> String {
    // SAFETY: callers pass a NUL-terminated task allocation or a null pointer.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static AUDIO_SETTINGS: Mutex<()> = Mutex::new(());

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
    fn a_loaded_icon_replaces_the_placeholder_without_changing_audio_state() {
        let mut apps =
            AppOrder::default().group([row(7, "Player", SessionState::Active, 0.6, true)]);
        let app = &mut apps[0];
        AppIcon::default().apply(app);
        assert!(app.icon.is_none());
        assert_eq!(app.accent, icon::fallback_accent("Player"));

        let image = Arc::new(Rgba {
            width: 1,
            height: 1,
            bytes: vec![10, 20, 30, 255],
        });
        AppIcon {
            image: Some(Arc::clone(&image)),
            accent: Some([10, 20, 30]),
        }
        .apply(app);

        assert!(Arc::ptr_eq(app.icon.as_ref().unwrap(), &image));
        assert_eq!(app.accent, [10, 20, 30]);
        assert_eq!(app.name, "Player");
        assert_eq!(app.volume, 0.6);
        assert!(app.muted);
        assert_eq!(app.streams, 1);
    }

    #[test]
    #[ignore = "switches and restores the real default output when two devices are active"]
    fn real_default_output_switches_and_restores() -> Result<()> {
        let _settings = AUDIO_SETTINGS.lock().unwrap();
        let _apartment = Apartment::start()?;
        // SAFETY: this test owns the COM apartment and all endpoint interfaces.
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let start = Instant::now();
            let devices = active_devices(&enumerator)?;
            let elapsed = start.elapsed();
            let count = enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?
                .GetCount()?;
            assert_eq!(devices.len(), count as usize);
            assert!(
                devices
                    .iter()
                    .all(|device| !device.id.0.is_empty() && device.name != "Output device")
            );
            println!(
                "Enumerated {count} active render endpoints with IDs and names in {elapsed:?}"
            );
            if devices.len() < 2 {
                println!("Switch skipped: at least two active render endpoints are required");
                return Ok(());
            }

            let default = |role| device_id(&enumerator.GetDefaultAudioEndpoint(eRender, role)?);
            let roles = [eConsole, eMultimedia];
            let originals = [default(eConsole)?, default(eMultimedia)?];
            let communications = default(eCommunications).ok();
            let target = devices
                .iter()
                .find(|device| device.id != originals[0])
                .unwrap();
            let config = CoCreateInstance(&policy::POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)?;

            let switched = set_default_device(&target.id);
            let written = roles.map(default);
            let written_communications = default(eCommunications).ok();
            let restores = std::array::from_fn::<_, 2, _>(|i| {
                set_endpoint_role(&config, &originals[i], roles[i])
            });
            for restored in restores {
                restored?;
            }
            assert_eq!(default(eConsole)?, originals[0]);
            assert_eq!(default(eMultimedia)?, originals[1]);
            assert_eq!(default(eCommunications).ok(), communications);
            assert_eq!(written_communications, communications);
            switched?;
            for id in written {
                assert_eq!(id?, target.id);
            }
            println!(
                "Switched console and multimedia to {} and restored both defaults",
                target.name
            );
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires an audio session on the real default output"]
    fn real_default_output_restores_app_volume() -> Result<()> {
        let _settings = AUDIO_SETTINGS.lock().unwrap();
        let _apartment = Apartment::start()?;
        let mut worker = Worker::new(Live::new(0.0), mpsc::channel().0);
        let mut snapshot = Snapshot::default();
        worker.output = Some(worker.read_output(&mut snapshot)?);
        let app = snapshot
            .apps
            .iter()
            .find(|app| app.key != AppKey::for_process(0, None))
            .expect("play audio in an app on the default device before running this test");
        let channel = Channel::App(app.key.clone());
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
