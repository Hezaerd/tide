use std::{
    collections::HashMap,
    path::Path,
    sync::{Weak, mpsc::Sender},
    time::{Duration, Instant},
};

use rune::{
    AppHandle, Color, Cursor, Cx, El, GesturePhase, Key, KeyEvent, LineCap, Live, Phase, Pixels,
    ScrollSpec, Shape, Tray, Tx, WheelEvent, canvas, col, el, row, text, tok::TextStyle,
};

use rune_kit::{ScrollBar, ScrollBarStyle};

use crate::{
    audio::{AppInfo, AppKey, Channel, Command, Peaks, Snapshot, Update},
    theme::{self, Theme},
    tray::{self, Placement},
};

pub const WIDTH: f32 = 320.0;
const FRAME_HEIGHT: f32 = 99.0;
const ROW_HEIGHT: f32 = 42.0;
const TRACK_WIDTH: f32 = 204.0;
const TRACK_HEIGHT: f32 = 6.0;
const DEBOUNCE: Duration = Duration::from_millis(300);
const WHEEL_VOLUME_STEP: f32 = 0.02;
const SCROLL_POINTS_PER_STEP: f32 = 16.0;
const UNPHASED_SCROLL_IDLE: Duration = Duration::from_millis(100);

struct VolumeScroll {
    channel: Channel,
    remainder: f32,
    last_event: Instant,
}

#[derive(Default)]
enum Panel {
    #[default]
    Closed,
    Opening(Placement),
    Open(Placement),
}

pub struct State {
    snapshot: Snapshot,
    peaks: Peaks,
    master_peak: Live<f32>,
    pixels: HashMap<AppKey, [Pixels; 2]>,
    command: Option<Sender<Command>>,
    window: Option<AppHandle<Self>>,
    tray: Weak<Tray>,
    panel: Panel,
    lost_focus_at: Option<Instant>,
    generation: u64,
    revision: u64,
    scene: Option<(bool, f32)>,
    volume_scroll: Option<VolumeScroll>,
}

impl State {
    pub fn new() -> Self {
        Self {
            snapshot: Snapshot::default(),
            peaks: HashMap::new(),
            master_peak: Live::new(0.0),
            pixels: HashMap::new(),
            command: None,
            window: None,
            tray: Weak::new(),
            panel: Panel::Closed,
            lost_focus_at: None,
            generation: 0,
            revision: 0,
            scene: None,
            volume_scroll: None,
        }
    }

    pub fn connected(
        command: Sender<Command>,
        master_peak: Live<f32>,
        window: AppHandle<Self>,
        tray: Weak<Tray>,
    ) -> Self {
        Self {
            command: Some(command),
            master_peak,
            window: Some(window),
            tray,
            ..Self::new()
        }
    }

    fn send(&self, command: Command) -> bool {
        self.command
            .as_ref()
            .is_some_and(|tx| tx.send(command).is_ok())
    }

    pub fn toggle(&mut self, placement: Option<Placement>, time: Instant, tx: &mut Tx) {
        if !matches!(self.panel, Panel::Closed) {
            self.close(tx);
        } else if !same_click(self.lost_focus_at.take(), time)
            && let Some(placement) = placement
            && self.send(Command::Open)
        {
            self.generation += 1;
            self.revision = 0;
            self.panel = Panel::Opening(placement);
        }
    }

    pub fn apply(&mut self, update: Update, tx: &mut Tx) -> bool {
        if update.generation != self.generation || update.revision < self.revision {
            return false;
        }
        let (placement, opening) = match self.panel {
            Panel::Closed => return false,
            Panel::Opening(placement) => (placement, true),
            Panel::Open(placement) => (placement, false),
        };
        let count_changed = self.snapshot.apps.len() != update.snapshot.apps.len();
        for app in &update.snapshot.apps {
            if let Some(icon) = &app.icon {
                self.pixels.entry(app.key.clone()).or_insert_with(|| {
                    [icon.as_ref().clone(), icon.gray()].map(|image| {
                        let pixels = Pixels::new();
                        pixels.set(image.width, image.height, image.bytes);
                        pixels
                    })
                });
            }
        }
        self.snapshot = update.snapshot;
        self.peaks = update.peaks;
        if let Some(window) = &self.window {
            if opening || count_changed {
                let height = panel_height(self.snapshot.apps.len(), placement.max_height());
                window.set_position(placement.position(WIDTH, height));
                tx.resize_window_instant((WIDTH, height));
            }
            if opening {
                self.panel = Panel::Open(placement);
                window.open();
            }
        }
        true
    }

    fn close(&mut self, tx: &mut Tx) {
        self.closed(tx);
        tx.close_window();
    }

    pub fn closed(&mut self, _: &mut Tx) {
        self.volume_scroll = None;
        if !matches!(self.panel, Panel::Closed) {
            self.panel = Panel::Closed;
            self.send(Command::Close);
        }
    }

    pub fn focus_changed(&mut self, focused: bool, tx: &mut Tx) {
        if !focused && matches!(self.panel, Panel::Open(_)) {
            let over_icon = self
                .tray
                .upgrade()
                .and_then(|tray| tray::pointer_on_icon(&tray));
            self.lost_focus_at = over_icon.unwrap_or(true).then(Instant::now);
            self.close(tx);
        }
    }

    pub fn key(&mut self, event: &KeyEvent, tx: &mut Tx) -> bool {
        if event.key != Key::Escape {
            return false;
        }
        self.close(tx);
        true
    }

    fn max_height(&self) -> f32 {
        if let Some((_, height)) = self.scene {
            return height;
        }
        match self.panel {
            Panel::Closed => FRAME_HEIGHT,
            Panel::Opening(placement) | Panel::Open(placement) => placement.max_height(),
        }
    }

    fn channel_mut(&mut self, channel: &Channel) -> Option<(&mut f32, &mut bool)> {
        match channel {
            Channel::Master => Some((
                &mut self.snapshot.master_volume,
                &mut self.snapshot.master_muted,
            )),
            Channel::App(key) => self
                .snapshot
                .apps
                .iter_mut()
                .find(|app| &app.key == key)
                .map(|app| (&mut app.volume, &mut app.muted)),
        }
    }

    fn wheel(&mut self, channel: &Channel, event: WheelEvent, now: Instant) -> bool {
        if event.touch {
            return false;
        }
        if event.momentum {
            return true;
        }
        if event.notch && !event.precise {
            self.volume_scroll = None;
            if event.dy == 0.0 {
                return false;
            }
            self.change(channel, Change::Step(event.dy.signum() * WHEEL_VOLUME_STEP));
            return true;
        }

        let remainder = self.volume_scroll.as_ref().map_or(0.0, |scroll| {
            // Windows sends fractional wheel events without gesture phases.
            if &scroll.channel != channel
                || event.phase == GesturePhase::Began
                || (!event.precise && now.duration_since(scroll.last_event) >= UNPHASED_SCROLL_IDLE)
            {
                0.0
            } else {
                scroll.remainder
            }
        });
        let (steps, remainder) = scroll_steps(remainder, event.dy);
        self.volume_scroll = (event.phase != GesturePhase::Ended).then(|| VolumeScroll {
            channel: channel.clone(),
            remainder,
            last_event: now,
        });
        if steps != 0.0 {
            self.change(channel, Change::Step(steps * WHEEL_VOLUME_STEP));
        }
        true
    }

    fn change(&mut self, channel: &Channel, change: Change) {
        let Some((volume, muted)) = self.channel_mut(channel) else {
            return;
        };
        let command = match change {
            Change::Mute => {
                *muted = !*muted;
                Command::SetMute(channel.clone(), *muted)
            }
            Change::Set(_) | Change::Step(_) => {
                let value = match change {
                    Change::Set(value) => value,
                    Change::Step(amount) => (*volume * 100.0).round() / 100.0 + amount,
                    Change::Mute => unreachable!(),
                };
                let value = (value.clamp(0.0, 1.0) * 100.0).round() / 100.0;
                if *volume == value {
                    return;
                }
                *volume = value;
                Command::SetVolume(channel.clone(), value)
            }
        };
        if self.send(command) {
            self.revision += 1;
        }
    }
}

fn scroll_steps(remainder: f32, dy: f32) -> (f32, f32) {
    let points = remainder + dy;
    (
        (points / SCROLL_POINTS_PER_STEP).trunc(),
        points % SCROLL_POINTS_PER_STEP,
    )
}

fn same_click(lost_focus_at: Option<Instant>, activated_at: Instant) -> bool {
    lost_focus_at.is_some_and(|lost| {
        activated_at
            .checked_duration_since(lost)
            .is_none_or(|elapsed| elapsed <= DEBOUNCE)
    })
}

fn panel_height(count: usize, max_height: f32) -> f32 {
    (FRAME_HEIGHT + ROW_HEIGHT * count as f32).min(max_height.max(FRAME_HEIGHT))
}

enum Change {
    Set(f32),
    Step(f32),
    Mute,
}

fn keyboard_change(key: &Key) -> Option<Change> {
    Some(match key {
        Key::Left | Key::Down => Change::Step(-0.02),
        Key::Right | Key::Up => Change::Step(0.02),
        Key::PageDown => Change::Step(-0.1),
        Key::PageUp => Change::Step(0.1),
        Key::Home => Change::Set(0.0),
        Key::End => Change::Set(1.0),
        Key::Text(text) if text == " " || text.eq_ignore_ascii_case("m") => Change::Mute,
        _ => return None,
    })
}

fn label(value: impl Into<String>, style: TextStyle, color: Color) -> El {
    text(value.into()).font(style).color(color).truncate()
}

pub fn view(state: &State, cx: &mut Cx<State>) -> El {
    let dark = state
        .scene
        .map_or_else(|| cx.system_dark(), |(dark, _)| dark);
    let t = Theme::new(dark);
    let snapshot = &state.snapshot;

    let count = snapshot.apps.len();
    let header = row()
        .key("header")
        .h(26.0)
        .px(8.0)
        .pt(2.0)
        .pb(8.0)
        .gap(8.0)
        .items_center()
        .child(theme::wave(t.sub))
        .child(
            label(
                &snapshot.device,
                TextStyle::new(12.5, 600.0).lh(16.0),
                t.text,
            )
            .key("device")
            .grow(1.0)
            .min_w(0.0),
        )
        .child(
            label(
                format!("{count} {}", if count == 1 { "app" } else { "apps" }),
                TextStyle::new(11.5, 400.0).lh(16.0).features("'tnum' 1"),
                t.sub,
            )
            .key("count"),
        );

    let master = MixerRow {
        channel: Channel::Master,
        name: "Master",
        volume: snapshot.master_volume,
        muted: snapshot.master_muted,
        streams: 0,
        accent: theme::hex(0xd9a441),
        peak: Some(&state.master_peak),
        pixels: None,
    }
    .element(t);

    let content = col()
        .key("app-list")
        .w_full()
        .children(snapshot.apps.iter().map(|app| {
            MixerRow {
                channel: Channel::App(app.key.clone()),
                name: &app.name,
                volume: app.volume,
                muted: app.muted,
                streams: app.streams,
                accent: theme::rgb(app.accent),
                peak: state.peaks.get(&app.key),
                pixels: state
                    .pixels
                    .get(&app.key)
                    .map(|p| &p[usize::from(app.muted)]),
            }
            .element(t)
        }));
    let list_height = panel_height(count, state.max_height()) - FRAME_HEIGHT;
    let overflow = list_height < ROW_HEIGHT * count as f32;
    let list = if overflow {
        col()
            .key("scroll")
            .w_full()
            .h(list_height)
            .scroll(ScrollSpec::default())
            .scroll_edge_fade(12.0)
            .child(content)
            .scroll_bar_styled(
                ScrollBarStyle::native(t.faint, t.sub)
                    .inside()
                    .always_visible(),
            )
    } else {
        col().key("scroll").w_full().h(list_height).child(content)
    };

    col()
        .key("panel")
        .w(WIDTH)
        .px(8.0)
        .pt(10.0)
        .pb(8.0)
        .radius(14.0)
        .fill(t.card)
        .child(header)
        .child(master)
        .child(el().key("master-gap").h(4.0))
        .child(
            el().key("separator")
                .w(WIDTH - 32.0)
                .h(1.0)
                .ml(8.0)
                .fill(t.line),
        )
        .child(el().key("list-gap").h(4.0))
        .child(list)
}

struct MixerRow<'a> {
    channel: Channel,
    name: &'a str,
    volume: f32,
    muted: bool,
    streams: usize,
    accent: Color,
    peak: Option<&'a Live<f32>>,
    pixels: Option<&'a Pixels>,
}

impl MixerRow<'_> {
    fn key(&self) -> String {
        match &self.channel {
            Channel::Master => "master".into(),
            Channel::App(key) => format!("app:{}", key.as_str()),
        }
    }

    fn element(&self, t: Theme) -> El {
        let master = matches!(self.channel, Channel::Master);
        let name_style = TextStyle::new(12.5, if master { 600.0 } else { 400.0 }).lh(16.0);
        let mut title = row()
            .key("name-line")
            .w(TRACK_WIDTH)
            .h(16.0)
            .gap(6.0)
            .items_center()
            .child(label(self.name, name_style, t.text).key("name").min_w(0.0));
        if self.streams > 1 {
            title = title.child(
                label(
                    format!("{} streams", self.streams),
                    TextStyle::new(11.0, 400.0).lh(16.0),
                    t.faint,
                )
                .key("streams"),
            );
        }

        let volume_text = if self.muted {
            "off".into()
        } else {
            format!("{:.0}", self.volume * 100.0)
        };
        let value_color = if self.muted { t.off } else { t.sub };
        let value_style = TextStyle::new(12.0, if self.muted { 600.0 } else { 400.0 })
            .lh(16.0)
            .features("'tnum' 1");
        let key = self.key();
        let wheel = self.channel.clone();
        let keyboard = self.channel.clone();
        let setter = self.channel.clone();
        row()
            .key(&key)
            .w_full()
            .h(if master { ROW_HEIGHT + 4.0 } else { ROW_HEIGHT })
            .px(8.0)
            .pt(7.0)
            .pb(if master { 11.0 } else { 7.0 })
            .gap(10.0)
            .items_center()
            .radius(9.0)
            .hover(t.hover)
            .focusable(true)
            .focus_ring(2.0, -1.0, self.accent.alpha(0.6))
            .role("slider")
            .label(self.name)
            .range_value(f64::from(self.volume * 100.0), 0.0, 100.0)
            .range_step(2.0)
            .a11y_value(if self.muted {
                "Muted".into()
            } else {
                format!("{volume_text} percent")
            })
            .on_set_value(move |s: &mut State, value, _| {
                s.change(&setter, Change::Set(value as f32 / 100.0))
            })
            .on_key(move |s: &mut State, event, tx| {
                let Some(change) = keyboard_change(&event.key) else {
                    return false;
                };
                if !matches!(change, Change::Mute) || !tx.key_repeat() {
                    s.change(&keyboard, change);
                }
                true
            })
            .on_wheel(move |s: &mut State, event, _| s.wheel(&wheel, event, Instant::now()))
            .child(self.icon(t, key.clone()))
            .child(
                col()
                    .key("control")
                    .w(TRACK_WIDTH)
                    .h(28.0)
                    .child(title)
                    .child(self.track(t, key)),
            )
            .child(
                row()
                    .key("value")
                    .size(36.0, 16.0)
                    .self_end()
                    .offset(0.0, 5.0)
                    .justify_end()
                    .child(label(volume_text, value_style, value_color).key("value-text")),
            )
    }

    fn icon(&self, t: Theme, row_key: String) -> El {
        let tile = match (self.pixels, &self.channel) {
            (Some(pixels), _) => el()
                .key("mark")
                .size(28.0, 28.0)
                .pixels_fit(pixels, rune::SurfaceFit::Fit),
            (None, Channel::Master) => canvas(
                Shape::new()
                    .view_box(16.0, 16.0)
                    .fill_svg("M2 6h2.6L8 3v10L4.6 10H2z", Color::WHITE)
                    .stroke_svg(
                        "M10.6 5.6a3.4 3.4 0 0 1 0 4.8",
                        1.5,
                        LineCap::Round,
                        Color::WHITE,
                    ),
            )
            .key("mark")
            .size(14.0, 14.0),
            (None, Channel::App(_)) => {
                let letter: String = self
                    .name
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .collect();
                label(letter, TextStyle::new(12.5, 700.0).lh(16.0), Color::WHITE).key("mark")
            }
        };
        let fallback = if self.muted {
            t.faint.alpha(0.4)
        } else {
            self.accent
        };
        let channel = self.channel.clone();
        let mut button = el()
            .key("mute")
            .size(28.0, 28.0)
            .radius(8.0)
            .center()
            .cursor(Cursor::Pointer)
            .role("button")
            .label(format!(
                "{} {}",
                if self.muted { "Unmute" } else { "Mute" },
                self.name
            ))
            .focusable(false)
            .on_press_tx(move |state: &mut State, tx| {
                state.change(&channel, Change::Mute);
                tx.focus(row_key.clone(), false);
            });
        if self.pixels.is_none() {
            button = button.fill(fallback);
        }
        button = button.child(if self.muted { tile.opacity(0.4) } else { tile });
        if self.muted {
            button = button.child(
                canvas(Shape::new().line([3.0, 3.0], [11.0, 11.0], 2.0, LineCap::Round, t.off))
                    .key("slash")
                    .size(14.0, 14.0)
                    .absolute(7.0, 7.0)
                    .a11y_hidden(),
            );
        }
        button
    }

    fn track(&self, t: Theme, row_key: String) -> El {
        let end = self.volume.clamp(0.0, 1.0) * TRACK_WIDTH;
        let channel = self.channel.clone();
        let background = el()
            .key("track")
            .size(TRACK_WIDTH, TRACK_HEIGHT)
            .absolute(0.0, 6.0)
            .radius(3.0)
            .fill(t.track)
            .a11y_hidden();
        let mut volume = el()
            .key("volume")
            .size(end, TRACK_HEIGHT)
            .absolute(0.0, 6.0)
            .radius(3.0)
            .fill(t.volume)
            .clip_rounded(3.0)
            .a11y_hidden();
        if let Some(peak) = self.peak {
            // A uniform live scale gives the peak its width. The track clips the height.
            volume = volume.child(
                el().key("level")
                    .size(TRACK_WIDTH, TRACK_WIDTH * TRACK_HEIGHT)
                    .absolute(0.0, 0.0)
                    .origin(0.0, 0.0)
                    .fill(if self.muted {
                        t.faint.alpha(0.7)
                    } else {
                        self.accent
                    })
                    .live_scale(peak),
            );
        }
        let knob = el()
            .key("knob")
            .size(14.0, 14.0)
            .absolute(end - 7.0, 2.0)
            .radius(7.0)
            .fill(t.knob)
            .border(1.0, t.knob_ring)
            .shadow(1.0, 3.0, Color::BLACK.alpha(0.25))
            .a11y_hidden();
        el().key("drag")
            .size(TRACK_WIDTH, 18.0)
            .cursor(Cursor::Pointer)
            .on_drag(move |state: &mut State, drag, tx| {
                if drag.phase == Phase::Down {
                    tx.focus(row_key.clone(), false);
                    drag.data = state
                        .channel_mut(&channel)
                        .map_or(0.0, |(volume, _)| *volume);
                }
                let value = if drag.cancelled() {
                    drag.data
                } else {
                    drag.x / drag.w
                };
                state.change(&channel, Change::Set(value));
            })
            .child(background)
            .child(volume)
            .child(knob)
    }
}

type Scene = (&'static str, fn(&mut State));

pub const SCENES: &[Scene] = &[
    ("light", |state| fake(state, false, false, 6)),
    ("dark", |state| fake(state, true, false, 6)),
    ("muted", |state| fake(state, false, true, 6)),
    ("scroll", |state| fake(state, true, true, 14)),
];

fn fake(state: &mut State, dark: bool, muted: bool, count: usize) {
    let fixtures = [
        ("Discord", 0.80, 0.42, [88, 101, 242]),
        ("Roblox Studio", 1.0, 0.25, [224, 72, 62]),
        ("Roblox Player", 0.65, 0.17, [58, 141, 222]),
        ("Flow Launcher", 1.0, 0.06, [47, 163, 127]),
        ("T3 Code", 0.40, 0.07, [138, 92, 246]),
        ("System sounds", 1.0, 0.02, [125, 132, 148]),
        ("Media Player", 0.55, 0.31, [205, 120, 70]),
        ("Microsoft Edge", 0.70, 0.29, [33, 158, 220]),
        ("Firefox", 0.75, 0.04, [241, 116, 38]),
        ("Spotify", 0.33, 0.18, [30, 200, 94]),
        ("Slack", 0.67, 0.0, [174, 50, 123]),
        ("OneNote", 0.24, 0.0, [138, 34, 158]),
        ("VLC", 0.42, 0.11, [236, 133, 40]),
        ("Terminal", 0.80, 0.03, [100, 124, 153]),
    ];
    state.scene = Some((dark, if count > 6 { 411.0 } else { 351.0 }));
    state.snapshot = Snapshot {
        device: "Speakers (Yeti Classic)".into(),
        master_volume: 0.50,
        master_muted: false,
        apps: fixtures
            .into_iter()
            .take(count)
            .enumerate()
            .map(|(i, (name, volume, peak, accent))| {
                let key = AppKey::for_process(i as u32 + 1, Some(Path::new(name)));
                state.peaks.insert(key.clone(), Live::new(peak));
                AppInfo {
                    key,
                    name: name.into(),
                    icon: None,
                    accent,
                    volume,
                    muted: muted && i == 1,
                    streams: if i == 3 { 2 } else { 1 },
                }
            })
            .collect(),
    };
    state.master_peak.set(0.33);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_accumulates_small_deltas_and_keeps_the_remainder() {
        let (steps, remainder) = scroll_steps(0.0, 7.5);
        assert_eq!(steps, 0.0);
        assert_eq!(scroll_steps(remainder, 29.0), (2.0, 4.5));
    }

    #[test]
    fn scroll_steps_down_and_cancels_opposite_deltas() {
        assert_eq!(scroll_steps(-6.0, -12.0), (-1.0, -2.0));
        assert_eq!(scroll_steps(10.0, -17.0), (0.0, -7.0));
    }

    #[test]
    fn a_new_gesture_or_channel_starts_without_the_previous_remainder() {
        let mut state = State::new();
        state.snapshot.master_volume = 0.5;
        let now = Instant::now();
        let master = Channel::Master;
        let other = Channel::App(AppKey::for_process(42, None));
        let delta = WheelEvent::new(0.0, 10.0).precise(true);

        assert!(state.wheel(&master, delta, now));
        state.wheel(&master, delta.phase(GesturePhase::Began), now);
        assert_eq!(state.snapshot.master_volume, 0.5);

        state.wheel(&other, delta, now);
        state.wheel(&master, delta, now);
        assert_eq!(state.snapshot.master_volume, 0.5);
        state.wheel(&master, delta, now);
        assert_eq!(state.snapshot.master_volume, 0.52);
    }

    #[test]
    fn fractional_windows_wheel_deltas_drop_their_remainder_after_a_pause() {
        let mut state = State::new();
        state.snapshot.master_volume = 0.5;
        let now = Instant::now();
        let delta = WheelEvent::new(0.0, -10.0);
        assert!(state.wheel(&Channel::Master, delta, now));
        let next = now + Duration::from_millis(150);
        state.wheel(&Channel::Master, delta, next);
        assert_eq!(state.snapshot.master_volume, 0.5);
        state.wheel(&Channel::Master, delta, next + Duration::from_millis(10));
        assert_eq!(state.snapshot.master_volume, 0.48);
    }

    #[test]
    fn momentum_is_consumed_and_a_notch_changes_volume_by_one_step() {
        let mut state = State::new();
        state.snapshot.master_volume = 0.5;
        let now = Instant::now();
        let mut momentum = WheelEvent::new(0.0, -64.0).precise(true);
        momentum.momentum = true;
        assert!(state.wheel(&Channel::Master, momentum, now));
        assert_eq!(state.snapshot.master_volume, 0.5);
        let notch = WheelEvent::new(0.0, 120.0).notch(true);
        assert!(state.wheel(&Channel::Master, notch, now));
        assert_eq!(state.snapshot.master_volume, 0.52);
    }

    #[test]
    fn clicking_the_tray_after_it_takes_focus_does_not_reopen() {
        let lost = Instant::now();
        assert!(same_click(Some(lost), lost + Duration::from_millis(190)));
        assert!(!same_click(Some(lost), lost + Duration::from_millis(400)));
        assert!(!same_click(None, lost + Duration::from_millis(40)));
    }

    #[test]
    fn panel_fits_a_few_rows_and_caps_the_list() {
        assert_eq!(panel_height(0, 400.0), 99.0);
        assert_eq!(panel_height(3, 400.0), 225.0);
        assert_eq!(panel_height(14, 400.0), 400.0);
    }
}
