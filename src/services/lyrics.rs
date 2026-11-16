//! Synced lyrics from LRCLIB (https://lrclib.net) with an on-disk cache.
//! Line-synced LRC is upgraded to per-word timing (enhanced LRC `<mm:ss.xx>` tags are
//! used when present, otherwise words are timed by syllable weight across the line).

use std::sync::Arc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

use crate::services::media::LyricRequest;
use crate::state::*;

const UA: &str = "Aurolo/0.1 (desktop lyrics overlay; https://lrclib.net)";

pub fn spawn(shared: Arc<Shared>) -> Sender<LyricRequest> {
    let (tx, rx) = channel::<LyricRequest>();
    std::thread::Builder::new()
        .name("lyrics".into())
        .spawn(move || worker(shared, rx))
        .unwrap();
    tx
}

fn worker(shared: Arc<Shared>, rx: Receiver<LyricRequest>) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .user_agent(UA)
        .http_status_as_error(false)
        .build()
        .into();
    while let Ok(mut req) = rx.recv() {
        // collapse rapid skips: only handle the newest request
        while let Ok(r) = rx.try_recv() {
            req = r;
        }
        {
            let mut l = shared.lyrics.write();
            l.track_id = req.track_id;
            l.status = LyricStatus::Loading;
            l.lines.clear();
        }
        let result = fetch(&agent, &req);
        let mut l = shared.lyrics.write();
        if l.track_id != req.track_id {
            continue;
        }
        match result {
            Fetched::Synced(lrc) => {
                l.lines = parse_lrc(&lrc, req.duration);
                l.status = if l.lines.is_empty() { LyricStatus::NotFound } else { LyricStatus::Synced };
            }
            Fetched::Plain(text) => {
                l.lines = text
                    .lines()
                    .map(|s| Line { t: 0.0, end: 0.0, text: s.trim().to_string(), words: vec![] })
                    .collect();
                l.status = LyricStatus::Plain;
            }
            Fetched::Instrumental => l.status = LyricStatus::Instrumental,
            Fetched::None => l.status = LyricStatus::NotFound,
        }
    }
}

enum Fetched {
    Synced(String),
    Plain(String),
    Instrumental,
    None,
}

fn cache_path(req: &LyricRequest) -> std::path::PathBuf {
    let d = crate::config::data_dir().join("lyrics");
    let _ = std::fs::create_dir_all(&d);
    d.join(format!("{:016x}.json", req.track_id))
}

fn clean_title(t: &str) -> String {
    let mut s = t.to_string();
    for sep in [" - ", " (feat", " (with", " [", " (From", " (Remaster", " (Live"] {
        if let Some(i) = s.find(sep) {
            if i > 0 {
                s.truncate(i);
            }
        }
    }
    s.trim().to_string()
}

fn first_artist(a: &str) -> String {
    a.split([',', '&', ';']).next().unwrap_or(a).replace(" feat.", "").trim().to_string()
}

fn from_json(v: &serde_json::Value) -> Option<Fetched> {
    if v.get("instrumental").and_then(|x| x.as_bool()) == Some(true) {
        return Some(Fetched::Instrumental);
    }
    if let Some(s) = v.get("syncedLyrics").and_then(|x| x.as_str()) {
        if !s.trim().is_empty() {
            return Some(Fetched::Synced(s.to_string()));
        }
    }
    if let Some(s) = v.get("plainLyrics").and_then(|x| x.as_str()) {
        if !s.trim().is_empty() {
            return Some(Fetched::Plain(s.to_string()));
        }
    }
    None
}

fn fetch(agent: &ureq::Agent, req: &LyricRequest) -> Fetched {
    let cp = cache_path(req);
    if let Ok(s) = std::fs::read_to_string(&cp) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
            if let Some(f) = from_json(&v) {
                return f;
            }
            if v.get("none").is_some() {
                return Fetched::None;
            }
        }
    }
    let save = |v: &serde_json::Value| {
        let _ = std::fs::write(&cp, v.to_string());
    };

    // 1. exact match
    let mut url = format!(
        "https://lrclib.net/api/get?track_name={}&artist_name={}",
        enc(&req.title),
        enc(&req.artist)
    );
    if !req.album.is_empty() {
        url += &format!("&album_name={}", enc(&req.album));
    }
    if req.duration > 1.0 {
        url += &format!("&duration={}", req.duration.round() as i64);
    }
    if let Some(v) = get_json(agent, &url) {
        if let Some(f) = from_json(&v) {
            save(&v);
            return f;
        }
    }
    // 2. search with cleaned title, pick best duration match preferring synced
    let queries = [
        format!("track_name={}&artist_name={}", enc(&clean_title(&req.title)), enc(&first_artist(&req.artist))),
        format!("q={}", enc(&format!("{} {}", clean_title(&req.title), first_artist(&req.artist)))),
    ];
    for q in queries {
        let url = format!("https://lrclib.net/api/search?{q}");
        if let Some(serde_json::Value::Array(items)) = get_json(agent, &url) {
            let mut best: Option<(f64, &serde_json::Value)> = None;
            for it in &items {
                let synced = it.get("syncedLyrics").and_then(|x| x.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
                let d = it.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0);
                let dd = if req.duration > 1.0 { (d - req.duration).abs() } else { 0.0 };
                if req.duration > 1.0 && dd > 8.0 {
                    continue;
                }
                let score = dd + if synced { 0.0 } else { 50.0 };
                if best.map(|b| score < b.0).unwrap_or(true) {
                    best = Some((score, it));
                }
            }
            if let Some((_, v)) = best {
                if let Some(f) = from_json(v) {
                    save(v);
                    return f;
                }
            }
        }
    }
    save(&serde_json::json!({"none": true}));
    Fetched::None
}

fn get_json(agent: &ureq::Agent, url: &str) -> Option<serde_json::Value> {
    let mut resp = agent.get(url).call().ok()?;
    if resp.status() != 200 {
        return None;
    }
    resp.body_mut().read_json::<serde_json::Value>().ok()
}

fn enc(s: &str) -> String {
    let mut o = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => o.push(b as char),
            b' ' => o.push_str("%20"),
            _ => o.push_str(&format!("%{b:02X}")),
        }
    }
    o
}

fn parse_ts(s: &str) -> Option<f64> {
    // mm:ss.xx or mm:ss
    let (m, rest) = s.split_once(':')?;
    let m: f64 = m.trim().parse().ok()?;
    let sec: f64 = rest.trim().parse().ok()?;
    Some(m * 60.0 + sec)
}

/// Rough syllable weight so long words take longer to sing.
fn weight(w: &str) -> f64 {
    let mut v = 0;
    let mut prev = false;
    for c in w.chars() {
        let is_v = "aeiouyAEIOUY".contains(c);
        if is_v && !prev {
            v += 1;
        }
        prev = is_v;
    }
    let chars = w.chars().filter(|c| c.is_alphanumeric()).count() as f64;
    0.6 + v.max(1) as f64 * 0.8 + chars * 0.08
}

pub fn parse_lrc(lrc: &str, duration: f64) -> Vec<Line> {
    let mut raw: Vec<(f64, String)> = vec![];
    for line in lrc.lines() {
        let mut rest = line.trim();
        let mut stamps = vec![];
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            let tag = &rest[1..end];
            rest = &rest[end + 1..];
            if let Some(t) = parse_ts(tag) {
                stamps.push(t);
            }
        }
        for t in stamps {
            raw.push((t, rest.trim().to_string()));
        }
    }
    raw.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let total = if duration > 1.0 { duration } else { raw.last().map(|r| r.0 + 6.0).unwrap_or(0.0) };
    let mut lines = vec![];
    for i in 0..raw.len() {
        let (t, ref text) = raw[i];
        let next = raw.get(i + 1).map(|r| r.0).unwrap_or(total.max(t + 4.0));
        // enhanced lrc word tags
        let mut words: Vec<Word> = vec![];
        let plain: String;
        if text.contains('<') {
            let mut s = text.as_str();
            let mut cur_t = t;
            let mut acc = String::new();
            let mut parts: Vec<(f64, String)> = vec![];
            while let Some(open) = s.find('<') {
                let before = &s[..open];
                acc.push_str(before);
                if let Some(close) = s[open..].find('>') {
                    let tag = &s[open + 1..open + close];
                    s = &s[open + close + 1..];
                    if let Some(ts) = parse_ts(tag) {
                        if !acc.trim().is_empty() {
                            parts.push((cur_t, acc.trim().to_string()));
                        }
                        acc.clear();
                        cur_t = ts;
                    }
                } else {
                    break;
                }
            }
            acc.push_str(s);
            if !acc.trim().is_empty() {
                parts.push((cur_t, acc.trim().to_string()));
            }
            for (j, (wt, wtext)) in parts.iter().enumerate() {
                let end = parts.get(j + 1).map(|p| p.0).unwrap_or(next);
                for (k, piece) in wtext.split_whitespace().enumerate() {
                    let _ = k;
                    words.push(Word { t: *wt, end, text: piece.to_string() });
                }
            }
            plain = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        } else {
            plain = text.clone();
            let toks: Vec<&str> = text.split_whitespace().collect();
            if !toks.is_empty() {
                let weights: Vec<f64> = toks.iter().map(|w| weight(w)).collect();
                let sum: f64 = weights.iter().sum();
                // estimated sung duration, bounded by the gap to the next line
                let est = (sum * 0.21).clamp(0.9, 9.0);
                let gap = (next - t).max(0.3);
                let span = if gap < est * 1.6 { gap * 0.94 } else { est };
                let mut acc = t;
                for (w, wt) in toks.iter().zip(weights.iter()) {
                    let d = span * wt / sum;
                    words.push(Word { t: acc, end: acc + d, text: w.to_string() });
                    acc += d;
                }
            }
        }
        let end = words.last().map(|w| w.end).unwrap_or(next).min(next);
        lines.push(Line { t, end: end.max(t + 0.2), text: plain, words });
    }
    // collapse consecutive empty lines
    lines.dedup_by(|b, a| a.text.is_empty() && b.text.is_empty());
    lines
}
