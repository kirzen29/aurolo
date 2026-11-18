#![windows_subsystem = "windows"]
//! Aurolo — a Dynamic Island, synced-lyrics widget and ambient edge glow for Windows.

mod anim;
mod app;
mod config;
mod demo;
mod gfx;
mod netctl;
mod platform;
mod services;
mod desktop;
mod focuslock;
mod shell;
mod state;
mod ui;

use winit::event_loop::EventLoop;

/// Only one Aurolo runs at a time. Demo mode (screenshots / testing) has its own slot,
/// so it can run next to the real one instead of closing it.
fn single_instance(demo: bool) -> bool {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    let name = if demo { "Local\\AuroloDemo" } else { "Local\\AuroloDesktopIslandLyrics" };
    unsafe {
        let h = CreateMutexW(None, true, &HSTRING::from(name));
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
        // never leave the user without a taskbar
        desktop::set_windows_taskbar_visible(true);
    }));
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let demo = demo::init();
    if !single_instance(demo) {
        gfx::log(&format!("start skipped: Aurolo is already running ({exe})"));
        return;
    }
    gfx::log(&format!("start{}: Aurolo {} ({exe})", if demo { " (demo)" } else { "" }, env!("CARGO_PKG_VERSION")));
    let mut cfg = config::Config::load();
    if demo {
        demo::apply_cfg(&mut cfg);
    }
    let shared = state::Shared::new();

    let lyrics_tx = services::lyrics::spawn(shared.clone());
    services::media::spawn(shared.clone(), lyrics_tx);
    services::audio::spawn(shared.clone());
    services::sysmon::spawn(shared.clone(), cfg.lhm_url.clone());
    if demo {
        // sample data instead of the user's notifications, Wi-Fi, app usage, clipboard
        demo::seed(&shared);
    } else {
        services::system::spawn(shared.clone());
        services::usage::spawn(shared.clone());
        services::web::spawn(shared.clone(), cfg.weather_city.clone(), cfg.calendar_ics_url.clone());
    }

    let el = EventLoop::new().expect("event loop");
    let mut app = app::App::new(shared, cfg);
    if let Err(e) = el.run_app(&mut app) {
        gfx::log(&format!("event loop error: {e}"));
    }
    // never leave the user without a taskbar
    desktop::set_windows_taskbar_visible(true);
    gfx::log("exit");
}
