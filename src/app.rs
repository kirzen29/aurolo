//! Event loop, windows, tray menu, global hotkeys and frame pacing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{TrayIcon, TrayIconBuilder};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key as WKey, NamedKey};
use winit::monitor::MonitorHandle;
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Fullscreen as WFullscreen, Window, WindowId, WindowLevel};

use crate::config::{Config, LyricStyle, WidgetBg};
use crate::gfx::{Gpu, Painter, Scene};
use crate::platform;
use crate::shell as platform_shell;
use crate::state::Shared;
use crate::ui::dock::{Dock, DockAction};
use crate::ui::edge::EdgeGlow;
use crate::ui::fullscreen::{FsAction, Fullscreen};
use crate::ui::island::{Island, IslandAction};
use crate::ui::widget::{Widget, WidgetAction};
use crate::ui::{Frame, Input, Key};

struct Win {
    painter: Painter,
    input: Input,
    hwnd: windows::Win32::Foundation::HWND,
    last: Instant,
    next: Instant,
    visible: bool,
    through: bool,
    /// Desired "overlay" style (tool window, optionally no-activate). Re-applied whenever
    /// winit rewrites the window styles (it does on every show/hide).
    overlay: Option<bool>,
    prev_down: bool,
    cursor_resize: bool,
}

impl Win {
    fn new(gpu: &Gpu, w: Arc<Window>) -> Self {
        let hwnd = platform::hwnd_of(&w);
        Win {
            painter: Painter::new(gpu, w),
            input: Input::default(),
            hwnd,
            last: Instant::now(),
            next: Instant::now(),
            visible: true,
            through: false,
            overlay: None,
            prev_down: false,
            cursor_resize: false,
        }
    }
    fn id(&self) -> WindowId {
        self.painter.window.id()
    }
    fn set_visible(&mut self, v: bool) {
        if self.visible != v {
            self.visible = v;
            self.painter.window.set_visible(v);
            // winit rebuilds the extended window style on show/hide, dropping our
            // click-through / no-activate / tool-window bits: put them back.
            self.reapply_styles();
        }
    }
    /// Make the window ignore (or receive) the mouse. Checks the real window style every
    /// call, so it also repairs styles that something else overwrote.
    fn set_through(&mut self, t: bool) {
        self.through = t;
        platform::set_click_through(self.hwnd, t);
    }
    fn set_overlay(&mut self, no_activate: bool) {
        self.overlay = Some(no_activate);
        platform::set_overlay_style(self.hwnd, no_activate);
    }
    fn reapply_styles(&self) {
        if let Some(na) = self.overlay {
            platform::set_overlay_style(self.hwnd, na);
        }
        platform::set_click_through(self.hwnd, self.through);
    }
    /// Poll global mouse state into window-relative logical input.
    fn poll_mouse(&mut self) {
        let (cx, cy) = platform::cursor_pos();
        let pos = self.painter.window.inner_position().unwrap_or_default();
        let s = self.painter.scale;
        let (w, h) = self.painter.logical_size();
        let lx = (cx - pos.x) as f32 / s;
        let ly = (cy - pos.y) as f32 / s;
        let m = if lx >= 0.0 && ly >= 0.0 && lx < w && ly < h { Some((lx, ly)) } else { None };
        if m != self.input.mouse {
            self.input.moved = true;
        }
        self.input.mouse = m;
        let down = platform::mouse_down();
        if down && !self.prev_down {
            self.input.pressed = true;
            self.input.press_pos = m;
        }
        if !down && self.prev_down {
            self.input.released = true;
        }
        self.input.down = down;
        self.prev_down = down;
    }
}

pub struct App {
    gpu: Option<Gpu>,
    shared: Arc<Shared>,
    cfg: Config,
    t0: Instant,
    island: Option<(Win, Island)>,
    widget: Option<(Win, Widget)>,
    fs: Option<(Win, Fullscreen)>,
    edge: Option<(Win, EdgeGlow)>,
    dock: Option<(Win, Dock)>,
    /// Physical rect of the strip the dock occupies (and reserves, as an appbar).
    dock_rc: Option<windows::Win32::Foundation::RECT>,
    /// We hid the Windows taskbar (so we know to bring it back).
    taskbar_hidden: bool,
    last_taskbar_sync: Instant,
    backdrop: Option<Win>,
    tray: Option<TrayIcon>,
    menu: Option<MenuIds>,
    hotkeys: Option<GlobalHotKeyManager>,
    hk_lyrics: u32,
    hk_island: u32,
    fullscreen_app: bool,
    last_fs_check: Instant,
    last_topmost: Instant,
    monitor: Option<MonitorHandle>,
}

struct MenuIds {
    island: CheckMenuItem,
    widget: CheckMenuItem,
    lyrics_only: CheckMenuItem,
    sizes: Vec<(CheckMenuItem, usize)>,
    bgs: Vec<(CheckMenuItem, WidgetBg)>,
    edge: CheckMenuItem,
    dock: CheckMenuItem,
    win_taskbar: CheckMenuItem,
    fullscreen: MenuItem,
    wstyles: Vec<(CheckMenuItem, LyricStyle)>,
    fstyles: Vec<(CheckMenuItem, LyricStyle)>,
    earlier: MenuItem,
    later: MenuItem,
    reset: MenuItem,
    autostart: CheckMenuItem,
    config: MenuItem,
    quit: MenuItem,
}

impl App {
    pub fn new(shared: Arc<Shared>, cfg: Config) -> Self {
        *shared.lyrics_offset.write() = cfg.lyrics_offset_ms as f64 / 1000.0;
        App {
            gpu: None,
            shared,
            cfg,
            t0: Instant::now(),
            island: None,
            widget: None,
            fs: None,
            edge: None,
            dock: None,
            dock_rc: None,
            taskbar_hidden: false,
            last_taskbar_sync: Instant::now(),
            backdrop: None,
            tray: None,
            menu: None,
            hotkeys: None,
            hk_lyrics: 0,
            hk_island: 0,
            fullscreen_app: false,
            last_fs_check: Instant::now(),
            last_topmost: Instant::now(),
            monitor: None,
        }
    }

    fn overlay_attrs(title: &str) -> winit::window::WindowAttributes {
        Window::default_attributes()
            .with_title(title)
            .with_transparent(true)
            .with_decorations(false)
            .with_resizable(false)
            .with_no_redirection_bitmap(true)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_skip_taskbar(true)
            .with_active(false)
    }

    fn mon_rect(&self) -> (PhysicalPosition<i32>, PhysicalSize<u32>, f64) {
        match &self.monitor {
            Some(m) => (m.position(), m.size(), m.scale_factor()),
            None => (PhysicalPosition::new(0, 0), PhysicalSize::new(1920, 1080), 1.0),
        }
    }

    fn create_island(&mut self, el: &ActiveEventLoop) {
        let (mp, ms, sc) = self.mon_rect();
        let w = (crate::ui::island::WIN_W as f64 * sc) as i32;
        let x = mp.x + (ms.width as i32 - w) / 2;
        let attrs = Self::overlay_attrs("Aurolo Island")
            .with_inner_size(LogicalSize::new(crate::ui::island::WIN_W, crate::ui::island::WIN_H))
            .with_position(PhysicalPosition::new(x, mp.y));
        let win = Arc::new(el.create_window(attrs).expect("island window"));
        let mut w = Win::new(self.gpu.as_ref().unwrap(), win);
        w.set_overlay(true);
        w.set_through(true);
        let mut isl = Island::new();
        isl.lyrics_widget_on = self.cfg.lyrics_widget;
        self.island = Some((w, isl));
    }

    fn create_widget(&mut self, el: &ActiveEventLoop) {
        let (mp, ms, sc) = self.mon_rect();
        let (lw, lh) = self.cfg.widget_size();
        let ww = (lw as f64 * sc) as i32;
        let wh = (lh as f64 * sc) as i32;
        let pos = self.cfg.widget_pos.map(|(x, y)| PhysicalPosition::new(x, y)).unwrap_or_else(|| {
            PhysicalPosition::new(mp.x + ms.width as i32 - ww - (28.0 * sc) as i32, mp.y + ms.height as i32 - wh - (76.0 * sc) as i32)
        });
        let attrs = Self::overlay_attrs("Aurolo Lyrics")
            .with_inner_size(LogicalSize::new(lw, lh))
            .with_position(pos);
        let win = Arc::new(el.create_window(attrs).expect("widget window"));
        let mut w = Win::new(self.gpu.as_ref().unwrap(), win);
        w.set_overlay(true);
        self.widget = Some((w, Widget::new(self.cfg.widget_style, self.cfg.widget_lyrics_only, self.cfg.widget_background)));
    }

    fn create_edge(&mut self, el: &ActiveEventLoop) {
        let (mp, ms, _) = self.mon_rect();
        let attrs = Self::overlay_attrs("Aurolo Edge").with_inner_size(ms).with_position(mp);
        let win = Arc::new(el.create_window(attrs).expect("edge window"));
        let mut w = Win::new(self.gpu.as_ref().unwrap(), win);
        w.set_overlay(true);
        w.set_through(true);
        self.edge = Some((w, EdgeGlow::new()));
    }

    fn create_dock(&mut self, el: &ActiveEventLoop) {
        let (mp, ms, sc) = self.mon_rect();
        let h = ((crate::ui::dock::DOCK_H + crate::ui::dock::TIP_H) as f64 * sc).round() as i32;
        let attrs = Self::overlay_attrs("Aurolo Dock")
            .with_inner_size(PhysicalSize::new(ms.width, h as u32))
            .with_position(PhysicalPosition::new(mp.x, mp.y + ms.height as i32 - h));
        let win = Arc::new(el.create_window(attrs).expect("dock window"));
        let mut w = Win::new(self.gpu.as_ref().unwrap(), win);
        w.set_overlay(true);
        w.set_through(true);
        if self.cfg.dock_reserve {
            platform_shell::appbar_register(w.hwnd);
        }
        self.dock = Some((w, Dock::new()));
        self.dock_rc = None;
        self.place_dock();
    }

    fn destroy_dock(&mut self) {
        if let Some((w, _)) = self.dock.take() {
            platform_shell::appbar_remove(w.hwnd);
        }
        self.dock_rc = None;
    }

    /// Fit the dock to the bottom of its monitor (above the Windows taskbar when that is
    /// visible) and, if enabled, reserve the strip so maximized windows stop above it.
    fn place_dock(&mut self) {
        let (mp, ms, _) = self.mon_rect();
        let Some((w, _)) = &self.dock else { return };
        let sc = w.painter.scale as f64;
        let bar = (crate::ui::dock::DOCK_H as f64 * sc).round() as i32;
        let tip = (crate::ui::dock::TIP_H as f64 * sc).round() as i32;
        let (mon, work) = platform_shell::monitor_rects(mp.x + ms.width as i32 / 2, mp.y + ms.height as i32 / 2);
        let rc = if self.cfg.dock_reserve {
            let q = platform_shell::appbar_place(w.hwnd, mon, bar, false);
            if self.dock_rc != Some(q) { platform_shell::appbar_place(w.hwnd, mon, bar, true) } else { q }
        } else {
            windows::Win32::Foundation::RECT { left: work.left, right: work.right, top: work.bottom - bar, bottom: work.bottom }
        };
        if self.dock_rc == Some(rc) {
            return;
        }
        self.dock_rc = Some(rc);
        let win = &w.painter.window;
        let _ = win.request_inner_size(PhysicalSize::new((rc.right - rc.left) as u32, (bar + tip) as u32));
        win.set_outer_position(PhysicalPosition::new(rc.left, rc.bottom - bar - tip));
    }

    fn set_dock(&mut self, el: &ActiveEventLoop, on: bool) {
        self.cfg.dock = on;
        self.cfg.save();
        if on && self.dock.is_none() {
            self.create_dock(el);
        } else if !on {
            self.destroy_dock();
        }
        if let Some(m) = &self.menu {
            m.dock.set_checked(on);
        }
        self.sync_windows_taskbar();
    }

    fn set_hide_taskbar(&mut self, on: bool) {
        self.cfg.hide_windows_taskbar = on;
        self.cfg.save();
        if let Some(m) = &self.menu {
            m.win_taskbar.set_checked(on);
        }
        self.sync_windows_taskbar();
    }

    /// Hide the Windows taskbar while the dock runs (if asked to); show it otherwise.
    fn sync_windows_taskbar(&mut self) {
        let hide = self.cfg.hide_windows_taskbar && self.dock.is_some();
        if hide || self.taskbar_hidden {
            crate::desktop::set_windows_taskbar_visible(!hide);
        }
        self.taskbar_hidden = hide;
    }

    fn toggle_launcher(&mut self) {
        let t = self.t0.elapsed().as_secs_f32();
        if let Some((w, isl)) = &mut self.island {
            if isl.launcher {
                isl.close(t);
                w.set_overlay(true);
            } else {
                isl.open_launcher(t);
                w.set_overlay(false);
                w.set_through(false);
                w.painter.window.focus_window();
                platform::force_foreground(w.hwnd);
            }
        }
    }

    fn open_fullscreen(&mut self, el: &ActiveEventLoop) {
        if let Some((w, _)) = &self.fs {
            w.painter.window.focus_window();
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Aurolo — Lyrics")
            .with_transparent(true)
            .with_decorations(false)
            .with_no_redirection_bitmap(true)
            .with_fullscreen(Some(WFullscreen::Borderless(self.monitor.clone())));
        let win = Arc::new(el.create_window(attrs).expect("fullscreen window"));
        win.focus_window();
        let w = Win::new(self.gpu.as_ref().unwrap(), win);
        platform::force_foreground(w.hwnd);
        self.fs = Some((w, Fullscreen::new(self.cfg.fullscreen_style)));
    }

    fn close_fullscreen(&mut self) {
        self.fs = None;
    }

    fn toggle_widget(&mut self, el: &ActiveEventLoop, on: bool) {
        self.cfg.lyrics_widget = on;
        self.cfg.save();
        if on && self.widget.is_none() {
            self.create_widget(el);
        } else if !on {
            self.save_widget_pos();
            self.widget = None;
        }
        if let Some((_, i)) = &mut self.island {
            i.lyrics_widget_on = on;
        }
        if let Some(m) = &self.menu {
            m.widget.set_checked(on);
        }
    }

    fn save_widget_pos(&mut self) {
        if let Some((w, _)) = &self.widget {
            if let Ok(p) = w.painter.window.outer_position() {
                self.cfg.widget_pos = Some((p.x, p.y));
                self.cfg.save();
            }
        }
    }

    /// Settings changed from the island's Settings tab.
    fn apply_cfg(&mut self, el: &ActiveEventLoop, c: crate::ui::island::CfgChange) {
        use crate::ui::island::CfgChange as C;
        match c {
            C::Widget => {
                let on = !self.cfg.lyrics_widget;
                self.toggle_widget(el, on);
            }
            C::WidgetLayout => self.toggle_layout(),
            C::WidgetSize => {
                let next = self.cfg.widget_preset().map(|i| (i + 1) % crate::config::WIDGET_PRESETS.len()).unwrap_or(0);
                let s = self.cfg.apply_widget_preset(next);
                self.resize_widget(s);
            }
            C::WidgetBg => {
                let b = self.cfg.widget_background.next();
                self.set_bg(b);
            }
            C::WidgetStyle => {
                self.cfg.widget_style = self.cfg.widget_style.next();
                self.cfg.save();
                if let Some((_, wd)) = &mut self.widget {
                    wd.view.style = self.cfg.widget_style;
                }
                self.sync_style_checks();
            }
            C::FsStyle => {
                self.cfg.fullscreen_style = self.cfg.fullscreen_style.next();
                self.cfg.save();
                if let Some((_, f)) = &mut self.fs {
                    f.view.style = self.cfg.fullscreen_style;
                }
                self.sync_style_checks();
            }
            C::Edge => {
                self.cfg.edge_glow = !self.cfg.edge_glow;
                self.cfg.save();
                if self.cfg.edge_glow {
                    if self.edge.is_none() {
                        self.create_edge(el);
                    }
                } else {
                    self.edge = None;
                }
                if let Some(m) = &self.menu {
                    m.edge.set_checked(self.cfg.edge_glow);
                }
            }
            C::WinTaskbar => {
                let on = !self.cfg.hide_windows_taskbar;
                self.set_hide_taskbar(on);
            }
            C::Dock => {
                let on = !self.cfg.dock;
                self.set_dock(el, on);
            }
            C::Autostart => {
                self.cfg.autostart = !self.cfg.autostart;
                platform::set_autostart(self.cfg.autostart);
                self.cfg.save();
                if let Some(m) = &self.menu {
                    m.autostart.set_checked(self.cfg.autostart);
                }
            }
            C::OpenFile => {
                platform::open_with_args("notepad.exe", &format!("\"{}\"", crate::config::config_path().display()));
            }
        }
    }

    /// Resize the lyrics widget window, remember the size, and update the tray checks.
    fn resize_widget(&mut self, s: (f32, f32)) {
        self.cfg.set_widget_size(s);
        self.cfg.save();
        if let Some((w, _)) = &self.widget {
            let _ = w.painter.window.request_inner_size(LogicalSize::new(s.0, s.1));
        }
        self.sync_size_checks();
    }

    fn sync_size_checks(&self) {
        if let Some(m) = &self.menu {
            let cur = self.cfg.widget_preset();
            for (it, i) in &m.sizes {
                it.set_checked(cur == Some(*i));
            }
        }
    }

    fn toggle_layout(&mut self) {
        self.cfg.widget_lyrics_only = !self.cfg.widget_lyrics_only;
        self.cfg.save();
        let (w, h) = self.cfg.widget_size();
        if let Some((win, wd)) = &mut self.widget {
            wd.lyrics_only = self.cfg.widget_lyrics_only;
            let _ = win.painter.window.request_inner_size(LogicalSize::new(w, h));
        }
        if let Some(m) = &self.menu {
            m.lyrics_only.set_checked(self.cfg.widget_lyrics_only);
        }
        self.sync_size_checks();
    }

    fn set_bg(&mut self, b: WidgetBg) {
        self.cfg.widget_background = b;
        self.cfg.save();
        if let Some((_, wd)) = &mut self.widget {
            wd.bg = b;
        }
        if let Some(m) = &self.menu {
            for (it, x) in &m.bgs {
                it.set_checked(*x == b);
            }
        }
    }

    fn set_offset(&mut self, delta: f64, reset: bool) {
        let mut o = self.shared.lyrics_offset.write();
        *o = if reset { 0.0 } else { *o + delta };
        self.cfg.lyrics_offset_ms = (*o * 1000.0).round() as i64;
        drop(o);
        self.cfg.save();
    }

    fn sync_style_checks(&self) {
        if let Some(m) = &self.menu {
            for (item, s) in &m.wstyles {
                item.set_checked(*s == self.cfg.widget_style);
            }
            for (item, s) in &m.fstyles {
                item.set_checked(*s == self.cfg.fullscreen_style);
            }
        }
    }

    fn build_tray(&mut self) {
        let island = CheckMenuItem::new("Dynamic Island", true, self.cfg.island, None);
        let widget = CheckMenuItem::new("Lyrics widget", true, self.cfg.lyrics_widget, None);
        let edge = CheckMenuItem::new("Edge glow", true, self.cfg.edge_glow, None);
        let dock = CheckMenuItem::new("Taskbar dock", true, self.cfg.dock, None);
        let win_taskbar = CheckMenuItem::new("Hide Windows taskbar", true, self.cfg.hide_windows_taskbar, None);
        let lyrics_only = CheckMenuItem::new("Widget: lyrics only", true, self.cfg.widget_lyrics_only, None);
        let bgsub = Submenu::new("Widget background", true);
        let mut bgs = vec![];
        for b in WidgetBg::ALL {
            let it = CheckMenuItem::new(b.name(), true, b == self.cfg.widget_background, None);
            let _ = bgsub.append(&it);
            bgs.push((it, b));
        }
        let sizesub = Submenu::new("Widget size", true);
        let mut sizes = vec![];
        let cur_preset = self.cfg.widget_preset();
        for (i, p) in crate::config::WIDGET_PRESETS.iter().enumerate() {
            let it = CheckMenuItem::new(p.0, true, cur_preset == Some(i), None);
            let _ = sizesub.append(&it);
            sizes.push((it, i));
        }
        let fullscreen = MenuItem::new(format!("Fullscreen lyrics\t{}", self.cfg.hotkey_lyrics), true, None);
        let wsub = Submenu::new("Widget style", true);
        let fsub = Submenu::new("Fullscreen style", true);
        let mut wstyles = vec![];
        let mut fstyles = vec![];
        for s in LyricStyle::ALL {
            let a = CheckMenuItem::new(s.name(), true, s == self.cfg.widget_style, None);
            let b = CheckMenuItem::new(s.name(), true, s == self.cfg.fullscreen_style, None);
            let _ = wsub.append(&a);
            let _ = fsub.append(&b);
            wstyles.push((a, s));
            fstyles.push((b, s));
        }
        let sync = Submenu::new("Lyrics sync", true);
        let earlier = MenuItem::new("Show lyrics earlier (−0.25s)", true, None);
        let later = MenuItem::new("Show lyrics later (+0.25s)", true, None);
        let reset = MenuItem::new("Reset", true, None);
        let _ = sync.append_items(&[&earlier, &later, &reset]);
        let autostart = CheckMenuItem::new("Start with Windows", true, self.cfg.autostart, None);
        let config = MenuItem::new("Open settings file", true, None);
        let quit = MenuItem::new("Quit Aurolo", true, None);
        let menu = Menu::new();
        let _ = menu.append_items(&[
            &island,
            &widget,
            &edge,
            &dock,
            &win_taskbar,
            &PredefinedMenuItem::separator(),
            &lyrics_only,
            &sizesub,
            &bgsub,
            &fullscreen,
            &wsub,
            &fsub,
            &sync,
            &PredefinedMenuItem::separator(),
            &autostart,
            &config,
            &quit,
        ]);
        let icon = tray_icon_image();
        self.tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Aurolo — island & lyrics")
            .with_icon(icon)
            .build()
            .ok();
        self.menu = Some(MenuIds { island, widget, lyrics_only, sizes, bgs, edge, dock, win_taskbar, fullscreen, wstyles, fstyles, earlier, later, reset, autostart, config, quit });
    }

    /// Registers any hotkey not yet owned. Called at startup and retried every few
    /// seconds, so a key held by another app (e.g. Dynamic Ledge) is picked up once it quits.
    fn register_hotkeys(&mut self) {
        if self.hotkeys.is_none() {
            match GlobalHotKeyManager::new() {
                Ok(m) => self.hotkeys = Some(m),
                Err(_) => return,
            }
        }
        let mgr = self.hotkeys.as_ref().unwrap();
        if self.hk_lyrics == 0 {
            if let Some(hk) = parse_hotkey(&self.cfg.hotkey_lyrics) {
                match mgr.register(hk) {
                    Ok(_) => self.hk_lyrics = hk.id(),
                    Err(e) => crate::gfx::log(&format!("hotkey {} busy: {e}", self.cfg.hotkey_lyrics)),
                }
            }
        }
        if self.hk_island == 0 {
            if let Some(hk) = parse_hotkey(&self.cfg.hotkey_island) {
                match mgr.register(hk) {
                    Ok(_) => self.hk_island = hk.id(),
                    Err(e) => crate::gfx::log(&format!("hotkey {} busy: {e}", self.cfg.hotkey_island)),
                }
            }
        }
    }

    fn handle_events(&mut self, el: &ActiveEventLoop) {
        while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
            if ev.state != HotKeyState::Pressed {
                continue;
            }
            if ev.id == self.hk_lyrics {
                if self.fs.is_some() {
                    self.close_fullscreen();
                } else {
                    self.open_fullscreen(el);
                }
            } else if ev.id == self.hk_island {
                self.toggle_launcher();
            }
        }
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            let Some(m) = &self.menu else { break };
            let id = ev.id;
            if id == *m.quit.id() {
                self.save_widget_pos();
                self.destroy_dock();
                crate::desktop::set_windows_taskbar_visible(true);
                el.exit();
                return;
            } else if id == *m.island.id() {
                self.cfg.island = m.island.is_checked();
                self.cfg.save();
                if self.cfg.island {
                    if self.island.is_none() {
                        self.create_island(el);
                    }
                } else {
                    self.island = None;
                }
            } else if id == *m.widget.id() {
                let on = m.widget.is_checked();
                self.toggle_widget(el, on);
            } else if id == *m.lyrics_only.id() {
                let v = m.lyrics_only.is_checked();
                if v != self.cfg.widget_lyrics_only {
                    self.toggle_layout();
                }
            } else if let Some(i) = m.sizes.iter().find(|(it, _)| id == *it.id()).map(|(_, i)| *i) {
                let s = self.cfg.apply_widget_preset(i);
                self.resize_widget(s);
            } else if let Some(b) = m.bgs.iter().find(|(it, _)| id == *it.id()).map(|(_, b)| *b) {
                self.set_bg(b);
            } else if id == *m.edge.id() {
                self.cfg.edge_glow = m.edge.is_checked();
                self.cfg.save();
                if self.cfg.edge_glow {
                    if self.edge.is_none() {
                        self.create_edge(el);
                    }
                } else {
                    self.edge = None;
                }
            } else if id == *m.win_taskbar.id() {
                let on = m.win_taskbar.is_checked();
                self.set_hide_taskbar(on);
            } else if id == *m.dock.id() {
                let on = m.dock.is_checked();
                self.set_dock(el, on);
            } else if id == *m.fullscreen.id() {
                self.open_fullscreen(el);
            } else if id == *m.earlier.id() {
                self.set_offset(-0.25, false);
            } else if id == *m.later.id() {
                self.set_offset(0.25, false);
            } else if id == *m.reset.id() {
                self.set_offset(0.0, true);
            } else if id == *m.autostart.id() {
                self.cfg.autostart = m.autostart.is_checked();
                platform::set_autostart(self.cfg.autostart);
                self.cfg.save();
            } else if id == *m.config.id() {
                platform::open_with_args("notepad.exe", &format!("\"{}\"", crate::config::config_path().display()));
            } else {
                let mut changed = false;
                for (item, s) in &m.wstyles {
                    if id == *item.id() {
                        self.cfg.widget_style = *s;
                        changed = true;
                    }
                }
                for (item, s) in &m.fstyles {
                    if id == *item.id() {
                        self.cfg.fullscreen_style = *s;
                        changed = true;
                    }
                }
                if changed {
                    self.cfg.save();
                    if let Some((_, wd)) = &mut self.widget {
                        wd.view.style = self.cfg.widget_style;
                    }
                    if let Some((_, f)) = &mut self.fs {
                        f.view.style = self.cfg.fullscreen_style;
                    }
                    self.sync_style_checks();
                }
            }
        }
    }

    fn render_window(&mut self, el: &ActiveEventLoop, id: WindowId) {
        let t = self.t0.elapsed().as_secs_f32();
        let gpu = self.gpu.as_ref().unwrap();
        let shared = self.shared.clone();

        if let Some((w, isl)) = self.island.as_mut().filter(|(w, _)| w.id() == id) {
            let dt = w.last.elapsed().as_secs_f32();
            w.last = Instant::now();
            w.poll_mouse();
            isl.hidden = self.fullscreen_app;
            isl.widget_style_name = self.cfg.widget_style.name();
            let (lw, lh) = w.painter.logical_size();
            let mut f = Frame { gpu, painter: &mut w.painter, scene: Scene::new(), w: lw, h: lh, t, dt };
            let action = isl.draw(&mut f, &w.input, &shared, &self.cfg);
            let scene = std::mem::take(&mut f.scene);
            w.painter.render(gpu, &scene, t);
            // click-through outside the pill
            let over = match (isl.hit, w.input.mouse) {
                (Some(r), Some(p)) => crate::ui::hit(r, p),
                _ => false,
            };
            w.set_through(!over && !isl.launcher);
            w.input.end_frame();
            match action {
                IslandAction::ToggleWidget => {
                    let on = !self.cfg.lyrics_widget;
                    self.toggle_widget(el, on);
                }
                IslandAction::Fullscreen => self.open_fullscreen(el),
                IslandAction::WantFocus(on) => {
                    if let Some((w, _)) = &mut self.island {
                        w.set_overlay(!on);
                        if on {
                            w.set_through(false);
                            w.painter.window.focus_window();
                            platform::force_foreground(w.hwnd);
                        }
                    }
                }
                IslandAction::Cfg(c) => self.apply_cfg(el, c),
                IslandAction::None => {}
            }
            return;
        }

        if let Some((w, wd)) = self.widget.as_mut().filter(|(w, _)| w.id() == id) {
            let dt = w.last.elapsed().as_secs_f32();
            w.last = Instant::now();
            w.poll_mouse();
            let (lw, lh) = w.painter.logical_size();
            let mut f = Frame { gpu, painter: &mut w.painter, scene: Scene::new(), w: lw, h: lh, t, dt };
            let action = wd.draw(&mut f, &w.input, &shared);
            let scene = std::mem::take(&mut f.scene);
            w.painter.render(gpu, &scene, t);
            w.input.end_frame();
            if wd.cursor_resize != w.cursor_resize {
                w.cursor_resize = wd.cursor_resize;
                w.painter.window.set_cursor(if wd.cursor_resize {
                    winit::window::CursorIcon::SeResize
                } else {
                    winit::window::CursorIcon::Default
                });
            }
            match action {
                WidgetAction::Close => self.toggle_widget(el, false),
                WidgetAction::Fullscreen => self.open_fullscreen(el),
                WidgetAction::Drag => {
                    if let Some((w, _)) = &self.widget {
                        let _ = w.painter.window.drag_window();
                    }
                }
                WidgetAction::ToggleLayout => self.toggle_layout(),
                WidgetAction::BgChanged(b) => self.set_bg(b),
                WidgetAction::Resize(nw, nh) => {
                    if let Some((w, _)) = &self.widget {
                        let _ = w.painter.window.request_inner_size(LogicalSize::new(nw, nh));
                    }
                }
                WidgetAction::ResizeEnd(nw, nh) => {
                    if let Some((w, _)) = &self.widget {
                        let _ = w.painter.window.request_inner_size(LogicalSize::new(nw, nh));
                    }
                    self.cfg.set_widget_size((nw, nh));
                    self.sync_size_checks();
                    self.save_widget_pos();
                }
                WidgetAction::StyleChanged(s) => {
                    self.cfg.widget_style = s;
                    self.cfg.save();
                    self.sync_style_checks();
                }
                WidgetAction::None => {}
            }
            return;
        }

        if let Some((w, fsv)) = self.fs.as_mut().filter(|(w, _)| w.id() == id) {
            let dt = w.last.elapsed().as_secs_f32();
            w.last = Instant::now();
            w.poll_mouse();
            let (lw, lh) = w.painter.logical_size();
            let mut f = Frame { gpu, painter: &mut w.painter, scene: Scene::new(), w: lw, h: lh, t, dt };
            let action = fsv.draw(&mut f, &w.input, &shared);
            let scene = std::mem::take(&mut f.scene);
            w.painter.render(gpu, &scene, t);
            w.input.end_frame();
            match action {
                FsAction::Close => self.close_fullscreen(),
                FsAction::StyleChanged(s) => {
                    self.cfg.fullscreen_style = s;
                    self.cfg.save();
                    self.sync_style_checks();
                }
                FsAction::Offset(d) => self.set_offset(d, false),
                FsAction::None => {}
            }
            return;
        }

        if let Some((w, dk)) = self.dock.as_mut().filter(|(w, _)| w.id() == id) {
            let dt = w.last.elapsed().as_secs_f32();
            w.last = Instant::now();
            w.poll_mouse();
            let (lw, lh) = w.painter.logical_size();
            let mut f = Frame { gpu, painter: &mut w.painter, scene: Scene::new(), w: lw, h: lh, t, dt };
            let action = dk.draw(
                &mut f,
                &w.input,
                &shared,
                &self.cfg.dock_pins,
                &self.cfg.dock_hidden,
                self.cfg.pomodoro_minutes,
                &self.cfg.wallpaper_dir,
            );
            let scene = std::mem::take(&mut f.scene);
            w.painter.render(gpu, &scene, t);
            let over = w.input.mouse.map(|p| dk.hits.iter().any(|r| crate::ui::hit(*r, p))).unwrap_or(false);
            w.set_through(!over);
            w.input.end_frame();
            match action {
                DockAction::Launcher => self.toggle_launcher(),
                DockAction::Pins(op) => {
                    use crate::ui::dock::PinOp;
                    let short = |s: &str| s.rsplit('\\').next().unwrap_or(s).trim_end_matches(".exe").trim_end_matches(".lnk").to_string();
                    match op {
                        PinOp::Pin(exe) => {
                            if !self.cfg.dock_pins.iter().any(|p| p.eq_ignore_ascii_case(&exe)) {
                                self.shared.hud(crate::state::Hud::Message("Pinned to dock".into(), short(&exe)));
                                self.cfg.dock_pins.push(exe);
                            }
                        }
                        PinOp::Unpin(exe) => {
                            self.cfg.dock_pins.retain(|p| !p.eq_ignore_ascii_case(&exe));
                            self.shared.hud(crate::state::Hud::Message("Unpinned from dock".into(), short(&exe)));
                        }
                        PinOp::Hide(key) => {
                            self.shared.hud(crate::state::Hud::Message("Hidden from dock".into(), short(&key)));
                            self.cfg.dock_hidden.push(key);
                        }
                    }
                    self.cfg.save();
                }
                DockAction::None => {}
            }
            return;
        }

        if let Some(w) = self.backdrop.as_mut().filter(|w| w.id() == id) {
            let (lw, lh) = w.painter.logical_size();
            let mut f = Frame { gpu, painter: &mut w.painter, scene: Scene::new(), w: lw, h: lh, t, dt: 0.016 };
            crate::demo::draw_backdrop(&mut f);
            let scene = std::mem::take(&mut f.scene);
            w.painter.render(gpu, &scene, t);
            return;
        }

        if let Some((w, eg)) = self.edge.as_mut().filter(|(w, _)| w.id() == id) {
            let dt = w.last.elapsed().as_secs_f32();
            w.last = Instant::now();
            let (lw, lh) = w.painter.logical_size();
            let mut f = Frame { gpu, painter: &mut w.painter, scene: Scene::new(), w: lw, h: lh, t, dt };
            let alive = eg.draw(&mut f, &shared, self.cfg.edge_thickness, self.cfg.edge_intensity, self.cfg.edge_only_when_playing);
            let scene = std::mem::take(&mut f.scene);
            w.painter.render(gpu, &scene, t);
            // never let the fullscreen glow catch clicks or focus
            w.reapply_styles();
            if !alive {
                w.set_visible(false);
            }
        }
    }

    fn win_mut(&mut self, id: WindowId) -> Option<&mut Win> {
        if let Some((w, _)) = self.island.as_mut().filter(|(w, _)| w.id() == id) {
            return Some(w);
        }
        if let Some((w, _)) = self.widget.as_mut().filter(|(w, _)| w.id() == id) {
            return Some(w);
        }
        if let Some((w, _)) = self.fs.as_mut().filter(|(w, _)| w.id() == id) {
            return Some(w);
        }
        if let Some((w, _)) = self.edge.as_mut().filter(|(w, _)| w.id() == id) {
            return Some(w);
        }
        if let Some((w, _)) = self.dock.as_mut().filter(|(w, _)| w.id() == id) {
            return Some(w);
        }
        if let Some(w) = self.backdrop.as_mut().filter(|w| w.id() == id) {
            return Some(w);
        }
        None
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gpu.is_some() {
            return;
        }
        match Gpu::new(&self.cfg.lyrics_font) {
            Ok(g) => self.gpu = Some(g),
            Err(e) => {
                crate::gfx::log(&format!("GPU init failed: {e}"));
                el.exit();
                return;
            }
        }
        self.monitor = el.primary_monitor().or_else(|| el.available_monitors().next());
        if crate::demo::is_demo() {
            let (mp, ms, _) = self.mon_rect();
            let attrs = Window::default_attributes()
                .with_title("Aurolo demo backdrop")
                .with_decorations(false)
                .with_no_redirection_bitmap(true)
                .with_skip_taskbar(true)
                .with_inner_size(ms)
                .with_position(mp);
            if let Ok(win) = el.create_window(attrs) {
                let win = Arc::new(win);
                win.focus_window();
                let w = Win::new(self.gpu.as_ref().unwrap(), win);
                platform::force_foreground(w.hwnd);
                self.backdrop = Some(w);
            }
        }
        if self.cfg.edge_glow {
            self.create_edge(el);
        }
        if self.cfg.dock {
            self.create_dock(el);
        }
        if self.cfg.lyrics_widget {
            self.create_widget(el);
        }
        if self.cfg.island {
            self.create_island(el);
        }
        self.build_tray();
        self.register_hotkeys();
        if crate::demo::want_fullscreen() {
            self.open_fullscreen(el);
        }
        if self.cfg.autostart {
            platform::set_autostart(true);
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, ev: WindowEvent) {
        match ev {
            WindowEvent::RedrawRequested => self.render_window(el, id),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                let gpu = self.gpu.as_ref().unwrap() as *const Gpu;
                if let Some(w) = self.win_mut(id) {
                    w.painter.resize(unsafe { &*gpu });
                }
            }
            WindowEvent::CloseRequested => {
                if self.fs.as_ref().map(|(w, _)| w.id() == id).unwrap_or(false) {
                    self.close_fullscreen();
                }
            }
            WindowEvent::Focused(false) => {
                let t = self.t0.elapsed().as_secs_f32();
                if let Some((w, isl)) = self.island.as_mut().filter(|(w, _)| w.id() == id) {
                    if isl.launcher {
                        isl.close(t);
                        w.set_overlay(true);
                    }
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                let Some(w) = self.win_mut(id) else { return };
                let k = match &event.logical_key {
                    WKey::Named(NamedKey::Enter) => Some(Key::Enter),
                    WKey::Named(NamedKey::Escape) => Some(Key::Esc),
                    WKey::Named(NamedKey::Backspace) => Some(Key::Back),
                    WKey::Named(NamedKey::ArrowUp) => Some(Key::Up),
                    WKey::Named(NamedKey::ArrowDown) => Some(Key::Down),
                    WKey::Named(NamedKey::ArrowLeft) => Some(Key::Left),
                    WKey::Named(NamedKey::ArrowRight) => Some(Key::Right),
                    WKey::Named(NamedKey::Tab) => Some(Key::Tab),
                    WKey::Named(NamedKey::Space) => Some(Key::Space),
                    WKey::Character(c) if c == "[" => Some(Key::BracketL),
                    WKey::Character(c) if c == "]" => Some(Key::BracketR),
                    _ => None,
                };
                if let Some(k) = k {
                    w.input.keys.push(k);
                }
                if let Some(t) = &event.text {
                    w.input.text.push_str(t);
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                if let Some(w) = self.win_mut(id) {
                    w.input.right_pressed = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if let Some(w) = self.win_mut(id) {
                    w.input.scroll += match delta {
                        MouseScrollDelta::LineDelta(_, y) => y,
                        MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                    };
                }
            }
            WindowEvent::HoveredFile(_) => {
                if let Some(w) = self.win_mut(id) {
                    w.input.hovering_file = true;
                }
            }
            WindowEvent::HoveredFileCancelled => {
                if let Some(w) = self.win_mut(id) {
                    w.input.hovering_file = false;
                }
            }
            WindowEvent::DroppedFile(p) => {
                if let Some(w) = self.win_mut(id) {
                    w.input.hovering_file = false;
                    w.input.dropped.push(p);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.gpu.is_none() {
            return;
        }
        self.handle_events(el);
        let now = Instant::now();

        if now.duration_since(self.last_taskbar_sync) > Duration::from_millis(700) {
            self.last_taskbar_sync = now;
            self.sync_windows_taskbar();
        }
        if now.duration_since(self.last_fs_check) > Duration::from_millis(500) {
            self.last_fs_check = now;
            let ours = self.fs.is_some();
            self.fullscreen_app = !ours && platform::foreground_is_fullscreen();
        }
        if now.duration_since(self.last_topmost) > Duration::from_secs(3) {
            self.last_topmost = now;
            if self.hk_island == 0 || self.hk_lyrics == 0 {
                self.register_hotkeys();
            }
            self.place_dock();
            for w in [
                self.island.as_ref().map(|x| &x.0),
                self.widget.as_ref().map(|x| &x.0),
                self.edge.as_ref().map(|x| &x.0),
                self.dock.as_ref().map(|x| &x.0),
            ]
                .into_iter()
                .flatten()
            {
                if w.visible {
                    platform::keep_topmost(w.hwnd);
                }
            }
        }

        let playing = {
            let m = self.shared.media.read();
            m.playing && m.active
        };
        // visibility rules
        let hide_for_fs = self.fullscreen_app;
        if let Some((w, _)) = &mut self.widget {
            w.set_visible(!hide_for_fs);
        }
        if let Some((w, _)) = &mut self.dock {
            w.set_visible(!hide_for_fs);
        }
        if let Some((w, _)) = &mut self.edge {
            let want = !hide_for_fs && self.fs.is_none() && (playing || !self.cfg.edge_only_when_playing);
            if want {
                w.set_visible(true);
            } else if !w.visible {
                // stays hidden
            } else if hide_for_fs || self.fs.is_some() {
                w.set_visible(false);
            }
        }

        let fast = Duration::from_micros(16_667);
        let mut next_wake = now + Duration::from_millis(250);
        let island_idle = self
            .island
            .as_ref()
            .map(|(_, i)| !playing && i.hit.map(|h| h[2] < 230.0).unwrap_or(true) && !i.launcher)
            .unwrap_or(true);
        let mut schedule = |w: &mut Win, interval: Duration| {
            if !w.visible {
                return;
            }
            if now >= w.next {
                w.painter.window.request_redraw();
                w.next = (w.next + interval).max(now);
            }
            next_wake = next_wake.min(w.next);
        };
        if let Some((w, _)) = &mut self.island {
            // stay smooth while the mouse is near the island
            let near = {
                let (_, cy) = platform::cursor_pos();
                let pos = w.painter.window.inner_position().unwrap_or_default();
                (cy - pos.y) < (140.0 * w.painter.scale) as i32
            };
            let iv = if island_idle && !near { Duration::from_millis(120) } else { fast };
            schedule(w, iv);
        }
        if let Some((w, _)) = &mut self.widget {
            schedule(w, if playing { fast } else { Duration::from_millis(66) });
        }
        if let Some((w, _)) = &mut self.fs {
            schedule(w, fast);
        }
        if let Some((w, dk)) = &mut self.dock {
            let near = {
                let (_, cy) = platform::cursor_pos();
                let pos = w.painter.window.inner_position().unwrap_or_default();
                cy >= pos.y + (crate::ui::dock::TIP_H * w.painter.scale) as i32
            };
            let iv = if dk.busy || near || dk.popup_open() {
                fast
            } else if playing {
                Duration::from_millis(33)
            } else {
                Duration::from_millis(500)
            };
            schedule(w, iv);
        }
        if let Some((w, _)) = &mut self.edge {
            schedule(w, fast);
        }
        if let Some(w) = &mut self.backdrop {
            schedule(w, Duration::from_millis(500));
        }
        el.set_control_flow(ControlFlow::WaitUntil(next_wake));
    }
}

fn parse_hotkey(s: &str) -> Option<HotKey> {
    let mut mods = Modifiers::empty();
    let mut code = None;
    for part in s.split('+').map(|p| p.trim().to_lowercase()) {
        match part.as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            "win" | "super" | "meta" => mods |= Modifiers::SUPER,
            "space" => code = Some(Code::Space),
            "enter" => code = Some(Code::Enter),
            k if k.len() == 1 => {
                let c = k.chars().next().unwrap().to_ascii_uppercase();
                code = format!("Key{c}").parse::<Code>().ok().or_else(|| format!("Digit{c}").parse::<Code>().ok());
            }
            k if k.starts_with('f') => code = k.to_uppercase().parse::<Code>().ok(),
            _ => {}
        }
    }
    Some(HotKey::new(Some(mods), code?))
}

fn tray_icon_image() -> tray_icon::Icon {
    let n = 32u32;
    let mut px = vec![0u8; (n * n * 4) as usize];
    for y in 0..n {
        for x in 0..n {
            let dx = x as f32 - 15.5;
            let dy = y as f32 - 15.5;
            let d = (dx * dx + dy * dy).sqrt();
            let ang = dy.atan2(dx);
            let t = 0.5 + 0.5 * ang.sin();
            let (r, g, b) = (30.0 + 30.0 * t, 100.0 + 90.0 * t, 245.0);
            let ring = (1.0 - ((d - 11.0).abs() - 2.4).clamp(0.0, 1.0)).max(if d < 4.5 { 1.0 - (d - 3.5).clamp(0.0, 1.0) } else { 0.0 });
            let i = ((y * n + x) * 4) as usize;
            px[i] = r as u8;
            px[i + 1] = g as u8;
            px[i + 2] = b as u8;
            px[i + 3] = (ring * 255.0) as u8;
        }
    }
    tray_icon::Icon::from_rgba(px, n, n).expect("icon")
}
