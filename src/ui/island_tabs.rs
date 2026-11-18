//! Extra Dynamic Island tabs: Game mode, Notes & to-do, Countdowns & schedule, Weather.
//!
//! Notes, tasks, countdowns, the timetable and the "quiet while gaming" switch are saved
//! to %APPDATA%\Aurolo\planner.json. Games are found from the Riot, Epic and Steam installs.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::gfx::*;
use crate::platform;
use crate::state::*;
use crate::ui::{ic, Frame, Input, Key};

// ------------------------------------------------------------------- storage

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
struct Todo {
    text: String,
    done: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Countdown {
    title: String,
    /// days since 1970-01-01 (local date)
    day: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Class {
    /// 0 = Sunday … 6 = Saturday
    weekday: u8,
    /// minutes after midnight
    start: u32,
    end: u32,
    title: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct Planner {
    notes: String,
    todos: Vec<Todo>,
    countdowns: Vec<Countdown>,
    classes: Vec<Class>,
    quiet_gaming: bool,
    /// Focus lock end (unix seconds), 0 = off. Saved so a restart doesn't end it.
    focus_until: i64,
}

impl Default for Planner {
    fn default() -> Self {
        Planner { notes: String::new(), todos: vec![], countdowns: vec![], classes: vec![], quiet_gaming: true, focus_until: 0 }
    }
}

fn planner_path() -> PathBuf {
    crate::config::data_dir().join("planner.json")
}

fn demo_planner() -> Planner {
    let today = today_day();
    Planner {
        notes: "Ideas for the weekend\n- try the new ramen place\n- finish the island tabs".into(),
        todos: vec![
            Todo { text: "Submit lab report".into(), done: false },
            Todo { text: "Buy headphones case".into(), done: true },
            Todo { text: "Call the bank".into(), done: false },
        ],
        countdowns: vec![
            Countdown { title: "Physics exam".into(), day: today + 6 },
            Countdown { title: "Goa trip".into(), day: today + 23 },
        ],
        classes: vec![
            Class { weekday: weekday_of(today), start: 9 * 60, end: 10 * 60, title: "Maths".into() },
            Class { weekday: weekday_of(today), start: 11 * 60, end: 12 * 60 + 30, title: "Physics lab".into() },
            Class { weekday: weekday_of(today), start: 15 * 60, end: 16 * 60, title: "Gym".into() },
        ],
        quiet_gaming: true,
        focus_until: 0,
    }
}

fn today_day() -> i64 {
    let t = platform::local_time();
    platform::days_from_civil(t.year as i64, t.month as i64, t.day as i64)
}

fn weekday_of(day: i64) -> u8 {
    ((day + 4).rem_euclid(7)) as u8
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

fn month_of(s: &str) -> Option<i64> {
    let s = s.to_lowercase();
    let names = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    names.iter().position(|m| s.starts_with(m)).map(|i| i as i64 + 1)
}

/// "Physics exam 12 Dec" / "Trip in 10 days" / "Results tomorrow" / "Fest 2026-12-20"
/// -> (title, day). The date may be at the end or the start.
fn parse_countdown(s: &str) -> Option<(String, i64)> {
    let words: Vec<&str> = s.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let today = today_day();
    let t = platform::local_time();
    let next_year_if_past = |m: i64, d: i64| {
        let mut day = platform::days_from_civil(t.year as i64, m, d);
        if day < today {
            day = platform::days_from_civil(t.year as i64 + 1, m, d);
        }
        day
    };
    let lw: Vec<String> = words.iter().map(|w| w.to_lowercase().trim_matches(',').to_string()).collect();
    let n = lw.len();
    let title_of = |range: std::ops::Range<usize>| words[range].join(" ").trim_matches(|c: char| c == ',' || c == '-').trim().to_string();
    // "... in N days/weeks/months"
    if n >= 3 && lw[n - 3] == "in" {
        if let Ok(k) = lw[n - 2].parse::<i64>() {
            let mult = match lw[n - 1].trim_end_matches('s') {
                "day" => 1,
                "week" => 7,
                "month" => 30,
                _ => 0,
            };
            if mult > 0 {
                return Some((title_of(0..n - 3), today + k * mult));
            }
        }
    }
    if lw[n - 1] == "tomorrow" {
        return Some((title_of(0..n - 1), today + 1));
    }
    if lw[n - 1] == "today" {
        return Some((title_of(0..n - 1), today));
    }
    // "... 12 Dec" / "... Dec 12"
    if n >= 3 {
        let (a, b) = (&lw[n - 2], &lw[n - 1]);
        if let (Ok(d), Some(m)) = (a.parse::<i64>(), month_of(b)) {
            return Some((title_of(0..n - 2), next_year_if_past(m, d)));
        }
        if let (Some(m), Ok(d)) = (month_of(a), b.trim_end_matches(|c: char| !c.is_ascii_digit()).parse::<i64>()) {
            return Some((title_of(0..n - 2), next_year_if_past(m, d)));
        }
    }
    // "... 2026-12-20" or "... 20/12" or "... 20/12/2026"
    let last = &lw[n - 1];
    let parts: Vec<i64> = last.split(['-', '/', '.']).filter_map(|p| p.parse().ok()).collect();
    if n >= 2 && parts.len() == 3 && last.contains('-') {
        return Some((title_of(0..n - 1), platform::days_from_civil(parts[0], parts[1], parts[2])));
    }
    if n >= 2 && parts.len() == 3 {
        let y = if parts[2] < 100 { 2000 + parts[2] } else { parts[2] };
        return Some((title_of(0..n - 1), platform::days_from_civil(y, parts[1], parts[0])));
    }
    if n >= 2 && parts.len() == 2 {
        return Some((title_of(0..n - 1), next_year_if_past(parts[1], parts[0])));
    }
    None
}

/// "9:00", "9", "14:30", "2pm", "2:30pm" -> minutes after midnight
fn parse_time(s: &str) -> Option<u32> {
    let s = s.trim().to_lowercase();
    let (s, pm, am) = if let Some(x) = s.strip_suffix("pm") {
        (x.to_string(), true, false)
    } else if let Some(x) = s.strip_suffix("am") {
        (x.to_string(), false, true)
    } else {
        (s, false, false)
    };
    let mut it = s.split(':');
    let h: u32 = it.next()?.trim().parse().ok()?;
    let m: u32 = it.next().map(|m| m.trim().parse().unwrap_or(0)).unwrap_or(0);
    let h = if pm && h < 12 { h + 12 } else if am && h == 12 { 0 } else { h };
    if h < 24 && m < 60 { Some(h * 60 + m) } else { None }
}

/// "Mon 9:00-10:30 Maths" / "Mon,Wed 2pm-3pm Lab" / "weekdays 9-10 Standup"
fn parse_class(s: &str) -> Vec<Class> {
    let mut words = s.split_whitespace();
    let Some(days_w) = words.next() else { return vec![] };
    let Some(time_w) = words.next() else { return vec![] };
    let title = words.collect::<Vec<_>>().join(" ");
    let dl = days_w.to_lowercase();
    let days: Vec<u8> = if dl == "daily" || dl == "everyday" {
        (0..7).collect()
    } else if dl == "weekdays" {
        (1..6).collect()
    } else if dl == "weekends" {
        vec![0, 6]
    } else {
        dl.split(',')
            .filter_map(|d| {
                let names = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
                names.iter().position(|n| d.starts_with(n)).map(|i| i as u8)
            })
            .collect()
    };
    let (a, b) = time_w.split_once('-').unwrap_or((time_w, ""));
    let Some(start) = parse_time(a) else { return vec![] };
    let end = parse_time(b).unwrap_or(start + 60).max(start + 5);
    if days.is_empty() || title.is_empty() {
        return vec![];
    }
    days.into_iter().map(|weekday| Class { weekday, start, end, title: title.clone() }).collect()
}

fn hm(m: u32) -> String {
    format!("{}:{:02}", m / 60, m % 60)
}

// --------------------------------------------------------------------- games

#[derive(Clone, Debug)]
struct Game {
    name: String,
    /// What to open to launch it (exe path with args, or a URI).
    launch: String,
    args: String,
    /// File whose icon to show, or an image file to show as a cover.
    icon_src: Option<String>,
    cover: Option<PathBuf>,
    store: &'static str,
}

fn find_games() -> Vec<Game> {
    let mut out = vec![];
    // Riot: the client path is recorded in RiotClientInstalls.json
    let rc = std::fs::read_to_string(r"C:\ProgramData\Riot Games\RiotClientInstalls.json")
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v["rc_default"].as_str().or(v["rc_live"].as_str()).map(|s| s.replace('/', "\\")))
        .filter(|p| Path::new(p).exists())
        .or_else(|| Some(r"C:\Riot Games\Riot Client\RiotClientServices.exe".to_string()).filter(|p| Path::new(p).exists()));
    if let Some(rc) = rc {
        let league = [r"C:\Riot Games\League of Legends\LeagueClient.exe", r"C:\Riot Games\League of Legends\Live\LeagueClient.exe"]
            .into_iter()
            .find(|p| Path::new(p).exists());
        let mut riot: Vec<(&str, &str, Option<&str>)> = vec![];
        if Path::new(r"C:\Riot Games\VALORANT\live\VALORANT.exe").exists() {
            riot.push(("VALORANT", "valorant", Some(r"C:\Riot Games\VALORANT\live\VALORANT.exe")));
        }
        if let Some(l) = league {
            // Teamfight Tactics runs inside the League client
            riot.push(("League of Legends", "league_of_legends", Some(l)));
            riot.push(("Teamfight Tactics", "league_of_legends", Some(l)));
        }
        for (name, product, exe) in riot {
            out.push(Game {
                name: name.into(),
                launch: rc.clone(),
                args: format!("--launch-product={product} --launch-patchline=live"),
                icon_src: exe.map(String::from),
                cover: None,
                store: "Riot",
            });
        }
    }
    // Epic: one manifest per installed game
    if let Ok(rd) = std::fs::read_dir(r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests") {
        for e in rd.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) != Some("item") {
                continue;
            }
            let Ok(s) = std::fs::read_to_string(e.path()) else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else { continue };
            let cats: Vec<String> = v["AppCategories"].as_array().map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect()).unwrap_or_default();
            if !cats.iter().any(|c| c == "games") || v["bIsIncompleteInstall"].as_bool().unwrap_or(false) {
                continue;
            }
            let name = v["DisplayName"].as_str().unwrap_or("").to_string();
            let (ns, item, app) = (
                v["CatalogNamespace"].as_str().unwrap_or(""),
                v["CatalogItemId"].as_str().unwrap_or(""),
                v["AppName"].as_str().unwrap_or(""),
            );
            if name.is_empty() || app.is_empty() {
                continue;
            }
            let exe = Path::new(v["InstallLocation"].as_str().unwrap_or("")).join(v["LaunchExecutable"].as_str().unwrap_or(""));
            out.push(Game {
                name,
                launch: format!("com.epicgames.launcher://apps/{ns}%3A{item}%3A{app}?action=launch&silent=true"),
                args: String::new(),
                icon_src: Some(exe.to_string_lossy().to_string()).filter(|p| Path::new(p).is_file()),
                cover: None,
                store: "Epic",
            });
        }
    }
    // Steam: libraries listed in libraryfolders.vdf, games in appmanifest_*.acf
    let steam = steam_path();
    if let Some(steam) = steam {
        let mut libs = vec![steam.clone()];
        if let Ok(vdf) = std::fs::read_to_string(steam.join(r"steamapps\libraryfolders.vdf")) {
            for l in vdf.lines() {
                let l = l.trim();
                if let Some(rest) = l.strip_prefix("\"path\"") {
                    let p = rest.trim().trim_matches('"').replace("\\\\", "\\");
                    let pb = PathBuf::from(p);
                    if !libs.contains(&pb) {
                        libs.push(pb);
                    }
                }
            }
        }
        for lib in libs {
            let Ok(rd) = std::fs::read_dir(lib.join("steamapps")) else { continue };
            for e in rd.flatten() {
                let fname = e.file_name().to_string_lossy().to_string();
                if !fname.starts_with("appmanifest_") {
                    continue;
                }
                let Ok(s) = std::fs::read_to_string(e.path()) else { continue };
                let get = |k: &str| {
                    s.lines().find_map(|l| {
                        let l = l.trim();
                        l.strip_prefix(&format!("\"{k}\"")).map(|r| r.trim().trim_matches('"').to_string())
                    })
                };
                let (Some(id), Some(name)) = (get("appid"), get("name")) else { continue };
                // 228980 = Steamworks redistributables, 480 = Spacewar (a test app some games install)
                if id == "228980" || id == "480" || name.to_lowercase().contains("redistributable") || name.to_lowercase().contains("steamworks") {
                    continue;
                }
                let cache = steam.join(r"appcache\librarycache");
                let cover = [cache.join(format!("{id}_header.jpg")), cache.join(&id).join("header.jpg")]
                    .into_iter()
                    .find(|p| p.exists());
                out.push(Game {
                    name,
                    launch: format!("steam://rungameid/{id}"),
                    args: String::new(),
                    icon_src: None,
                    cover,
                    store: "Steam",
                });
            }
        }
    }
    // the same game can be listed twice (several libraries)
    let mut seen = std::collections::HashSet::new();
    out.retain(|g| seen.insert(g.name.to_lowercase()));
    out
}

fn steam_path() -> Option<PathBuf> {
    use windows::core::HSTRING;
    use windows::Win32::System::Registry::*;
    unsafe {
        let mut buf = [0u16; 520];
        let mut size = (buf.len() * 2) as u32;
        let r = RegGetValueW(
            HKEY_CURRENT_USER,
            &HSTRING::from("Software\\Valve\\Steam"),
            &HSTRING::from("SteamPath"),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut _),
            Some(&mut size),
        );
        if r.is_err() {
            return None;
        }
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        let p = PathBuf::from(String::from_utf16_lossy(&buf[..n]).replace('/', "\\"));
        if p.exists() { Some(p) } else { None }
    }
}

// ---------------------------------------------------------------------- ping

const PING_TARGETS: [(&str, &str); 3] = [
    ("Mumbai · Valorant", "dynamodb.ap-south-1.amazonaws.com:443"),
    ("Singapore", "dynamodb.ap-southeast-1.amazonaws.com:443"),
    ("Internet", "one.one.one.one:443"),
];

#[derive(Clone, Default)]
struct PingRow {
    last: Option<f32>,
    hist: VecDeque<f32>,
}

/// Background pinger (TCP connect time ≈ round-trip). Runs while the Game tab was
/// viewed in the last few seconds.
fn spawn_pinger(rows: Arc<Mutex<Vec<PingRow>>>, viewed: Arc<AtomicU64>) {
    let _ = std::thread::Builder::new().name("ping".into()).spawn(move || {
        use std::net::{TcpStream, ToSocketAddrs};
        loop {
            let now = platform::unix_now() as u64;
            if now.saturating_sub(viewed.load(Ordering::Relaxed)) > 5 {
                std::thread::sleep(Duration::from_millis(700));
                continue;
            }
            for (k, (_, host)) in PING_TARGETS.iter().enumerate() {
                let ms = host.to_socket_addrs().ok().and_then(|mut a| a.next()).and_then(|addr| {
                    let t = Instant::now();
                    TcpStream::connect_timeout(&addr, Duration::from_millis(1500)).ok().map(|_| t.elapsed().as_secs_f32() * 1000.0)
                });
                let mut r = rows.lock();
                if let Some(row) = r.get_mut(k) {
                    row.last = ms;
                    row.hist.push_back(ms.unwrap_or(0.0));
                    while row.hist.len() > 30 {
                        row.hist.pop_front();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(1500));
        }
    });
}

// ---------------------------------------------------------------- the tabs

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Edit {
    Note,
    Todo,
    Countdown,
    Class,
    City,
}

pub enum ExtraAction {
    /// Save the weather city to the settings file.
    SaveCity(String),
}

pub struct Extras {
    planner: Planner,
    edit: Option<Edit>,
    input: String,
    caret_t: f32,
    error: Option<(String, f32)>,
    games: Arc<Mutex<Option<Vec<Game>>>>,
    games_started: bool,
    game_scroll: f32,
    ping: Arc<Mutex<Vec<PingRow>>>,
    ping_viewed: Arc<AtomicU64>,
    ping_started: bool,
    icons: HashMap<String, Option<u64>>,
    sched_day: Option<u8>,
    notes_scroll: f32,
    lock: crate::focuslock::FocusLock,
    lock_choice: i64,
    hold_since: Option<f32>,
    blocked_count: u32,
}

fn fmt_mins(m: i64) -> String {
    match m {
        m if m >= 60 && m % 60 == 0 => format!("{} h", m / 60),
        m if m >= 60 => format!("{} h {} min", m / 60, m % 60),
        m => format!("{m} min"),
    }
}

fn key_of(s: &str) -> u64 {
    s.bytes().fold(0x9e37_79b9_7f4a_7c15u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)) ^ 0x1515_0000_0000_0000
}

impl Extras {
    pub fn new() -> Self {
        let planner = if crate::demo::is_demo() {
            demo_planner()
        } else {
            std::fs::read_to_string(planner_path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
        };
        let demo_lock = crate::demo::is_demo() && std::env::var("AUROLO_DEMO_LOCK").is_ok();
        let until = if demo_lock { platform::unix_now() + 42 * 60 } else if crate::demo::is_demo() { 0 } else { planner.focus_until };
        let lock = crate::focuslock::spawn(until);
        Extras {
            lock,
            lock_choice: 50,
            hold_since: None,
            blocked_count: 0,
            planner,
            edit: None,
            input: String::new(),
            caret_t: 0.0,
            error: None,
            games: Arc::new(Mutex::new(None)),
            games_started: false,
            game_scroll: 0.0,
            ping: Arc::new(Mutex::new(vec![PingRow::default(); PING_TARGETS.len()])),
            ping_viewed: Arc::new(AtomicU64::new(0)),
            ping_started: false,
            icons: HashMap::new(),
            sched_day: None,
            notes_scroll: 0.0,
        }
    }

    /// Typing into one of these tabs: the island needs keyboard focus.
    pub fn editing(&self) -> bool {
        self.edit.is_some()
    }

    pub fn stop_editing(&mut self) {
        if self.edit == Some(Edit::Note) {
            self.save();
        }
        self.edit = None;
        self.input.clear();
    }

    /// Silence island popups while a full-screen game is up.
    pub fn quiet_gaming(&self) -> bool {
        self.planner.quiet_gaming
    }

    /// Seconds left on the focus lock, if one is running.
    pub fn lock_left(&self) -> Option<i64> {
        let left = self.lock.until.load(Ordering::Relaxed) - platform::unix_now();
        if left > 0 { Some(left) } else { None }
    }

    fn set_lock(&mut self, until: i64) {
        self.lock.until.store(until, Ordering::Relaxed);
        self.planner.focus_until = until;
        self.save();
    }

    /// Every frame: report what the lock blocked, and note when it runs out.
    pub fn tick(&mut self, shared: &Shared) {
        while let Ok(name) = self.lock.blocked.try_recv() {
            self.blocked_count += 1;
            shared.hud(Hud::Message("Blocked · focus lock".into(), name));
        }
        if self.planner.focus_until != 0 && self.lock_left().is_none() {
            self.planner.focus_until = 0;
            self.save();
            shared.hud(Hud::Message("Focus lock".into(), "Done, games are unlocked".into()));
        }
    }

    /// Next class today or nearest countdown, for the compact island.
    pub fn next_up(&self) -> Option<String> {
        if let Some(left) = self.lock_left() {
            let m = (left + 59) / 60;
            return Some(if m >= 60 { format!("Focus lock · {}h {:02}m", m / 60, m % 60) } else { format!("Focus lock · {m} min left") });
        }
        let t = platform::local_time();
        let now_m = t.hour as u32 * 60 + t.minute as u32;
        let wd = t.weekday as u8;
        if let Some(c) = self.planner.classes.iter().filter(|c| c.weekday == wd && c.end > now_m).min_by_key(|c| c.start) {
            return Some(if c.start <= now_m {
                format!("{} · until {}", c.title, hm(c.end))
            } else {
                format!("{} at {}", c.title, hm(c.start))
            });
        }
        let today = today_day();
        self.planner
            .countdowns
            .iter()
            .filter(|c| c.day >= today)
            .min_by_key(|c| c.day)
            .map(|c| match c.day - today {
                0 => format!("{} · today", c.title),
                1 => format!("{} · tomorrow", c.title),
                n => format!("{} · {n} days", c.title),
            })
    }

    fn save(&self) {
        if crate::demo::is_demo() {
            return;
        }
        if let Ok(s) = serde_json::to_string_pretty(&self.planner) {
            let _ = std::fs::write(planner_path(), s);
        }
    }

    fn begin(&mut self, e: Edit) {
        if self.edit == Some(Edit::Note) && e != Edit::Note {
            self.save();
        }
        self.edit = Some(e);
        self.input.clear();
        self.caret_t = 0.0;
    }

    /// Feed typed characters into the current single-line input. Returns true on Enter.
    fn type_line(&mut self, input: &Input, f: &Frame) -> bool {
        self.caret_t += f.dt;
        for c in input.text.chars() {
            if !c.is_control() && self.input.chars().count() < 120 {
                self.input.push(c);
                self.caret_t = 0.0;
            }
        }
        if input.key(Key::Back) {
            self.input.pop();
        }
        input.key(Key::Enter) && !self.input.trim().is_empty()
    }

    /// A rounded input field. Returns (clicked, rect).
    fn field(&mut self, f: &mut Frame, input: &Input, r: [f32; 4], e: Edit, hint: &str, pal: &Palette, a: f32) -> bool {
        let on = self.edit == Some(e);
        let hov = input.over(r);
        f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 11.0, [1.0, 1.0, 1.0, if on { 0.13 } else if hov { 0.1 } else { 0.07 } * a]));
        if on {
            f.scene.shape(Shape::stroke(r[0], r[1], r[2], r[3], 11.0, 1.0, with_a(pal.primary, 0.7 * a)));
        }
        let (shown, sa) = if on && !self.input.is_empty() { (self.input.clone(), 0.95) } else { (hint.to_string(), 0.4) };
        let t = f.fit(Text::new(shown, r[0] + 12.0, r[1] + (r[3] - 16.0) / 2.0 - 1.0, 12.0, with_a(WHITE, sa * a)), r[2] - 24.0);
        let tw = if on && !self.input.is_empty() { f.measure(&t).0 } else { 0.0 };
        f.scene.text(t);
        if on && (self.caret_t * 2.0) as i32 % 2 == 0 {
            f.scene.shape(Shape::rect(r[0] + 12.0 + tw + 1.0, r[1] + (r[3] - 16.0) / 2.0, 1.5, 16.0, 0.7, with_a(WHITE, 0.9 * a)));
        }
        if input.clicked(r) {
            if !on {
                self.begin(e);
            }
            return true;
        }
        false
    }

    fn card(f: &mut Frame, r: [f32; 4], title: &str, g: char, a: f32) {
        f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 20.0, [1.0, 1.0, 1.0, 0.055 * a]));
        f.icon(g, r[0] + 22.0, r[1] + 22.0, 12.0, with_a(WHITE, 0.55 * a));
        f.scene.text(Text::new(title, r[0] + 38.0, r[1] + 13.0, 12.5, with_a(WHITE, 0.7 * a)).weight(600));
    }

    fn show_error(&mut self, f: &mut Frame, x: f32, y: f32, a: f32) {
        if let Some((e, t0)) = &self.error {
            if f.t - t0 > 3.0 {
                self.error = None;
            } else {
                f.scene.text(Text::new(e.clone(), x, y, 11.0, with_a(rgba(255, 140, 140, 1.0), a)));
            }
        }
    }

    // ------------------------------------------------------------- weather

    pub fn tab_weather(&mut self, f: &mut Frame, input: &Input, shared: &Shared, pal: &Palette, area: [f32; 4], a: f32) -> Option<ExtraAction> {
        let [x, y, w, h] = area;
        let wt = shared.misc.read().weather.clone();
        let city = shared.weather_city.read().clone();
        let mut out = None;
        let asking = city.trim().is_empty() || self.edit == Some(Edit::City);
        if asking {
            let cw = 380.0;
            let cx = x + (w - cw) / 2.0;
            f.icon('\u{E706}', x + w / 2.0, y + 70.0, 26.0, with_a(rgba(255, 204, 0, 1.0), a));
            f.scene.text(Text::new("Which city's weather?", x + w / 2.0, y + 104.0, 15.0, with_a(WHITE, 0.92 * a)).weight(650).center());
            f.scene.text(Text::new("Type your city and press Enter", x + w / 2.0, y + 128.0, 11.5, with_a(WHITE, 0.5 * a)).center());
            if self.edit != Some(Edit::City) {
                self.begin(Edit::City);
            }
            self.field(f, input, [cx, y + 156.0, cw, 36.0], Edit::City, "e.g. Dehradun, London, Tokyo", pal, a);
            if self.type_line(input, f) {
                let c = self.input.trim().to_string();
                *shared.weather_city.write() = c.clone();
                out = Some(ExtraAction::SaveCity(c));
                self.edit = None;
                self.input.clear();
            }
            if !city.trim().is_empty() && f.icon_button(input, ic::CLOSE, cx + cw + 22.0, y + 174.0, 10.0, with_a(WHITE, 0.6 * a)) {
                self.edit = None;
                self.input.clear();
            }
            return out;
        }
        let Some(wt) = wt else {
            let dots = ".".repeat(1 + (f.t * 2.0) as usize % 3);
            f.scene.text(Text::new(format!("Getting the forecast for {city}{dots}"), x + w / 2.0, y + h / 2.0 - 10.0, 13.0, with_a(WHITE, 0.6 * a)).center());
            return None;
        };
        let gap = 12.0;
        let lw = w * 0.34;
        // ---- now
        let r = [x, y, lw, h];
        f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 20.0, [1.0, 1.0, 1.0, 0.055 * a]));
        f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 20.0, with_a(pal.primary, 0.06 * a)).grad(with_a(pal.secondary, 0.0), 1.2));
        let ct = f.fit(Text::new(&wt.city, x + 18.0, y + 14.0, 12.5, with_a(WHITE, 0.7 * a)).weight(600), lw - 80.0);
        f.scene.text(ct);
        let chg = Text::new("Change", x + lw - 16.0, y + 15.0, 11.0, with_a(WHITE, 0.45 * a)).right();
        let cwid = f.measure(&chg).0;
        let chr = [x + lw - 22.0 - cwid, y + 8.0, cwid + 12.0, 26.0];
        f.scene.text(Text { color: with_a(WHITE, if input.over(chr) { 0.9 } else { 0.45 } * a), ..chg });
        if input.clicked(chr) {
            self.begin(Edit::City);
        }
        crate::ui::island::weather_icon(f, x + 52.0, y + 80.0, 20.0, wt.code, wt.is_day, a);
        f.scene.text(Text::new(format!("{:.0}°", wt.temp), x + 92.0, y + 50.0, 46.0, with_a(WHITE, a)).family(Family::Display).weight(300));
        f.scene.text(Text::new(crate::services::web::weather_desc(wt.code), x + 18.0, y + 118.0, 14.0, with_a(WHITE, 0.9 * a)).weight(600));
        f.scene.text(Text::new(format!("H {:.0}°  ·  L {:.0}°", wt.hi, wt.lo), x + 18.0, y + 140.0, 12.0, with_a(WHITE, 0.6 * a)));
        let stats = [
            ('\u{E9CA}', format!("Feels {:.0}°", wt.feels)),
            ('\u{E945}', format!("Humidity {}%", wt.humidity)),
            ('\u{E9F3}', format!("Wind {:.0} km/h", wt.wind)),
        ];
        for (k, (_g, s)) in stats.iter().enumerate() {
            f.scene.text(Text::new(s.clone(), x + 18.0, y + 172.0 + k as f32 * 20.0, 11.5, with_a(WHITE, 0.72 * a)));
        }
        // sunrise / sunset + AQI
        let by = y + h - 40.0;
        f.scene.shape(Shape::glow(x + 26.0, by + 10.0, 5.0, with_a(rgba(255, 190, 80, 1.0), a), 0.6));
        f.scene.text(Text::new(format!("{}  →  {}", wt.sunrise, wt.sunset), x + 40.0, by + 2.0, 11.5, with_a(WHITE, 0.75 * a)));
        if let Some(q) = wt.aqi {
            let (label, c) = match q {
                0..=50 => ("Good", rgba(90, 210, 120, 1.0)),
                51..=100 => ("Moderate", rgba(240, 200, 70, 1.0)),
                101..=150 => ("Unhealthy*", rgba(250, 140, 60, 1.0)),
                151..=200 => ("Unhealthy", rgba(240, 80, 80, 1.0)),
                _ => ("Very poor", rgba(170, 80, 200, 1.0)),
            };
            let t = Text::new(format!("AQI {q} · {label}"), 0.0, 0.0, 11.0, WHITE).weight(600);
            let tw = f.measure(&t).0 + 18.0;
            let px = x + lw - 14.0 - tw;
            f.scene.shape(Shape::rect(px, by - 2.0, tw, 24.0, 12.0, with_a(c, 0.22 * a)));
            f.scene.text(Text { x: px + tw / 2.0, y: by + 2.5, color: with_a(mix(c, WHITE, 0.4), a), ..t }.center());
        }

        // ---- hourly
        let rx = x + lw + gap;
        let rw = w - lw - gap;
        let hh = h * 0.5 - gap / 2.0;
        f.scene.shape(Shape::rect(rx, y, rw, hh, 20.0, [1.0, 1.0, 1.0, 0.055 * a]));
        let hours: Vec<_> = wt.hourly.iter().take(12).cloned().collect();
        if !hours.is_empty() {
            let n = hours.len();
            let colw = (rw - 24.0) / n as f32;
            let (tmin, tmax) = hours.iter().fold((f32::MAX, f32::MIN), |(lo, hi), x| (lo.min(x.1), hi.max(x.1)));
            let span = (tmax - tmin).max(1.0);
            let mut pts = vec![];
            for (k, (hr, tmp, code, rain, day)) in hours.iter().enumerate() {
                let cx = rx + 12.0 + colw * (k as f32 + 0.5);
                let label = if k == 0 { "Now".to_string() } else { format!("{hr:02}") };
                f.scene.text(Text::new(label, cx, y + 12.0, 10.5, with_a(WHITE, if k == 0 { 0.9 } else { 0.55 } * a)).center().weight(600));
                crate::ui::island::weather_icon(f, cx, y + 44.0, 7.5, *code, *day, a);
                let ty = y + hh - 34.0 - 22.0 * (tmp - tmin) / span;
                pts.push((cx, ty));
                f.scene.text(Text::new(format!("{tmp:.0}°"), cx, ty - 18.0, 11.5, with_a(WHITE, 0.9 * a)).center().weight(600));
                if *rain >= 20 {
                    f.scene.text(Text::new(format!("{rain}%"), cx, y + hh - 18.0, 9.5, with_a(rgba(120, 190, 255, 1.0), 0.9 * a)).center());
                }
            }
            for k in 1..pts.len() {
                let mut l = Shape::line(pts[k - 1].0, pts[k - 1].1, pts[k].0, pts[k].1, 1.6, with_a(pal.primary, 0.8 * a));
                l.c1 = with_a(pal.secondary, 0.8 * a);
                f.scene.shape(l);
            }
            for p in &pts {
                f.scene.shape(Shape::rect(p.0 - 2.5, p.1 - 2.5, 5.0, 5.0, 2.5, with_a(WHITE, 0.9 * a)));
            }
        }
        // ---- next days
        let dy0 = y + hh + gap;
        let dh = h - hh - gap;
        f.scene.shape(Shape::rect(rx, dy0, rw, dh, 20.0, [1.0, 1.0, 1.0, 0.055 * a]));
        let days: Vec<_> = wt.daily.iter().take(5).cloned().collect();
        if !days.is_empty() {
            let (lo_all, hi_all) = days.iter().fold((f32::MAX, f32::MIN), |(lo, hi), d| (lo.min(d.2), hi.max(d.1)));
            let span = (hi_all - lo_all).max(1.0);
            let rowh = (dh - 16.0) / days.len() as f32;
            for (k, (wd, hi, lo, code, rain)) in days.iter().enumerate() {
                let ry = dy0 + 8.0 + rowh * k as f32;
                let cy = ry + rowh / 2.0;
                let name = if k == 0 { "Today" } else { DAYS[*wd as usize % 7] };
                f.scene.text(Text::new(name, rx + 18.0, cy - 8.0, 12.0, with_a(WHITE, 0.85 * a)).weight(600));
                crate::ui::island::weather_icon(f, rx + 92.0, cy, 7.0, *code, true, a);
                if *rain >= 20 {
                    f.scene.text(Text::new(format!("{rain}%"), rx + 112.0, cy - 7.0, 10.5, with_a(rgba(120, 190, 255, 1.0), a)));
                }
                f.scene.text(Text::new(format!("{lo:.0}°"), rx + 172.0, cy - 8.0, 12.0, with_a(WHITE, 0.55 * a)).right());
                let bx = rx + 182.0;
                let bw = rw - 182.0 - 56.0;
                f.scene.shape(Shape::rect(bx, cy - 2.5, bw, 5.0, 2.5, with_a(WHITE, 0.10 * a)));
                let s0 = (lo - lo_all) / span;
                let s1 = (hi - lo_all) / span;
                f.scene.shape(
                    Shape::rect(bx + bw * s0, cy - 2.5, (bw * (s1 - s0)).max(6.0), 5.0, 2.5, with_a(rgba(110, 190, 255, 1.0), a))
                        .grad(with_a(rgba(255, 180, 80, 1.0), a), 0.0),
                );
                f.scene.text(Text::new(format!("{hi:.0}°"), rx + rw - 18.0, cy - 8.0, 12.0, with_a(WHITE, 0.9 * a)).right().weight(600));
            }
        }
        out
    }

    // ------------------------------------------------------------- notes

    pub fn tab_notes(&mut self, f: &mut Frame, input: &Input, pal: &Palette, area: [f32; 4], a: f32) {
        let [x, y, w, h] = area;
        let gap = 12.0;
        let nw = w * 0.52;
        // ---- notes: a free-form scratchpad
        let nr = [x, y, nw, h];
        Self::card(f, nr, "Notes", '\u{E70B}', a);
        let pad = [x + 12.0, y + 40.0, nw - 24.0, h - 52.0];
        let on = self.edit == Some(Edit::Note);
        let hov = input.over(pad);
        f.scene.shape(Shape::rect(pad[0], pad[1], pad[2], pad[3], 14.0, [1.0, 1.0, 1.0, if on { 0.09 } else if hov { 0.07 } else { 0.04 } * a]));
        if on {
            f.scene.shape(Shape::stroke(pad[0], pad[1], pad[2], pad[3], 14.0, 1.0, with_a(pal.primary, 0.6 * a)));
            f.scene.text(Text::new("Saved automatically", x + nw - 16.0, y + 14.0, 10.5, with_a(WHITE, 0.4 * a)).right());
        }
        if input.clicked(pad) && !on {
            self.begin(Edit::Note);
        }
        if on {
            self.caret_t += f.dt;
            let mut changed = false;
            for c in input.text.chars() {
                if !c.is_control() {
                    self.planner.notes.push(c);
                    changed = true;
                }
            }
            if input.key(Key::Enter) {
                self.planner.notes.push('\n');
                changed = true;
            }
            if input.key(Key::Back) {
                self.planner.notes.pop();
                changed = true;
            }
            if changed {
                self.caret_t = 0.0;
                self.save();
            }
        }
        if input.over(pad) && input.scroll.abs() > 0.01 {
            self.notes_scroll = (self.notes_scroll - input.scroll * 30.0).max(0.0);
        }
        let empty = self.planner.notes.is_empty();
        let body = if empty { "Click to write anything — ideas, links, a shopping list…".to_string() } else { self.planner.notes.clone() };
        let t = Text::new(body, pad[0] + 14.0, pad[1] + 12.0 - self.notes_scroll, 13.0, with_a(WHITE, if empty { 0.38 } else { 0.9 } * a))
            .max_w(pad[2] - 28.0)
            .lh(1.4)
            .clip([pad[0], pad[1] + 4.0, pad[0] + pad[2], pad[1] + pad[3] - 4.0]);
        let (_, th) = f.measure(&t);
        f.scene.text(t);
        if on && (self.caret_t * 2.0) as i32 % 2 == 0 {
            // caret after the last line
            let last = self.planner.notes.rsplit('\n').next().unwrap_or("");
            let lines = self.planner.notes.matches('\n').count() as f32;
            let lw = f.measure(&Text::new(last, 0.0, 0.0, 13.0, WHITE)).0.min(pad[2] - 28.0);
            let cy = pad[1] + 12.0 - self.notes_scroll + if self.planner.notes.ends_with('\n') || empty { lines * 18.2 } else { (th - 18.2).max(0.0) };
            if cy > pad[1] && cy < pad[1] + pad[3] - 18.0 {
                f.scene.shape(Shape::rect(pad[0] + 14.0 + if self.planner.notes.ends_with('\n') || empty { 0.0 } else { lw } + 1.0, cy + 1.0, 1.5, 16.0, 0.7, with_a(WHITE, 0.9 * a)));
            }
        }
        // keep the caret in view
        if on && th > pad[3] - 24.0 {
            self.notes_scroll = self.notes_scroll.max(th - (pad[3] - 24.0));
        }

        // ---- to-do
        let tx = x + nw + gap;
        let tw = w - nw - gap;
        let tr = [tx, y, tw, h];
        Self::card(f, tr, "To-do", '\u{E73A}', a);
        let open = self.planner.todos.iter().filter(|t| !t.done).count();
        if !self.planner.todos.is_empty() {
            f.scene.text(Text::new(format!("{open} left"), tx + tw - 16.0, y + 14.0, 11.0, with_a(WHITE, 0.45 * a)).right());
        }
        self.field(f, input, [tx + 12.0, y + 40.0, tw - 24.0, 32.0], Edit::Todo, "Add a task and press Enter", pal, a);
        if self.edit == Some(Edit::Todo) && self.type_line(input, f) {
            let text = self.input.trim().to_string();
            self.planner.todos.insert(0, Todo { text, done: false });
            self.planner.todos.truncate(60);
            self.input.clear();
            self.save();
        }
        let mut toggle = None;
        let mut del = None;
        let row_h = 32.0;
        let max_rows = ((h - 82.0 - 34.0) / row_h) as usize;
        // open tasks first, then done ones
        let mut order: Vec<usize> = (0..self.planner.todos.len()).collect();
        order.sort_by_key(|&i| self.planner.todos[i].done);
        for (k, &i) in order.iter().take(max_rows).enumerate() {
            let td = &self.planner.todos[i];
            let ry = y + 82.0 + k as f32 * row_h;
            let row = [tx + 10.0, ry, tw - 20.0, row_h - 3.0];
            let rh = input.over(row);
            if rh {
                f.scene.shape(Shape::rect(row[0], row[1], row[2], row[3], 10.0, [1.0, 1.0, 1.0, 0.06 * a]));
            }
            let (cx, cy) = (row[0] + 16.0, ry + row_h / 2.0 - 1.5);
            if td.done {
                f.scene.shape(Shape::rect(cx - 8.0, cy - 8.0, 16.0, 16.0, 8.0, with_a(pal.primary, a)));
                f.icon('\u{E73E}', cx, cy, 9.0, with_a(BLACK, a));
            } else {
                f.scene.shape(Shape::stroke(cx - 8.0, cy - 8.0, 16.0, 16.0, 8.0, 1.5, with_a(WHITE, 0.6 * a)));
            }
            let t = f.fit(Text::new(&td.text, cx + 18.0, cy - 9.0, 12.5, with_a(WHITE, if td.done { 0.4 } else { 0.9 } * a)), row[2] - 70.0);
            if td.done {
                let lw = f.measure(&t).0;
                f.scene.shape(Shape::rect(cx + 18.0, cy, lw, 1.2, 0.6, with_a(WHITE, 0.4 * a)));
            }
            f.scene.text(t);
            if rh && f.icon_button(input, ic::CLOSE, row[0] + row[2] - 14.0, cy, 8.5, with_a(WHITE, 0.6 * a)) {
                del = Some(i);
            } else if input.clicked(row) {
                toggle = Some(i);
            }
        }
        if self.planner.todos.iter().any(|t| t.done) {
            let (c, _) = f.pill(input, "Clear done", tx + tw - 104.0, y + h - 38.0, 26.0, false, with_a(WHITE, a));
            if c {
                self.planner.todos.retain(|t| !t.done);
                self.save();
            }
        }
        if let Some(i) = del {
            self.planner.todos.remove(i);
            self.save();
        } else if let Some(i) = toggle {
            self.planner.todos[i].done = !self.planner.todos[i].done;
            self.save();
        }
        if self.planner.todos.is_empty() {
            f.scene.text(Text::new("Nothing to do — nice.", tx + tw / 2.0, y + 130.0, 12.0, with_a(WHITE, 0.4 * a)).center());
        }
    }

    // ------------------------------------------------------- countdowns + schedule

    pub fn tab_plan(&mut self, f: &mut Frame, input: &Input, pal: &Palette, area: [f32; 4], a: f32) {
        let [x, y, w, h] = area;
        let gap = 12.0;
        let cw = w * 0.46;
        let today = today_day();
        // ---- countdowns
        let cr = [x, y, cw, h];
        Self::card(f, cr, "Countdowns", ic::TIMER, a);
        self.field(f, input, [x + 12.0, y + 40.0, cw - 24.0, 32.0], Edit::Countdown, "e.g. \u{201C}Exam 12 Dec\u{201D}, \u{201C}Trip in 10 days\u{201D}", pal, a);
        if self.edit == Some(Edit::Countdown) && self.type_line(input, f) {
            match parse_countdown(&self.input) {
                Some((title, day)) if !title.is_empty() => {
                    self.planner.countdowns.push(Countdown { title, day });
                    self.planner.countdowns.sort_by_key(|c| c.day);
                    self.input.clear();
                    self.save();
                }
                _ => self.error = Some(("Add a name and a date, e.g. \u{201C}Exam 12 Dec\u{201D}".into(), f.t)),
            }
        }
        // drop ones that are over
        let before = self.planner.countdowns.len();
        self.planner.countdowns.retain(|c| c.day >= today - 1);
        if self.planner.countdowns.len() != before {
            self.save();
        }
        let mut del = None;
        for (k, c) in self.planner.countdowns.iter().take(4).enumerate() {
            let ry = y + 82.0 + k as f32 * 54.0;
            let row = [x + 10.0, ry, cw - 20.0, 48.0];
            let rh = input.over(row);
            f.scene.shape(Shape::rect(row[0], row[1], row[2], row[3], 12.0, [1.0, 1.0, 1.0, if rh { 0.08 } else { 0.045 } * a]));
            let left = c.day - today;
            let (big, small) = match left {
                ..=0 => ("Today".to_string(), String::new()),
                1 => ("1".to_string(), "day".to_string()),
                n => (n.to_string(), "days".to_string()),
            };
            let bt = Text::new(big, row[0] + 34.0, ry + 4.0, if left <= 0 { 15.0 } else { 24.0 }, with_a(WHITE, a)).weight(700).center();
            f.scene.text(Text { y: if left <= 0 { ry + 14.0 } else { ry + 3.0 }, ..bt });
            if !small.is_empty() {
                f.scene.text(Text::new(small, row[0] + 34.0, ry + 31.0, 9.5, with_a(WHITE, 0.5 * a)).center());
            }
            let urgency = if left <= 3 { rgba(255, 130, 110, 1.0) } else if left <= 10 { rgba(255, 200, 90, 1.0) } else { pal.primary };
            f.scene.shape(Shape::rect(row[0] + 62.0, ry + 10.0, 2.0, 28.0, 1.0, with_a(urgency, 0.9 * a)));
            let tt = f.fit(Text::new(&c.title, row[0] + 74.0, ry + 7.0, 13.0, with_a(WHITE, 0.92 * a)).weight(600), row[2] - 110.0);
            f.scene.text(tt);
            let (yy, mm, dd) = civil_from_days(c.day);
            let _ = yy;
            let date = format!("{} {} {}", DAYS[weekday_of(c.day) as usize], dd, platform::MONTHS[(mm as usize + 11) % 12]);
            f.scene.text(Text::new(date, row[0] + 74.0, ry + 26.0, 11.0, with_a(WHITE, 0.5 * a)));
            if rh && f.icon_button(input, ic::CLOSE, row[0] + row[2] - 16.0, ry + 24.0, 8.5, with_a(WHITE, 0.6 * a)) {
                del = Some(k);
            }
        }
        if let Some(k) = del {
            self.planner.countdowns.remove(k);
            self.save();
        }
        if self.planner.countdowns.is_empty() {
            f.scene.text(Text::new("Count down to exams, deadlines, trips…", x + cw / 2.0, y + 130.0, 12.0, with_a(WHITE, 0.4 * a)).center());
        }
        self.show_error(f, x + 18.0, y + h - 24.0, a);

        // ---- schedule
        let sx = x + cw + gap;
        let sw = w - cw - gap;
        let sr = [sx, y, sw, h];
        Self::card(f, sr, "Schedule", ic::CALENDAR, a);
        let t = platform::local_time();
        let wd_today = t.weekday as u8;
        let view = self.sched_day.unwrap_or(wd_today);
        // day chips
        let chip_w = (sw - 24.0 - 6.0 * 4.0) / 7.0;
        for d in 0..7u8 {
            let dd = (wd_today + d) % 7; // start from today
            let cx = sx + 12.0 + d as f32 * (chip_w + 4.0);
            let r = [cx, y + 40.0, chip_w, 24.0];
            let on = dd == view;
            let hov = input.over(r);
            let has = self.planner.classes.iter().any(|c| c.weekday == dd);
            f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 12.0, if on { with_a(pal.primary, 0.85 * a) } else { [1.0, 1.0, 1.0, if hov { 0.1 } else { 0.05 } * a] }));
            let label = if d == 0 { "Today" } else { DAYS[dd as usize] };
            let size = if d == 0 { 9.5 } else { 10.5 };
            f.scene.text(Text::new(label, cx + chip_w / 2.0, y + 45.0, size, with_a(if on { BLACK } else { WHITE }, if on { 0.9 } else { 0.75 } * a)).center().weight(600));
            if has && !on {
                f.scene.shape(Shape::rect(cx + chip_w / 2.0 - 1.5, y + 66.0, 3.0, 3.0, 1.5, with_a(WHITE, 0.5 * a)));
            }
            if input.clicked(r) {
                self.sched_day = Some(dd);
            }
        }
        let now_m = t.hour as u32 * 60 + t.minute as u32;
        let mut list: Vec<(usize, &Class)> = self.planner.classes.iter().enumerate().filter(|(_, c)| c.weekday == view).collect();
        list.sort_by_key(|(_, c)| c.start);
        let mut del = None;
        let row_h = 34.0;
        let max_rows = ((h - 76.0 - 50.0) / row_h) as usize;
        for (k, (i, c)) in list.iter().take(max_rows).enumerate() {
            let ry = y + 76.0 + k as f32 * row_h;
            let row = [sx + 10.0, ry, sw - 20.0, row_h - 4.0];
            let rh = input.over(row);
            let live = view == wd_today && c.start <= now_m && now_m < c.end;
            let past = view == wd_today && c.end <= now_m;
            if live {
                f.scene.shape(Shape::rect(row[0], row[1], row[2], row[3], 10.0, with_a(pal.primary, 0.22 * a)));
            } else if rh {
                f.scene.shape(Shape::rect(row[0], row[1], row[2], row[3], 10.0, [1.0, 1.0, 1.0, 0.06 * a]));
            }
            let ta = if past { 0.4 } else { 0.9 };
            f.scene.text(Text::new(format!("{}–{}", hm(c.start), hm(c.end)), row[0] + 10.0, ry + 7.0, 11.5, with_a(WHITE, 0.6 * ta * a)).family(Family::Mono));
            let tt = f.fit(Text::new(&c.title, row[0] + 104.0, ry + 6.0, 12.5, with_a(WHITE, ta * a)).weight(600), row[2] - 150.0);
            f.scene.text(tt);
            if live {
                f.scene.text(Text::new("now", row[0] + row[2] - 34.0, ry + 7.0, 10.5, with_a(mix(pal.primary, WHITE, 0.5), a)).right().weight(700));
            }
            if rh && f.icon_button(input, ic::CLOSE, row[0] + row[2] - 14.0, ry + row_h / 2.0 - 2.0, 8.0, with_a(WHITE, 0.6 * a)) {
                del = Some(*i);
            }
        }
        if list.is_empty() {
            f.scene.text(Text::new("Nothing on this day", sx + sw / 2.0, y + 120.0, 12.0, with_a(WHITE, 0.4 * a)).center());
        }
        if let Some(i) = del {
            self.planner.classes.remove(i);
            self.save();
        }
        self.field(f, input, [sx + 12.0, y + h - 44.0, sw - 24.0, 32.0], Edit::Class, "Add: \u{201C}Mon 9-10 Maths\u{201D}, \u{201C}weekdays 2pm-3pm Lab\u{201D}", pal, a);
        if self.edit == Some(Edit::Class) && self.type_line(input, f) {
            let added = parse_class(&self.input);
            if added.is_empty() {
                self.error = Some(("Try \u{201C}Mon 9-10 Maths\u{201D} (day, time, name)".into(), f.t));
            } else {
                self.sched_day = Some(added[0].weekday);
                self.planner.classes.extend(added);
                self.input.clear();
                self.save();
            }
        }
    }

    // ------------------------------------------------------------- game mode

    fn icon_for(&mut self, f: &mut Frame, g: &Game) -> Option<u64> {
        let id = g.cover.as_ref().map(|p| p.to_string_lossy().to_string()).or_else(|| g.icon_src.clone())?;
        if !self.icons.contains_key(&id) {
            let px = if let Some(c) = &g.cover {
                crate::desktop::thumbnail(c, 300)
            } else {
                crate::desktop::shell_icon(&id).or_else(|| crate::shell::file_icon(&id))
            };
            let k = key_of(&id);
            let v = px.map(|p| {
                f.painter.upload_image(f.gpu, k, p.0, p.1, &p.2);
                k
            });
            self.icons.insert(id.clone(), v);
        }
        let k = (*self.icons.get(&id)?)?;
        if !f.painter.has_image(k) {
            self.icons.remove(&id);
            return None;
        }
        f.painter.touch_image(k);
        Some(k)
    }

    pub fn tab_game(&mut self, f: &mut Frame, input: &Input, shared: &Shared, pal: &Palette, area: [f32; 4], a: f32) {
        let [x, y, w, h] = area;
        let gap = 12.0;
        if !self.games_started {
            self.games_started = true;
            let slot = self.games.clone();
            let _ = std::thread::Builder::new().name("games".into()).spawn(move || {
                let g = find_games();
                *slot.lock() = Some(g);
            });
        }
        if !self.ping_started {
            self.ping_started = true;
            spawn_pinger(self.ping.clone(), self.ping_viewed.clone());
        }
        self.ping_viewed.store(platform::unix_now() as u64, Ordering::Relaxed);

        // ---- games
        let gw = w * 0.56;
        let gr = [x, y, gw, h];
        Self::card(f, gr, "Play", '\u{E7FC}', a);
        let games = self.games.lock().clone();
        match games {
            None => {
                f.scene.text(Text::new("Looking for your games…", x + gw / 2.0, y + h / 2.0, 12.0, with_a(WHITE, 0.5 * a)).center());
            }
            Some(list) if list.is_empty() => {
                f.scene.text(Text::new("No Riot, Epic or Steam games found", x + gw / 2.0, y + h / 2.0, 12.0, with_a(WHITE, 0.5 * a)).center());
            }
            Some(list) => {
                let cols = 3;
                let tile_w = (gw - 24.0 - 10.0 * (cols as f32 - 1.0)) / cols as f32;
                let tile_h = (h - 52.0 - 10.0) / 2.0;
                let rows_total = (list.len() + cols - 1) / cols;
                let max_scroll = ((rows_total as f32 - 2.0) * (tile_h + 10.0)).max(0.0);
                if input.over(gr) && input.scroll.abs() > 0.01 {
                    self.game_scroll = (self.game_scroll - input.scroll * 40.0).clamp(0.0, max_scroll);
                }
                let clip = [x, y + 38.0, x + gw, y + h - 6.0];
                for (k, g) in list.iter().enumerate() {
                    let tx = x + 12.0 + (k % cols) as f32 * (tile_w + 10.0);
                    let ty = y + 40.0 + (k / cols) as f32 * (tile_h + 10.0) - self.game_scroll;
                    if ty + tile_h < clip[1] || ty > clip[3] {
                        continue;
                    }
                    let r = [tx, ty, tile_w, tile_h];
                    let hov = input.over(r) && input.over([clip[0], clip[1], clip[2] - clip[0], clip[3] - clip[1]]);
                    f.scene.shape(Shape::rect(tx, ty, tile_w, tile_h, 14.0, [1.0, 1.0, 1.0, if hov { 0.12 } else { 0.06 } * a]));
                    let img = self.icon_for(f, g);
                    match (img, &g.cover) {
                        (Some(k), Some(_)) => {
                            f.scene.image(ImageDraw::new(k, tx, ty, tile_w, tile_h - 26.0, 14.0).opacity(a));
                        }
                        (Some(k), None) => {
                            let s = (tile_h - 48.0).min(46.0);
                            f.scene.image(ImageDraw::new(k, tx + (tile_w - s) / 2.0, ty + 12.0 + (tile_h - 40.0 - s) / 2.0, s, s, 8.0).opacity(a));
                        }
                        _ => {
                            let (c0, c1) = crate::ui::app_colors(&g.name);
                            let s = (tile_h - 48.0).min(46.0);
                            let (ix, iy) = (tx + (tile_w - s) / 2.0, ty + 12.0 + (tile_h - 40.0 - s) / 2.0);
                            f.scene.shape(Shape::rect(ix, iy, s, s, 12.0, with_a(c0, a)).grad(with_a(c1, a), 0.8));
                            let ini = g.name.chars().next().unwrap_or('?').to_string();
                            f.scene.text(Text::new(ini, ix + s / 2.0, iy + s * 0.18, s * 0.5, with_a(WHITE, a)).center().weight(700));
                        }
                    }
                    let nt = f.fit(Text::new(&g.name, tx + tile_w / 2.0, ty + tile_h - 22.0, 11.5, with_a(WHITE, 0.92 * a)).center().weight(600), tile_w - 10.0);
                    f.scene.text(Text { clip: Some(clip), ..nt });
                    f.scene.text(Text::new(g.store, tx + tile_w - 8.0, ty + 6.0, 9.0, with_a(WHITE, 0.45 * a)).right().clip(clip));
                    if hov {
                        f.scene.shape(Shape::stroke(tx, ty, tile_w, tile_h, 14.0, 1.2, with_a(pal.primary, 0.8 * a)));
                    }
                    if self.lock_left().is_some() {
                        f.scene.shape(Shape::rect(tx, ty, tile_w, tile_h, 14.0, [0.0, 0.0, 0.0, 0.45 * a]));
                        f.icon('\u{E72E}', tx + tile_w / 2.0, ty + (tile_h - 26.0) / 2.0 + 6.0, 16.0, with_a(WHITE, 0.85 * a));
                    }
                    if hov && input.clicked(r) && self.lock_left().is_some() {
                        shared.hud(Hud::Message("Focus lock".into(), "Games are locked right now".into()));
                    } else if hov && input.clicked(r) {
                        if g.args.is_empty() {
                            platform::open(&g.launch);
                        } else {
                            platform::open_with_args(&g.launch, &g.args);
                        }
                        shared.hud(Hud::Message("Launching".into(), g.name.clone()));
                    }
                }
            }
        }

        // ---- ping
        let rx = x + gw + gap;
        let rw = w - gw - gap;
        let ph = 128.0;
        let pr = [rx, y, rw, ph];
        Self::card(f, pr, "Ping", '\u{E701}', a);
        let rows = self.ping.lock().clone();
        for (k, ((name, _), row)) in PING_TARGETS.iter().zip(rows.iter()).enumerate() {
            let ry = y + 40.0 + k as f32 * 28.0;
            f.scene.text(Text::new(*name, rx + 16.0, ry + 4.0, 11.5, with_a(WHITE, 0.7 * a)));
            let (txt, c) = match row.last {
                None if row.hist.is_empty() => ("…".to_string(), with_a(WHITE, 0.5)),
                None => ("timeout".to_string(), rgba(255, 110, 110, 1.0)),
                Some(ms) => (
                    format!("{ms:.0} ms"),
                    if ms < 60.0 { rgba(110, 220, 140, 1.0) } else if ms < 120.0 { rgba(250, 200, 80, 1.0) } else { rgba(255, 110, 110, 1.0) },
                ),
            };
            f.scene.text(Text::new(txt, rx + rw - 16.0, ry + 3.0, 12.5, with_a(c, a)).right().weight(700).family(Family::Mono));
            // sparkline
            let sx0 = rx + 138.0;
            let sw = rw - 138.0 - 80.0;
            if sw > 30.0 && row.hist.len() > 1 {
                let maxv = row.hist.iter().cloned().fold(30.0f32, f32::max);
                let n = row.hist.len();
                for i in 1..n {
                    let (v0, v1) = (row.hist[i - 1], row.hist[i]);
                    let x0 = sx0 + sw * (i - 1) as f32 / 29.0;
                    let x1 = sx0 + sw * i as f32 / 29.0;
                    let y0 = ry + 18.0 - 12.0 * v0 / maxv;
                    let y1 = ry + 18.0 - 12.0 * v1 / maxv;
                    f.scene.shape(Shape::line(x0, y0, x1, y1, 1.2, with_a(c, 0.6 * a)));
                }
            }
        }

        // ---- focus lock + quiet switch
        let fy0 = y + ph + gap;
        let fh = h - ph - gap;
        let fr = [rx, fy0, rw, fh];
        Self::card(f, fr, "Focus lock", '\u{E72E}', a);
        let lock_c = rgba(120, 170, 255, 1.0);
        match self.lock_left() {
            Some(left) => {
                f.scene.shape(Shape::rect(fr[0], fr[1], fr[2], fr[3], 20.0, with_a(lock_c, 0.08 * a)));
                let mins = (left + 59) / 60;
                let big = if mins >= 60 { format!("{}h {:02}m", mins / 60, mins % 60) } else { format!("{mins} min") };
                f.scene.text(Text::new(big, rx + 18.0, fy0 + 40.0, 26.0, with_a(WHITE, a)).weight(700).family(Family::Display));
                f.scene.text(Text::new("left · games and launchers are blocked", rx + 18.0, fy0 + 76.0, 11.0, with_a(WHITE, 0.6 * a)));
                if self.blocked_count > 0 {
                    let n = self.blocked_count;
                    f.scene.text(Text::new(format!("Stopped {n} launch{}", if n == 1 { "" } else { "es" }), rx + rw - 16.0, fy0 + 14.0, 10.5, with_a(lock_c, 0.9 * a)).right());
                }
                // hold to end early: a little friction on purpose
                let br = [rx + 14.0, fy0 + fh - 82.0, rw - 28.0, 32.0];
                let holding = input.down && input.over(br);
                if holding {
                    self.hold_since.get_or_insert(f.t);
                } else {
                    self.hold_since = None;
                }
                let p = self.hold_since.map(|t0| ((f.t - t0) / 3.0).clamp(0.0, 1.0)).unwrap_or(0.0);
                f.scene.shape(Shape::rect(br[0], br[1], br[2], br[3], 16.0, [1.0, 1.0, 1.0, if input.over(br) { 0.1 } else { 0.06 } * a]));
                if p > 0.0 {
                    f.scene.shape(Shape::rect(br[0], br[1], (br[2] * p).max(br[3]), br[3], 16.0, with_a(rgba(255, 120, 110, 1.0), 0.45 * a)));
                }
                let label = if p > 0.0 { "Keep holding…" } else { "Hold 3 s to end early" };
                f.scene.text(Text::new(label, br[0] + br[2] / 2.0, br[1] + 8.0, 12.0, with_a(WHITE, 0.85 * a)).center().weight(600));
                if p >= 1.0 {
                    self.set_lock(0);
                    self.hold_since = None;
                    shared.hud(Hud::Message("Focus lock".into(), "Ended early".into()));
                }
            }
            None => {
                f.scene.text(Text::new("Block games and game launchers for a while", rx + 18.0, fy0 + 40.0, 11.0, with_a(WHITE, 0.6 * a)));
                let opts: [(&str, i64); 4] = [("25m", 25), ("50m", 50), ("1½h", 90), ("2h", 120)];
                let cw = (rw - 28.0 - 3.0 * 6.0) / 4.0;
                for (k, (lab, mins)) in opts.iter().enumerate() {
                    let cx = rx + 14.0 + k as f32 * (cw + 6.0);
                    let r = [cx, fy0 + 62.0, cw, 28.0];
                    let on = self.lock_choice == *mins;
                    let hov = input.over(r);
                    f.scene.shape(Shape::rect(r[0], r[1], r[2], r[3], 14.0, if on { with_a(lock_c, 0.9 * a) } else { [1.0, 1.0, 1.0, if hov { 0.12 } else { 0.06 } * a] }));
                    f.scene.text(Text::new(*lab, cx + cw / 2.0, fy0 + 68.0, 12.0, with_a(if on { BLACK } else { WHITE }, if on { 0.9 } else { 0.8 } * a)).center().weight(650));
                    if input.clicked(r) {
                        self.lock_choice = *mins;
                    }
                }
                let sr = [rx + 14.0, fy0 + fh - 82.0, rw - 28.0, 32.0];
                let hov = input.over(sr);
                f.scene.shape(Shape::rect(sr[0], sr[1], sr[2], sr[3], 16.0, with_a(lock_c, if hov { 1.0 } else { 0.85 } * a)));
                f.scene.text(Text::new(format!("Start focus lock · {}", fmt_mins(self.lock_choice)), sr[0] + sr[2] / 2.0, sr[1] + 8.0, 12.0, with_a(BLACK, 0.9 * a)).center().weight(700));
                if input.clicked(sr) {
                    let until = platform::unix_now() + self.lock_choice * 60;
                    self.set_lock(until);
                    self.blocked_count = 0;
                    shared.hud(Hud::Message("Focus lock on".into(), format!("Games blocked for {}", fmt_mins(self.lock_choice))));
                }
            }
        }
        // quiet while gaming
        let qy = fy0 + fh - 42.0;
        let qr = [rx + 10.0, qy, rw - 20.0, 32.0];
        let on = self.planner.quiet_gaming;
        f.scene.text(Text::new("Quiet while gaming", rx + 18.0, qy + 8.0, 12.0, with_a(WHITE, 0.85 * a)).weight(600));
        let swx = rx + rw - 62.0;
        f.scene.shape(Shape::rect(swx, qy + 6.0, 40.0, 20.0, 10.0, if on { with_a(pal.primary, a) } else { with_a(WHITE, 0.15 * a) }));
        let kx = if on { swx + 30.0 } else { swx + 10.0 };
        f.scene.shape(Shape::rect(kx - 7.0, qy + 9.0, 14.0, 14.0, 7.0, with_a(WHITE, a)));
        if input.clicked(qr) {
            self.planner.quiet_gaming = !on;
            self.save();
        }
    }
}
