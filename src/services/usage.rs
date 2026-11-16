//! Screen time: which app is in the foreground, accumulated per day.
//! Time is not counted while the user is idle (no input for 2 minutes).
//! Saved to %APPDATA%\Aurolo\usage\YYYY-MM-DD.json.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::platform;
use crate::state::Shared;

const TICK: u64 = 2;
const IDLE_LIMIT: u64 = 120;

fn path_for(day: &str) -> std::path::PathBuf {
    let d = crate::config::data_dir().join("usage");
    let _ = std::fs::create_dir_all(&d);
    d.join(format!("{day}.json"))
}

fn load(day: &str) -> HashMap<String, u64> {
    std::fs::read_to_string(path_for(day))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn spawn(shared: Arc<Shared>) {
    std::thread::Builder::new()
        .name("usage".into())
        .spawn(move || {
            let mut day = platform::today_key();
            let mut map = load(&day);
            let mut ticks = 0u64;
            loop {
                std::thread::sleep(Duration::from_secs(TICK));
                ticks += 1;
                let today = platform::today_key();
                if today != day {
                    let _ = std::fs::write(path_for(&day), serde_json::to_string(&map).unwrap_or_default());
                    day = today;
                    map = load(&day);
                }
                if platform::idle_secs() < IDLE_LIMIT {
                    if let Some(app) = platform::foreground_app() {
                        let skip = matches!(app.as_str(), "LockApp" | "ShellExperienceHost" | "SearchHost" | "StartMenuExperienceHost");
                        if !skip {
                            *map.entry(app).or_insert(0) += TICK;
                        }
                    }
                }
                let mut list: Vec<(String, u64)> = map.iter().map(|(k, v)| (k.clone(), *v)).collect();
                list.sort_by(|a, b| b.1.cmp(&a.1));
                let total = list.iter().map(|x| x.1).sum();
                {
                    let mut m = shared.misc.write();
                    m.usage = list;
                    m.usage_total = total;
                }
                if ticks % 30 == 0 {
                    let _ = std::fs::write(path_for(&day), serde_json::to_string(&map).unwrap_or_default());
                }
            }
        })
        .unwrap();
}
