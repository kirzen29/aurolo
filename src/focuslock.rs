//! Focus lock: while it's on, games and game launchers are closed as soon as they start.
//!
//! A background thread checks running processes once a second while a lock is active
//! (and does nothing otherwise). A process is treated as a game when its executable is a
//! known game / launcher, or it runs from a game install folder (Riot Games, Steam's
//! steamapps\common, Epic install locations, Roblox). The lock end time is saved by the
//! caller, so restarting Aurolo or the PC doesn't end it early.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

use windows::Win32::Foundation::*;
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::Threading::*;

/// Executable names (lowercase) of games and launchers.
const BLOCK_EXES: &[&str] = &[
    "steam.exe",
    "steamwebhelper.exe",
    "epicgameslauncher.exe",
    "riotclientservices.exe",
    "riotclientux.exe",
    "riotclientuxrender.exe",
    "riot client.exe",
    "valorant.exe",
    "valorant-win64-shipping.exe",
    "leagueclient.exe",
    "leagueclientux.exe",
    "league of legends.exe",
    "robloxplayerbeta.exe",
    "robloxplayerlauncher.exe",
    "minecraftlauncher.exe",
    "minecraft.windows.exe",
    "battle.net.exe",
    "eadesktop.exe",
    "origin.exe",
    "upc.exe",
    "ubisoftconnect.exe",
    "galaxyclient.exe",
    "rockstarlauncher.exe",
    "fortniteclient-win64-shipping.exe",
    "rocketleague.exe",
    "fallguys_client.exe",
    "fallguys_client_game.exe",
    "cs2.exe",
    "dota2.exe",
    "gta5.exe",
    "playgtav.exe",
];

/// Folder fragments (lowercase) that hold games.
fn block_dirs() -> Vec<String> {
    let mut v: Vec<String> = vec!["\\riot games\\".into(), "\\steamapps\\common\\".into(), "\\roblox\\versions\\".into(), "\\epic games\\".into()];
    // every Epic game's install folder
    if let Ok(rd) = std::fs::read_dir(r"C:\ProgramData\Epic\EpicGamesLauncher\Data\Manifests") {
        for e in rd.flatten() {
            let Ok(s) = std::fs::read_to_string(e.path()) else { continue };
            let Ok(j) = serde_json::from_str::<serde_json::Value>(&s) else { continue };
            if let Some(dir) = j["InstallLocation"].as_str() {
                let d = dir.trim_end_matches(['\\', '/']).to_lowercase();
                if d.len() > 4 {
                    v.push(format!("{d}\\"));
                }
            }
        }
    }
    v
}

fn exe_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let r = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, windows::core::PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(h);
        r.ok()?;
        Some(String::from_utf16_lossy(&buf[..len as usize]).to_lowercase())
    }
}

/// (pid, exe name, full path) of every process.
fn processes() -> Vec<(u32, String, String)> {
    let mut out = vec![];
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut pe = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut pe).is_ok() {
            loop {
                let n = pe.szExeFile.iter().position(|&c| c == 0).unwrap_or(pe.szExeFile.len());
                let name = String::from_utf16_lossy(&pe.szExeFile[..n]).to_lowercase();
                out.push((pe.th32ProcessID, name, String::new()));
                if Process32NextW(snap, &mut pe).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
    }
    out
}

fn kill(pid: u32) -> bool {
    unsafe {
        let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) else { return false };
        let ok = TerminateProcess(h, 1).is_ok();
        let _ = CloseHandle(h);
        ok
    }
}

/// Friendly name for a blocked process, e.g. "valorant-win64-shipping.exe" -> "VALORANT".
fn pretty(name: &str) -> String {
    let n = name.trim_end_matches(".exe");
    let known = [
        ("valorant", "VALORANT"),
        ("riotclient", "Riot Client"),
        ("riot client", "Riot Client"),
        ("league", "League of Legends"),
        ("steam", "Steam"),
        ("epicgames", "Epic Games"),
        ("roblox", "Roblox"),
        ("minecraft", "Minecraft"),
        ("battle.net", "Battle.net"),
        ("rocketleague", "Rocket League"),
        ("fallguys", "Fall Guys"),
        ("fortnite", "Fortnite"),
    ];
    for (k, v) in known {
        if n.contains(k) {
            return v.into();
        }
    }
    let mut c = n.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

pub struct FocusLock {
    /// Unix time the lock ends (0 = off).
    pub until: Arc<AtomicI64>,
    /// Names of things that were blocked, for a pop-up.
    pub blocked: Receiver<String>,
}

pub fn spawn(until_unix: i64) -> FocusLock {
    let until = Arc::new(AtomicI64::new(until_unix));
    let (tx, rx) = channel::<String>();
    let u = until.clone();
    let _ = std::thread::Builder::new().name("focus-lock".into()).spawn(move || {
        let me = unsafe { GetCurrentProcessId() };
        let mut dirs: Option<Vec<String>> = None;
        let mut announced: std::collections::HashMap<String, i64> = Default::default();
        loop {
            std::thread::sleep(Duration::from_millis(900));
            let end = u.load(Ordering::Relaxed);
            let now = crate::platform::unix_now();
            // demo mode only shows the lock; it never closes anything
            if end <= now || crate::demo::is_demo() {
                dirs = None;
                continue;
            }
            let dirs = dirs.get_or_insert_with(block_dirs);
            for (pid, name, _) in processes() {
                if pid == me || pid <= 4 {
                    continue;
                }
                let mut hit = BLOCK_EXES.contains(&name.as_str());
                if !hit {
                    // only look up the full path when the name doesn't already decide it
                    if let Some(p) = exe_path(pid) {
                        hit = dirs.iter().any(|d| p.contains(d.as_str())) && !p.contains("\\aurolo");
                    }
                }
                if hit && kill(pid) {
                    let label = pretty(&name);
                    // one pop-up per app every 10 s, not one per helper process
                    let last = announced.get(&label).copied().unwrap_or(0);
                    if now - last > 10 {
                        announced.insert(label.clone(), now);
                        let _ = tx.send(label);
                    }
                }
            }
        }
    });
    FocusLock { until, blocked: rx }
}

#[cfg(test)]
mod tests {
    /// Starts a harmless copy of ping.exe named like a launcher, then checks the lock's
    /// process scan finds it by name and can close it. Only that dummy is touched.
    #[test]
    fn blocks_a_dummy_launcher() {
        let dir = std::env::temp_dir().join("aurolo-focus-test");
        let _ = std::fs::create_dir_all(&dir);
        let dummy = dir.join("riotclientux.exe");
        std::fs::copy(r"C:\Windows\System32\PING.EXE", &dummy).unwrap();
        let mut child = std::process::Command::new(&dummy).args(["-n", "30", "127.0.0.1"]).spawn().unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let seen = super::processes().into_iter().find(|p| p.0 == pid).expect("dummy listed");
        assert!(super::BLOCK_EXES.contains(&seen.1.as_str()), "name matched: {}", seen.1);
        assert!(super::kill(pid), "terminated");
        let status = child.wait().unwrap();
        println!("dummy {} exited: {:?}", seen.1, status);
        let _ = std::fs::remove_file(&dummy);
    }
}
