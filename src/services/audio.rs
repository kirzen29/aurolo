//! System audio loopback (what you hear) -> spectrum bands, level and beat pulses.
//! Also watches master volume / mute to drive the volume HUD.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustfft::{num_complex::Complex, FftPlanner};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

use crate::state::*;

const N: usize = 2048;

pub fn spawn(shared: Arc<Shared>) {
    std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            let mut st = Analyzer::new();
            loop {
                if let Err(e) = unsafe { run(&shared, &mut st) } {
                    crate::gfx::log(&format!("audio: {e:?}"));
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        })
        .unwrap();
}

struct Analyzer {
    ring: Vec<f32>,
    pos: usize,
    fft: Arc<dyn rustfft::Fft<f32>>,
    window: Vec<f32>,
    smooth: [f32; BANDS],
    peak: f32,
    bass_avg: f32,
    bass_prev: f32,
    last_beat: Instant,
    last_vol: Option<(f32, bool)>,
}

impl Analyzer {
    fn new() -> Self {
        let mut planner = FftPlanner::new();
        let window = (0..N)
            .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (N - 1) as f32).cos())
            .collect();
        Analyzer {
            ring: vec![0.0; N],
            pos: 0,
            fft: planner.plan_fft_forward(N),
            window,
            smooth: [0.0; BANDS],
            peak: 0.05,
            bass_avg: 0.0,
            bass_prev: 0.0,
            last_beat: Instant::now(),
            last_vol: None,
        }
    }

    fn push(&mut self, s: f32) {
        self.ring[self.pos] = s;
        self.pos = (self.pos + 1) % N;
    }

    fn analyze(&mut self, rate: f32, dt: f32, shared: &Shared) {
        let mut buf: Vec<Complex<f32>> = (0..N)
            .map(|i| Complex::new(self.ring[(self.pos + i) % N] * self.window[i], 0.0))
            .collect();
        self.fft.process(&mut buf);
        let mags: Vec<f32> = buf[..N / 2].iter().map(|c| c.norm() / N as f32).collect();
        let hz = rate / N as f32;
        let (f0, f1) = (35.0f32, 16000.0f32);
        let mut bands = [0.0f32; BANDS];
        let mut frame_peak = 0.0f32;
        for b in 0..BANDS {
            let lo = f0 * (f1 / f0).powf(b as f32 / BANDS as f32);
            let hi = f0 * (f1 / f0).powf((b + 1) as f32 / BANDS as f32);
            let i0 = ((lo / hz) as usize).max(1);
            let i1 = ((hi / hz) as usize).max(i0 + 1).min(mags.len());
            let mut m = 0.0f32;
            for i in i0..i1 {
                m = m.max(mags[i]);
            }
            // tilt so highs aren't invisible
            let v = m * (1.0 + b as f32 * 0.12);
            frame_peak = frame_peak.max(v);
            bands[b] = v;
        }
        // auto gain
        self.peak = (self.peak * (1.0 - dt * 0.35)).max(frame_peak).max(0.004);
        let mut level = 0.0;
        for b in 0..BANDS {
            let v = (bands[b] / self.peak).clamp(0.0, 1.0).powf(0.7);
            let s = &mut self.smooth[b];
            let rate = if v > *s { 28.0 } else { 7.0 };
            *s += (v - *s) * (1.0 - (-rate * dt).exp());
            level += *s;
        }
        level /= BANDS as f32;
        // beat detection on bass bands (~35-150 Hz)
        let bass: f32 = self.smooth[..7].iter().sum::<f32>() / 7.0;
        self.bass_avg += (bass - self.bass_avg) * (1.0 - (-dt * 1.2).exp());
        let rising = bass - self.bass_prev;
        self.bass_prev = bass;
        let mut a = shared.audio.lock();
        a.bands = self.smooth;
        a.level = level;
        a.bass = bass;
        if bass > self.bass_avg * 1.18 + 0.04
            && rising > 0.015
            && self.last_beat.elapsed() > Duration::from_millis(230)
        {
            self.last_beat = Instant::now();
            a.last_beat = Some(self.last_beat);
            a.beat = (bass * 1.4).clamp(0.35, 1.0);
        }
    }

    fn decay(&mut self, dt: f32, shared: &Shared) {
        for s in self.smooth.iter_mut() {
            *s *= (-dt * 6.0).exp();
        }
        let mut a = shared.audio.lock();
        a.bands = self.smooth;
        a.level *= (-dt * 6.0).exp();
        a.bass *= (-dt * 6.0).exp();
    }
}

unsafe fn run(shared: &Shared, st: &mut Analyzer) -> windows::core::Result<()> {
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole)?;
        let dev_id = device.GetId()?.to_string()?;
        let client: IAudioClient = device.Activate(CLSCTX_ALL, None)?;
        let volume: Option<IAudioEndpointVolume> = device.Activate(CLSCTX_ALL, None).ok();
        let fmt = client.GetMixFormat()?;
        let channels = (*fmt).nChannels as usize;
        let rate = (*fmt).nSamplesPerSec as f32;
        let bits = (*fmt).wBitsPerSample;
        let tag = (*fmt).wFormatTag as u32;
        // WAVE_FORMAT_IEEE_FLOAT = 3, WAVE_FORMAT_EXTENSIBLE = 0xFFFE (float in practice for mix format)
        let is_float = tag == 3 || (tag == 0xFFFE && bits == 32);
        client.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK, 2_000_000, 0, fmt, None)?;
        CoTaskMemFree(Some(fmt as *const _));
        let capture: IAudioCaptureClient = client.GetService()?;
        client.Start()?;
        let mut last = Instant::now();
        let mut last_analyze = Instant::now();
        let mut last_check = Instant::now();
        let mut last_dev_check = Instant::now();
        let mut got_audio_at = Instant::now() - Duration::from_secs(10);
        loop {
            std::thread::sleep(Duration::from_millis(8));
            let mut packet = capture.GetNextPacketSize()?;
            while packet > 0 {
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None)?;
                let silent = flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0;
                for f in 0..frames as usize {
                    let mut s = 0.0f32;
                    if !silent {
                        for c in 0..channels.min(2) {
                            let idx = f * channels + c;
                            s += if is_float {
                                *(data as *const f32).add(idx)
                            } else if bits == 16 {
                                *(data as *const i16).add(idx) as f32 / 32768.0
                            } else {
                                0.0
                            };
                        }
                    }
                    st.push(s * 0.5);
                }
                if !silent && frames > 0 {
                    got_audio_at = Instant::now();
                }
                capture.ReleaseBuffer(frames)?;
                packet = capture.GetNextPacketSize()?;
            }
            let dt = last.elapsed().as_secs_f32();
            last = Instant::now();
            if last_analyze.elapsed() >= Duration::from_millis(15) {
                last_analyze = Instant::now();
                if got_audio_at.elapsed() < Duration::from_millis(250) {
                    st.analyze(rate, dt.max(0.015), shared);
                } else {
                    st.decay(dt.max(0.015), shared);
                }
            }
            if last_check.elapsed() >= Duration::from_millis(120) {
                last_check = Instant::now();
                if let Some(v) = &volume {
                    let lvl = v.GetMasterVolumeLevelScalar().unwrap_or(0.0);
                    let muted = v.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                    {
                        let mut m = shared.misc.write();
                        m.volume = lvl;
                        m.muted = muted;
                    }
                    if let Some((pl, pm)) = st.last_vol {
                        if (pl - lvl).abs() > 0.004 || pm != muted {
                            shared.hud(Hud::Volume { level: lvl, muted });
                        }
                    }
                    st.last_vol = Some((lvl, muted));
                }
            }
            if last_dev_check.elapsed() >= Duration::from_secs(2) {
                last_dev_check = Instant::now();
                // default device switched (e.g. headphones plugged in)? restart
                if let Ok(d) = enumerator.GetDefaultAudioEndpoint(eRender, eConsole) {
                    if d.GetId().ok().and_then(|p| p.to_string().ok()).as_deref() != Some(dev_id.as_str()) {
                        let _ = client.Stop();
                        return Ok(());
                    }
                }
            }
        }
    }
}
