use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LyricStyle {
    Karaoke,
    Constellation,
    Drift,
    /// Album banner on the left, lyrics scrolling up beside it (Spotify-style), beat-synced.
    Banner,
    /// Words pop out one by one exactly as they're sung.
    Pop,
}

impl LyricStyle {
    pub const ALL: [LyricStyle; 5] =
        [LyricStyle::Constellation, LyricStyle::Karaoke, LyricStyle::Banner, LyricStyle::Pop, LyricStyle::Drift];
    pub fn name(self) -> &'static str {
        match self {
            LyricStyle::Karaoke => "Karaoke",
            LyricStyle::Constellation => "Constellation",
            LyricStyle::Drift => "3D Drift",
            LyricStyle::Banner => "Banner",
            LyricStyle::Pop => "Pop",
        }
    }
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WidgetBg {
    Glass,
    Solid,
    None,
}

impl WidgetBg {
    pub const ALL: [WidgetBg; 3] = [WidgetBg::Glass, WidgetBg::Solid, WidgetBg::None];
    pub fn name(self) -> &'static str {
        match self {
            WidgetBg::Glass => "Glass",
            WidgetBg::Solid => "Solid",
            WidgetBg::None => "No background",
        }
    }
    pub fn next(self) -> Self {
        let i = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Self::ALL[(i + 1) % Self::ALL.len()]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Social {
    pub name: String,
    /// Deep link or URL opened when the tile is clicked.
    pub url: String,
    /// Substring matched against notification app names to count unread messages.
    pub match_app: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub island: bool,
    pub lyrics_widget: bool,
    pub edge_glow: bool,
    pub widget_style: LyricStyle,
    pub fullscreen_style: LyricStyle,
    /// Font family used for lyrics ("" = Segoe UI Variable Display).
    pub lyrics_font: String,
    pub lyrics_offset_ms: i64,
    pub widget_pos: Option<(i32, i32)>,
    /// true = lyrics only, false = full card with art, title and controls.
    pub widget_lyrics_only: bool,
    pub widget_background: WidgetBg,
    pub widget_size_full: Option<(f32, f32)>,
    pub widget_size_lyrics: Option<(f32, f32)>,
    pub widget_glass: bool,
    pub edge_thickness: f32,
    pub edge_intensity: f32,
    pub edge_only_when_playing: bool,
    pub weather_city: String,
    pub calendar_ics_url: String,
    pub lhm_url: String,
    pub pomodoro_minutes: u32,
    pub socials: Vec<Social>,
    pub autostart: bool,
    pub hotkey_lyrics: String,
    pub hotkey_island: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            island: true,
            lyrics_widget: true,
            edge_glow: true,
            widget_style: LyricStyle::Karaoke,
            fullscreen_style: LyricStyle::Constellation,
            lyrics_font: String::new(),
            lyrics_offset_ms: 0,
            widget_pos: None,
            widget_lyrics_only: false,
            widget_background: WidgetBg::Glass,
            widget_size_full: None,
            widget_size_lyrics: None,
            widget_glass: true,
            edge_thickness: 26.0,
            edge_intensity: 0.85,
            edge_only_when_playing: true,
            weather_city: String::new(),
            calendar_ics_url: String::new(),
            lhm_url: "http://localhost:8085/data.json".into(),
            pomodoro_minutes: 25,
            socials: vec![
                Social {
                    name: "Discord".into(),
                    url: "discord://-/channels/@me".into(),
                    match_app: "discord".into(),
                },
                Social {
                    name: "Instagram".into(),
                    url: "https://www.instagram.com/direct/inbox/".into(),
                    match_app: "instagram".into(),
                },
                Social {
                    name: "WhatsApp".into(),
                    url: "whatsapp://".into(),
                    match_app: "whatsapp".into(),
                },
            ],
            autostart: false,
            hotkey_lyrics: "Ctrl+Alt+L".into(),
            hotkey_island: "Ctrl+Alt+Space".into(),
        }
    }
}

/// Lyrics widget size presets: (name, full layout size, lyrics-only size), logical px.
pub const WIDGET_PRESETS: [(&str, (f32, f32), (f32, f32)); 3] = [
    ("Small", (400.0, 155.0), (340.0, 76.0)),
    ("Medium", (490.0, 182.0), (430.0, 98.0)),
    ("Large", (620.0, 232.0), (580.0, 130.0)),
];

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let d = base.join("Aurolo");
    // one-time migration from the project's old name
    let old = base.join("Aura");
    if !d.exists() && old.join("config.toml").exists() {
        let _ = std::fs::rename(&old, &d);
    }
    let _ = std::fs::create_dir_all(&d);
    d
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

impl Config {
    pub fn load() -> Self {
        let p = config_path();
        match std::fs::read_to_string(&p) {
            Ok(s) => toml::from_str(&s).unwrap_or_else(|e| {
                crate::gfx::log(&format!("config parse error: {e}"));
                Config::default()
            }),
            Err(_) => {
                let c = Config::default();
                c.save();
                c
            }
        }
    }
    /// Logical size of the lyrics widget for the current layout.
    pub fn widget_size(&self) -> (f32, f32) {
        let m = WIDGET_PRESETS[1];
        if self.widget_lyrics_only {
            self.widget_size_lyrics.unwrap_or(m.2)
        } else {
            self.widget_size_full.unwrap_or(m.1)
        }
    }

    /// Store a size for the current widget layout.
    pub fn set_widget_size(&mut self, s: (f32, f32)) {
        let s = (s.0.round(), s.1.round());
        if self.widget_lyrics_only {
            self.widget_size_lyrics = Some(s);
        } else {
            self.widget_size_full = Some(s);
        }
    }

    /// Index of the preset matching the current size, if any.
    pub fn widget_preset(&self) -> Option<usize> {
        let cur = self.widget_size();
        WIDGET_PRESETS.iter().position(|p| {
            let s = if self.widget_lyrics_only { p.2 } else { p.1 };
            (s.0 - cur.0).abs() < 1.0 && (s.1 - cur.1).abs() < 1.0
        })
    }

    pub fn widget_size_name(&self) -> &'static str {
        self.widget_preset().map(|i| WIDGET_PRESETS[i].0).unwrap_or("Custom")
    }

    /// Apply preset `i` (0 = Small, 1 = Medium, 2 = Large) and return the new size.
    pub fn apply_widget_preset(&mut self, i: usize) -> (f32, f32) {
        let p = WIDGET_PRESETS[i.min(WIDGET_PRESETS.len() - 1)];
        let s = if self.widget_lyrics_only { p.2 } else { p.1 };
        self.set_widget_size(s);
        s
    }
    pub fn save(&self) {
        if crate::demo::is_demo() {
            return; // demo mode never touches the user's settings
        }
        if let Ok(s) = toml::to_string_pretty(self) {
            let _ = std::fs::write(config_path(), s);
        }
    }
}
