//! Now-playing info from Windows' Global System Media Transport Controls (SMTC).
//! Works with Spotify, browsers (YouTube, SoundCloud…), Apple Music, VLC, etc.

use std::sync::Arc;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

use crate::state::*;

pub fn spawn(shared: Arc<Shared>, lyrics_tx: std::sync::mpsc::Sender<LyricRequest>) {
    let (tx, rx) = channel();
    *shared.media_tx.lock() = Some(tx);
    std::thread::Builder::new()
        .name("media".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            loop {
                if let Err(e) = run(&shared, &rx, &lyrics_tx) {
                    crate::gfx::log(&format!("media service error: {e:?}"));
                }
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .unwrap();
}

#[derive(Clone, Debug)]
pub struct LyricRequest {
    pub track_id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f64,
}

fn hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn pick_session(mgr: &Manager) -> Option<Session> {
    if let Ok(list) = mgr.GetSessions() {
        let mut first = None;
        for s in list {
            let playing = s
                .GetPlaybackInfo()
                .and_then(|p| p.PlaybackStatus())
                .map(|st| st == Status::Playing)
                .unwrap_or(false);
            if playing {
                return Some(s);
            }
            if first.is_none() {
                first = Some(s);
            }
        }
        if let Ok(cur) = mgr.GetCurrentSession() {
            return Some(cur);
        }
        return first;
    }
    None
}

fn now_filetime() -> i64 {
    let ft = unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() };
    ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
}

fn run(shared: &Arc<Shared>, rx: &Receiver<MediaCmd>, lyrics_tx: &std::sync::mpsc::Sender<LyricRequest>) -> windows::core::Result<()> {
    let mgr = Manager::RequestAsync()?.join()?;
    let mut last_key = String::new();
    let mut last_art_key = String::new();
    let mut art_retry = 0;
    loop {
        let session = pick_session(&mgr);
        // handle commands
        while let Ok(cmd) = rx.try_recv() {
            if let Some(s) = &session {
                let _ = match cmd {
                    MediaCmd::PlayPause => s.TryTogglePlayPauseAsync().map(|_| ()),
                    MediaCmd::Next => s.TrySkipNextAsync().map(|_| ()),
                    MediaCmd::Prev => s.TrySkipPreviousAsync().map(|_| ()),
                    MediaCmd::Seek(secs) => {
                        s.TryChangePlaybackPositionAsync((secs * 10_000_000.0) as i64).map(|_| ())
                    }
                };
                if let MediaCmd::PlayPause = cmd {
                    // optimistic UI update
                    let mut m = shared.media.write();
                    let p = m.pos_now();
                    m.playing = !m.playing;
                    m.position = p;
                    m.pos_at = Some(Instant::now());
                }
            }
        }

        let Some(s) = session else {
            let mut m = shared.media.write();
            if m.active {
                *m = MediaState::default();
            }
            drop(m);
            std::thread::sleep(Duration::from_millis(400));
            continue;
        };

        let app = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
        let props = s.TryGetMediaPropertiesAsync().and_then(|op| op.join());
        let (title, artist, album, thumb) = match &props {
            Ok(p) => (
                p.Title().map(|h| h.to_string()).unwrap_or_default(),
                p.Artist().map(|h| h.to_string()).unwrap_or_default(),
                p.AlbumTitle().map(|h| h.to_string()).unwrap_or_default(),
                p.Thumbnail().ok(),
            ),
            Err(_) => Default::default(),
        };
        let playing = s
            .GetPlaybackInfo()
            .and_then(|p| p.PlaybackStatus())
            .map(|st| st == Status::Playing)
            .unwrap_or(false);
        let tl = s.GetTimelineProperties();
        let (mut pos, dur) = match &tl {
            Ok(t) => {
                let p = t.Position().map(|d| d.Duration as f64 / 1e7).unwrap_or(0.0);
                let st = t.StartTime().map(|d| d.Duration as f64 / 1e7).unwrap_or(0.0);
                let e = t.EndTime().map(|d| d.Duration as f64 / 1e7).unwrap_or(0.0);
                let updated = t.LastUpdatedTime().map(|d| d.UniversalTime).unwrap_or(0);
                let mut p = p - st;
                if playing && updated > 0 {
                    let since = (now_filetime() - updated) as f64 / 1e7;
                    if (0.0..36000.0).contains(&since) {
                        p += since;
                    }
                }
                (p, (e - st).max(0.0))
            }
            Err(_) => (0.0, 0.0),
        };
        if dur > 0.0 {
            pos = pos.clamp(0.0, dur);
        }

        let key = format!("{title}\u{1}{artist}\u{1}{album}");
        let track_changed = key != last_key && !title.is_empty();
        let track_id = hash_str(&key);
        {
            let mut m = shared.media.write();
            let ours = m.pos_now();
            m.active = !title.is_empty();
            m.title = title.clone();
            m.artist = artist.clone();
            m.album = album.clone();
            m.app = app.clone();
            m.duration = dur;
            m.track_id = track_id;
            // keep our smooth clock unless it drifted noticeably
            let drift = (ours - pos).abs();
            if track_changed || m.playing != playing || drift > 0.9 || m.pos_at.is_none() {
                m.position = pos;
                m.pos_at = Some(Instant::now());
            }
            m.playing = playing;
            if track_changed {
                m.art = None;
            }
        }
        if track_changed {
            last_key = key.clone();
            art_retry = 0;
            let _ = lyrics_tx.send(LyricRequest { track_id, title, artist, album, duration: dur });
        }
        // Album art: Spotify sometimes publishes the thumbnail a moment after the title.
        let art_key = format!("{key}#{}", art_retry);
        if last_art_key != art_key && shared.media.read().art.is_none() && art_retry < 6 {
            last_art_key = art_key;
            art_retry += 1;
            if let Some(t) = thumb {
                if let Ok(bytes) = read_stream(&t) {
                    if let Some(art) = process_art(&bytes, track_id) {
                        shared.media.write().art = Some(Arc::new(art));
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn read_stream(r: &windows::Storage::Streams::IRandomAccessStreamReference) -> windows::core::Result<Vec<u8>> {
    let stream = r.OpenReadAsync()?.join()?;
    let size = stream.Size()? as u32;
    if size == 0 {
        return Ok(vec![]);
    }
    let reader = DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.join()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf)?;
    Ok(buf)
}

pub fn process_art(bytes: &[u8], id: u64) -> Option<Art> {
    let img = image::load_from_memory(bytes).ok()?;
    let mut img = img.to_rgba8();
    // Spotify thumbnails for some sources have letterbox bars; crop to centre square.
    let (w, h) = img.dimensions();
    if w != h {
        let s = w.min(h);
        img = image::imageops::crop_imm(&img, (w - s) / 2, (h - s) / 2, s, s).to_image();
    }
    let disp = image::imageops::resize(&img, 300, 300, image::imageops::FilterType::Triangle);
    let small = image::imageops::resize(&img, 40, 40, image::imageops::FilterType::Triangle);
    let blur = image::imageops::blur(&small, 3.5);
    let palette = palette_of(&small);
    Some(Art {
        id,
        w: 300,
        h: 300,
        rgba: disp.into_raw(),
        blur_w: 40,
        blur_h: 40,
        blur: blur.into_raw(),
        palette,
    })
}

fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let mx = r.max(g).max(b);
    let mn = r.min(g).min(b);
    let d = mx - mn;
    let h = if d == 0.0 {
        0.0
    } else if mx == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if mx == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h };
    let s = if mx == 0.0 { 0.0 } else { d / mx };
    (h, s, mx)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 4] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m, 1.0]
}

/// Picks two vivid, distinct accent colours plus dark/light tones from album art.
fn palette_of(img: &image::RgbaImage) -> Palette {
    // 12 hue buckets weighted by saturation*value
    let mut buckets = [(0.0f32, 0.0f32, 0.0f32, 0.0f32); 12]; // weight, sum s, sum v, sum h-offset
    let mut total_v: f32 = 0.0;
    let mut n: f32 = 0.0;
    for p in img.pixels() {
        let (h, s, v) = rgb_to_hsv(p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0);
        total_v += v;
        n += 1.0;
        let w = s * s * v;
        let i = ((h / 30.0) as usize).min(11);
        buckets[i].0 += w;
        buckets[i].1 += s * w;
        buckets[i].2 += v * w;
        buckets[i].3 += (h - i as f32 * 30.0) * w;
    }
    let avg_v = total_v / n.max(1.0);
    let mut order: Vec<usize> = (0..12).collect();
    order.sort_by(|a, b| buckets[*b].0.partial_cmp(&buckets[*a].0).unwrap());
    let mk = |i: usize| {
        let b = buckets[i];
        if b.0 < 1e-3 {
            return None;
        }
        let h = i as f32 * 30.0 + b.3 / b.0;
        let s = (b.1 / b.0).clamp(0.45, 0.85);
        let v = (b.2 / b.0).clamp(0.75, 1.0);
        Some((h, s, v))
    };
    let first = mk(order[0]);
    // second: most weighted bucket at least 2 buckets away in hue
    let second = order[1..]
        .iter()
        .filter(|&&i| {
            let d = (i as i32 - order[0] as i32).rem_euclid(12);
            d >= 2 && d <= 10
        })
        .find_map(|&i| mk(i));
    let def = Palette::default();
    let (primary, h1) = match first {
        Some((h, s, v)) => (hsv_to_rgb(h, s, v), h),
        None => (def.primary, 260.0), // greyscale art → soft violet
    };
    let secondary = match second {
        Some((h, s, v)) => hsv_to_rgb(h, s * 0.9, v),
        None => hsv_to_rgb((h1 + 40.0) % 360.0, 0.55, 1.0),
    };
    let dark = hsv_to_rgb(h1, 0.55, 0.10 + avg_v * 0.06);
    let light = hsv_to_rgb(h1, 0.12, 1.0);
    Palette { primary, secondary, dark, light }
}
