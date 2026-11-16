use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LyricStyle {
    Karaoke,
    Constellation,
    Drift,
}

impl LyricStyle {
    pub const ALL: [LyricStyle; 3] = [LyricStyle::Constellation, LyricStyle::Karaoke, LyricStyle::Drift];
    pub fn name(self) -> &'static str {
        match self {
            LyricStyle::Karaoke => "Karaoke",
            LyricStyle::Constellation => "Constellation",
            LyricStyle::Drift => "3D Drift",
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
    /// Translucent glass island (true) or solid black pill (false).
    pub island_glass: bool,
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
            island_glass: false,
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
        if self.widget_lyrics_only {
            self.widget_size_lyrics.unwrap_or((520.0, 120.0))
        } else {
            self.widget_size_full.unwrap_or((560.0, 210.0))
        }
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
