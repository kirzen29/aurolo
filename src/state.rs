//! State shared between background services and the UI thread.

use parking_lot::{Mutex, RwLock};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Instant;

use crate::gfx::Rgba;

#[derive(Clone, Debug)]
pub struct Palette {
    pub primary: Rgba,
    pub secondary: Rgba,
    pub dark: Rgba,
    pub light: Rgba,
}

impl Default for Palette {
    fn default() -> Self {
        Palette {
            primary: [0.55, 0.45, 1.0, 1.0],
            secondary: [0.25, 0.75, 1.0, 1.0],
            dark: [0.05, 0.04, 0.10, 1.0],
            light: [0.92, 0.90, 1.0, 1.0],
        }
    }
}

pub struct Art {
    pub id: u64,
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
    pub blur_w: u32,
    pub blur_h: u32,
    pub blur: Vec<u8>,
    pub palette: Palette,
}

#[derive(Clone, Default)]
pub struct MediaState {
    pub active: bool,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub app: String,
    pub playing: bool,
    pub position: f64,
    pub pos_at: Option<Instant>,
    pub duration: f64,
    pub art: Option<Arc<Art>>,
    pub track_id: u64,
}

impl MediaState {
    pub fn pos_now(&self) -> f64 {
        match (self.playing, self.pos_at) {
            (true, Some(at)) => {
                let p = self.position + at.elapsed().as_secs_f64();
                if self.duration > 0.0 { p.min(self.duration) } else { p }
            }
            _ => self.position,
        }
    }
    pub fn palette(&self) -> Palette {
        self.art.as_ref().map(|a| a.palette.clone()).unwrap_or_default()
    }
}

#[derive(Clone, Debug)]
pub struct Word {
    pub t: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub t: f64,
    pub end: f64,
    pub text: String,
    pub words: Vec<Word>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum LyricStatus {
    #[default]
    Idle,
    Loading,
    Synced,
    Plain,
    NotFound,
    Instrumental,
}

#[derive(Clone, Default)]
pub struct LyricsState {
    pub track_id: u64,
    pub status: LyricStatus,
    pub lines: Vec<Line>,
}

impl LyricsState {
    /// Index of the line active at `pos` (or the last line before it).
    pub fn line_at(&self, pos: f64) -> Option<usize> {
        if self.lines.is_empty() {
            return None;
        }
        if self.status == LyricStatus::Plain {
            return None;
        }
        let idx = self.lines.partition_point(|l| l.t <= pos);
        if idx == 0 { None } else { Some(idx - 1) }
    }
}

pub const BANDS: usize = 48;

#[derive(Clone)]
pub struct AudioState {
    pub bands: [f32; BANDS],
    pub level: f32,
    pub bass: f32,
    pub beat: f32,
    pub last_beat: Option<Instant>,
}

impl Default for AudioState {
    fn default() -> Self {
        AudioState { bands: [0.0; BANDS], level: 0.0, bass: 0.0, beat: 0.0, last_beat: None }
    }
}

impl AudioState {
    /// Beat pulse 1 -> 0 decaying after each detected beat.
    pub fn pulse(&self) -> f32 {
        match self.last_beat {
            Some(t) => (-t.elapsed().as_secs_f32() * 5.0).exp() * self.beat,
            None => 0.0,
        }
    }
}

#[derive(Clone, Default)]
pub struct Battery {
    pub percent: f32,
    pub charging: bool,
    pub on_ac: bool,
}

#[derive(Clone, Default)]
pub struct SysState {
    pub cpu_name: String,
    pub cpu: f32,
    pub cpu_ghz: f32,
    pub cores: usize,
    pub ram_used: f64,
    pub ram_total: f64,
    pub gpu: Option<f32>,
    pub gpu_name: String,
    pub cpu_temp: Option<f32>,
    pub gpu_temp: Option<f32>,
    pub fans: Vec<(String, f32)>,
    pub cpu_power: Option<f32>,
    pub net_down: f64,
    pub net_up: f64,
    pub disk_used: f64,
    pub disk_total: f64,
    pub battery: Option<Battery>,
    pub cpu_hist: VecDeque<f32>,
    pub net_hist: VecDeque<f32>,
    pub lhm_ok: bool,
    pub uptime: u64,
    pub top_procs: Vec<(String, f32)>,
}

#[derive(Clone, Debug)]
pub enum Hud {
    Volume { level: f32, muted: bool },
    Brightness(f32),
    Battery { percent: f32, charging: bool },
    Wifi(Option<String>),
    Bluetooth { name: String, battery: Option<u8> },
    Notification { app: String, title: String, body: String },
    TimerDone(String),
    Download(String),
    Copied(String),
    Message(String, String),
}

#[derive(Clone, Debug)]
pub struct Notif {
    pub app: String,
    pub title: String,
    pub body: String,
    pub at: Instant,
}

#[derive(Clone, Debug)]
pub struct Weather {
    pub city: String,
    pub temp: f32,
    pub hi: f32,
    pub lo: f32,
    pub code: i32,
    pub is_day: bool,
}

#[derive(Clone, Debug)]
pub struct Event {
    /// unix seconds (UTC)
    pub start: i64,
    pub title: String,
    pub all_day: bool,
}

#[derive(Clone, Debug)]
pub struct Download {
    pub name: String,
    pub bytes: u64,
    pub speed: f64,
}

#[derive(Clone, Debug)]
pub struct BtDevice {
    pub name: String,
    pub battery: u8,
}

#[derive(Clone, Default)]
pub struct Misc {
    pub volume: f32,
    pub muted: bool,
    pub brightness: Option<f32>,
    pub wifi: Option<String>,
    pub cam_in_use: Option<String>,
    pub mic_in_use: Option<String>,
    pub clipboard: Vec<String>,
    pub weather: Option<Weather>,
    pub events: Vec<Event>,
    pub downloads: Vec<Download>,
    pub notifications: Vec<Notif>,
    pub notif_access: Option<bool>,
    pub bt_devices: Vec<BtDevice>,
    pub apps: Vec<(String, String)>, // (name, path) for the launcher
}

#[derive(Debug, Clone)]
pub enum MediaCmd {
    PlayPause,
    Next,
    Prev,
    Seek(f64),
}

pub struct Shared {
    pub media: RwLock<MediaState>,
    pub lyrics: RwLock<LyricsState>,
    pub audio: Mutex<AudioState>,
    pub sys: RwLock<SysState>,
    pub misc: RwLock<Misc>,
    pub huds: Mutex<VecDeque<Hud>>,
    pub media_tx: Mutex<Option<Sender<MediaCmd>>>,
    pub lyrics_offset: RwLock<f64>,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Shared {
            media: RwLock::new(MediaState::default()),
            lyrics: RwLock::new(LyricsState::default()),
            audio: Mutex::new(AudioState::default()),
            sys: RwLock::new(SysState::default()),
            misc: RwLock::new(Misc::default()),
            huds: Mutex::new(VecDeque::new()),
            media_tx: Mutex::new(None),
            lyrics_offset: RwLock::new(0.0),
        })
    }
    pub fn hud(&self, h: Hud) {
        let mut q = self.huds.lock();
        q.push_back(h);
        while q.len() > 8 {
            q.pop_front();
        }
    }
    pub fn media_cmd(&self, c: MediaCmd) {
        if let Some(tx) = self.media_tx.lock().as_ref() {
            let _ = tx.send(c);
        }
    }
    /// Playback position used for lyric sync (includes user offset).
    pub fn lyric_pos(&self) -> f64 {
        self.media.read().pos_now() + *self.lyrics_offset.read()
    }
}
