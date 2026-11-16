#![windows_subsystem = "windows"]
//! Aurolo — a Dynamic Island, synced-lyrics widget and ambient edge glow for Windows.

mod anim;
mod app;
mod config;
mod gfx;
mod platform;
mod services;
mod state;
mod ui;

use winit::event_loop::EventLoop;

fn single_instance() -> bool {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        let h = CreateMutexW(None, true, &HSTRING::from("Local\\AuroloDesktopIslandLyrics"));
        if h.is_err() {
            return true;
        }
        std::mem::forget(h);
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

fn main() {
    std::panic::set_hook(Box::new(|info| {
        gfx::log(&format!("panic: {info}"));
    }));
    if !single_instance() {
        return;
    }
    let cfg = config::Config::load();
    let shared = state::Shared::new();

    let lyrics_tx = services::lyrics::spawn(shared.clone());
    services::media::spawn(shared.clone(), lyrics_tx);
    services::audio::spawn(shared.clone());
    services::sysmon::spawn(shared.clone(), cfg.lhm_url.clone());
    services::system::spawn(shared.clone());
    services::web::spawn(shared.clone(), cfg.weather_city.clone(), cfg.calendar_ics_url.clone());

    let el = EventLoop::new().expect("event loop");
    let mut app = app::App::new(shared, cfg);
    if let Err(e) = el.run_app(&mut app) {
        gfx::log(&format!("event loop error: {e}"));
    }
}
