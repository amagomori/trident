mod clipboard;
mod input;
mod latency;
mod pty;
mod show;

use input::MouseButton as ReportButton;
use pty::Pty;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use term_core::{MouseMode, Point, SelectionKind, Terminal};
use term_render::{Fonts, PADDING, Renderer};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{Window, WindowId};

const DEFAULT_FONT_SIZE: f32 = 14.0;
/// Output parsed since the last frame before more is held back, so a flood reaches
/// `advance` in batches long enough to skip the lines it evicts. Interactive output (echo,
/// replies to queries) never gets near it and is parsed the moment it arrives.
const DEFER_AFTER: usize = 64 * 1024;
/// How long held-back output waits, unless the queue fills first: about one 60 Hz frame.
const MAX_DEFER: Duration = Duration::from_millis(16);
const RESIZE_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Default)]
struct ResizeThrottle {
    last_applied: Option<Instant>,
    deadline: Option<Instant>,
}

impl ResizeThrottle {
    fn schedule(&mut self, now: Instant) {
        self.deadline = Some(
            self.last_applied
                .map_or(now, |last| (last + RESIZE_INTERVAL).max(now)),
        );
    }

    fn applied(&mut self, now: Instant) {
        self.last_applied = Some(now);
        self.deadline = None;
    }

    fn ready(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
}

#[derive(Debug)]
enum UserEvent {
    Output,
    Exit,
}

struct Options {
    font: Option<PathBuf>,
    font_size: f32,
    option_as_alt: bool,
    command: Vec<String>,
}

fn usage() -> ! {
    println!(
        "{} {} - a fast GPU-rendered terminal emulator\n\n\
         USAGE: terminal [OPTIONS] [-e COMMAND [ARGS...]]\n       \
         terminal show [--width N] [--height N] [--fit] FILE...\n\n\
         OPTIONS:\n  \
           --font <PATH>       TrueType/OpenType font file (default: bundled JetBrains Mono)\n  \
           --font-size <SIZE>  Font size in points (default: {DEFAULT_FONT_SIZE})\n  \
           --option-as-alt     On macOS, make Option act as Alt/Meta\n  \
           -e, --command ...   Run this command instead of your shell\n  \
           -h, --help          Show this help\n  \
           -V, --version       Show the version\n\n\
         `terminal show` draws images and prints text files in the terminal it runs in\n\
         (see terminal show --help).",
        env!("CARGO_BIN_NAME"),
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(0);
}

fn parse_args() -> Result<Options, String> {
    let mut options = Options {
        font: None,
        font_size: DEFAULT_FONT_SIZE,
        option_as_alt: !cfg!(target_os = "macos"),
        command: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--font" => options.font = Some(args.next().ok_or("--font needs a path")?.into()),
            "--font-size" => {
                options.font_size = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|s: &f32| *s >= 4.0 && *s <= 200.0)
                    .ok_or("--font-size needs a number between 4 and 200")?;
            }
            "--option-as-alt" => options.option_as_alt = true,
            "-e" | "--command" => {
                options.command = args.by_ref().collect();
                if options.command.is_empty() {
                    return Err("-e needs a command".into());
                }
            }
            "-h" | "--help" => usage(),
            "-V" | "--version" => {
                println!("{} {}", env!("CARGO_BIN_NAME"), env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other} (see --help)")),
        }
    }
    Ok(options)
}

struct Session {
    window: Arc<Window>,
    renderer: Renderer,
    fonts: Fonts,
    term: Terminal,
    pty: Pty,
    /// Output parsed since the last frame was drawn.
    since_frame: usize,
    /// When output was first held back.
    deferred: Option<Instant>,
    latency: Option<latency::Latency>,
    resize: ResizeThrottle,
}

fn advance_output(term: &mut Terminal, output: pty::Output) {
    let mut start = 0;
    for offset in output.shell_returns {
        term.advance(&output.bytes[start..offset]);
        term.reset_input_modes();
        start = offset;
    }
    term.advance(&output.bytes[start..]);
}

/// Whether output that just arrived can be held back: only during a flood, and never once
/// the reader is stalled on a full queue.
fn defer_output(deferring: bool, since_frame: usize, queue_full: bool) -> bool {
    (deferring || since_frame >= DEFER_AFTER) && !queue_full
}

struct App {
    options: Options,
    proxy: EventLoopProxy<UserEvent>,
    session: Option<Session>,
    font_size: f32,
    modifiers: ModifiersState,
    focused: bool,
    mouse_pos: PhysicalPosition<f64>,
    left_down: bool,
    /// Button held while a program tracks the mouse, for drag reports.
    reported_button: Option<ReportButton>,
    last_click: Option<(Instant, Point, u8)>,
    scroll_accum: f64,
    clipboard: clipboard::Clipboard,
    error: Option<String>,
}

impl App {
    fn new(options: Options, proxy: EventLoopProxy<UserEvent>) -> App {
        App {
            font_size: options.font_size,
            options,
            proxy,
            session: None,
            modifiers: ModifiersState::empty(),
            focused: true,
            mouse_pos: PhysicalPosition::new(0.0, 0.0),
            left_down: false,
            reported_button: None,
            last_click: None,
            scroll_accum: 0.0,
            clipboard: clipboard::Clipboard::default(),
            error: None,
        }
    }

    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Session, String> {
        let probe = Fonts::new(self.options.font.as_deref(), self.font_size)?;
        let m = probe.metrics;
        let logical_size = LogicalSize::new(
            100.0 * m.width + 2.0 * PADDING,
            30.0 * m.height + 2.0 * PADDING,
        );
        let guess = event_loop
            .primary_monitor()
            .map_or(1.0, |m| m.scale_factor());
        let guessed_fonts = if guess == 1.0 {
            probe
        } else {
            Fonts::new(self.options.font.as_deref(), self.font_size * guess as f32)?
        };
        let (spawn_cols, spawn_rows) =
            grid_size_for(logical_size.to_physical(guess), guess, &guessed_fonts);
        let spawn_cell = (guessed_fonts.metrics.width, guessed_fonts.metrics.height);
        let proxy = self.proxy.clone();
        let pty = Pty::spawn(
            &self.options.command,
            spawn_cols,
            spawn_rows,
            spawn_cell,
            move |exited| {
                let _ = proxy.send_event(if exited {
                    UserEvent::Exit
                } else {
                    UserEvent::Output
                });
            },
        )?;
        let attrs = Window::default_attributes()
            .with_title("terminal")
            .with_inner_size(logical_size);
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| format!("cannot create window: {e}"))?,
        );
        window.set_ime_allowed(true);
        let scale = window.scale_factor();
        let fonts = if scale == guess {
            guessed_fonts
        } else {
            Fonts::new(self.options.font.as_deref(), self.font_size * scale as f32)?
        };
        let size = window.inner_size();
        let renderer = Renderer::new(window.clone(), size.width, size.height)?;
        eprintln!("terminal: rendering with {}", renderer.adapter_info);

        let (cols, rows) = grid_size(&window, &fonts);
        let mut term = Terminal::new(cols, rows);
        term.set_cell_pixel_size(fonts.metrics.width, fonts.metrics.height);
        let cell = (fonts.metrics.width, fonts.metrics.height);
        if (cols, rows, cell) != (spawn_cols, spawn_rows, spawn_cell) {
            pty.resize(cols, rows, cell);
        }
        window.request_redraw();
        let mut latency = latency::Latency::from_env(
            window
                .current_monitor()
                .and_then(|m| m.refresh_rate_millihertz())
                .unwrap_or(0),
        );
        if let Some(log) = &mut latency {
            log.feedback(window.clone());
            #[cfg(target_os = "macos")]
            log.metal_feedback(renderer.gpu_device());
        }
        Ok(Session {
            latency,
            window,
            renderer,
            fonts,
            term,
            pty,
            since_frame: 0,
            deferred: None,
            resize: ResizeThrottle::default(),
        })
    }

    fn resize_grid(&mut self) {
        let Some(s) = &mut self.session else { return };
        s.resize.applied(Instant::now());
        let (cols, rows) = grid_size(&s.window, &s.fonts);
        s.term
            .set_cell_pixel_size(s.fonts.metrics.width, s.fonts.metrics.height);
        if (cols, rows) != (s.term.cols(), s.term.rows()) {
            s.term.resize(cols, rows);
            release_freed_memory(&mut s.term);
            s.pty
                .resize(cols, rows, (s.fonts.metrics.width, s.fonts.metrics.height));
        }
        s.window.request_redraw();
    }

    fn set_font_size(&mut self, size: f32) {
        self.font_size = size.clamp(4.0, 200.0);
        let Some(s) = &mut self.session else { return };
        s.fonts
            .set_size(self.font_size * s.window.scale_factor() as f32);
        s.renderer.clear_glyph_cache();
        self.resize_grid();
    }

    fn output_ready(&mut self) {
        let Some(s) = &mut self.session else { return };
        if defer_output(s.deferred.is_some(), s.since_frame, s.pty.output_full()) {
            s.deferred.get_or_insert_with(Instant::now);
        } else {
            self.process_output();
        }
    }

    fn process_output(&mut self) {
        let Some(s) = &mut self.session else { return };
        s.pty.wake_pending.store(false, Ordering::Release);
        s.deferred = None;
        let batch = s.pty.take_output();
        let processed = batch.bytes.len();
        s.since_frame += processed;
        advance_output(&mut s.term, batch);
        release_freed_memory(&mut s.term);
        if s.term.modes.mouse == MouseMode::Off {
            self.reported_button = None;
        }
        let responses = s.term.take_responses();
        s.pty.write(&responses);
        if let Some(title) = s.term.take_title() {
            s.window
                .set_title(if title.is_empty() { "terminal" } else { &title });
        }
        if std::mem::take(&mut s.term.bell) && !self.focused {
            s.window
                .request_user_attention(Some(winit::window::UserAttentionType::Informational));
        }
        if processed > 0 {
            if let Some(log) = &mut s.latency {
                log.output(processed);
            }
            s.window.request_redraw();
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        let Some(s) = &mut self.session else { return };
        if let Some(log) = &mut s.latency {
            log.input(Instant::now());
        }
        s.term.scroll_to_bottom();
        s.term.selection = None;
        s.pty.write(bytes);
        s.window.request_redraw();
    }

    fn paste(&mut self, primary: bool) {
        let Some(text) = self.clipboard.get(primary) else {
            return;
        };
        let Some(s) = &self.session else { return };
        // Normalize newlines to carriage returns, as typed Enter would send.
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        let bytes = if s.term.modes.bracketed_paste {
            // Strip every ESC so the text cannot end the bracketed paste early.
            let text = text.replace('\x1b', "");
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text
        };
        self.write_input(bytes.as_bytes());
    }

    fn copy(&mut self, primary: bool) {
        let Some(s) = &self.session else { return };
        if let Some(text) = s.term.selection_text().filter(|t| !t.is_empty()) {
            self.clipboard.set(text, primary);
        }
    }

    /// Handles terminal shortcuts; true if the key was consumed.
    fn shortcut(&mut self, event: &KeyEvent) -> bool {
        let mods = self.modifiers;
        let command = if cfg!(target_os = "macos") {
            mods.super_key()
        } else {
            mods.control_key() && mods.shift_key()
        };
        let base = event.key_without_modifiers();
        if command && let Key::Character(c) = &base {
            match c.to_lowercase().as_str() {
                "c" => self.copy(false),
                "v" => self.paste(false),
                "=" | "+" => self.set_font_size(self.font_size + 1.0),
                "-" | "_" => self.set_font_size(self.font_size - 1.0),
                "0" | ")" => self.set_font_size(self.options.font_size),
                "k" if cfg!(target_os = "macos") => {
                    self.write_input(b"\x0c");
                    if let Some(s) = &mut self.session {
                        s.term.advance(b"\x1b[3J");
                        release_freed_memory(&mut s.term);
                    }
                }
                _ => return cfg!(target_os = "macos"),
            }
            return true;
        }
        let Some(s) = &mut self.session else {
            return false;
        };
        if mods.shift_key() && !mods.control_key() {
            let page = s.term.rows() as isize - 1;
            let handled = match &event.logical_key {
                Key::Named(NamedKey::PageUp) => Some(page),
                Key::Named(NamedKey::PageDown) => Some(-page),
                Key::Named(NamedKey::Home) if !s.term.is_alt_screen() => Some(isize::MAX / 2),
                Key::Named(NamedKey::End) if !s.term.is_alt_screen() => Some(-(isize::MAX / 2)),
                Key::Named(NamedKey::Insert) => {
                    self.paste(false);
                    return true;
                }
                _ => None,
            };
            if let Some(delta) = handled {
                s.term.scroll_display(delta);
                s.window.request_redraw();
                return true;
            }
        }
        false
    }

    fn cell_at(&self, pos: PhysicalPosition<f64>) -> Option<(usize, usize)> {
        let s = self.session.as_ref()?;
        let pad = PADDING as f64 * s.window.scale_factor();
        let m = s.fonts.metrics;
        let col = ((pos.x - pad) / m.width as f64).floor().max(0.0) as usize;
        let row = ((pos.y - pad) / m.height as f64).floor().max(0.0) as usize;
        Some((row.min(s.term.rows() - 1), col.min(s.term.cols() - 1)))
    }

    fn mouse_reporting(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.term.modes.mouse != MouseMode::Off)
            && !self.modifiers.shift_key()
    }

    fn report_mouse(&mut self, button: ReportButton, pressed: bool, motion: bool) {
        let Some((row, col)) = self.cell_at(self.mouse_pos) else {
            return;
        };
        let Some(s) = &mut self.session else { return };
        let sgr = s.term.modes.sgr_mouse;
        if let Some(bytes) =
            input::encode_mouse(button, pressed, motion, col, row, self.modifiers, sgr)
        {
            s.pty.write(&bytes);
        }
    }

    fn mouse_input(&mut self, state: ElementState, button: MouseButton) {
        let pressed = state == ElementState::Pressed;
        if self.mouse_reporting() {
            let button = match button {
                MouseButton::Left => ReportButton::Left,
                MouseButton::Middle => ReportButton::Middle,
                MouseButton::Right => ReportButton::Right,
                _ => return,
            };
            self.reported_button = pressed.then_some(button);
            self.report_mouse(button, pressed, false);
            return;
        }
        match button {
            MouseButton::Left => {
                self.left_down = pressed;
                if !pressed {
                    self.copy(true);
                    return;
                }
                let Some((row, col)) = self.cell_at(self.mouse_pos) else {
                    return;
                };
                let Some(s) = &mut self.session else { return };
                let point = s.term.viewport_to_point(row, col);
                let clicks = match self.last_click {
                    Some((at, p, n)) if p == point && at.elapsed() < Duration::from_millis(400) => {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                self.last_click = Some((Instant::now(), point, clicks));
                let kind = match clicks {
                    2 => SelectionKind::Word,
                    3 => SelectionKind::Line,
                    _ => SelectionKind::Simple,
                };
                s.term.start_selection(point, kind);
                s.window.request_redraw();
            }
            MouseButton::Middle if pressed => self.paste(true),
            _ => {}
        }
    }

    fn cursor_moved(&mut self, pos: PhysicalPosition<f64>) {
        let old = self.cell_at(self.mouse_pos);
        self.mouse_pos = pos;
        let cell = self.cell_at(pos);
        if self.mouse_reporting() {
            let mode = self.session.as_ref().map(|s| s.term.modes.mouse);
            if cell != old {
                match (mode, self.reported_button) {
                    (Some(MouseMode::Drag | MouseMode::Motion), Some(b)) => {
                        self.report_mouse(b, true, true)
                    }
                    (Some(MouseMode::Motion), None) => {
                        self.report_mouse(ReportButton::None, true, true)
                    }
                    _ => {}
                }
            }
            return;
        }
        if self.left_down
            && let (Some((row, col)), Some(s)) = (cell, &mut self.session)
        {
            let point = s.term.viewport_to_point(row, col);
            s.term.update_selection(point);
            s.window.request_redraw();
        }
    }

    fn mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let Some(s) = &self.session else { return };
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => y as f64 * 3.0,
            MouseScrollDelta::PixelDelta(p) => p.y / s.fonts.metrics.height as f64,
        };
        self.scroll_accum += lines;
        let steps = self.scroll_accum.trunc() as isize;
        if steps == 0 {
            return;
        }
        self.scroll_accum -= steps as f64;
        if self.mouse_reporting() {
            let button = if steps > 0 {
                ReportButton::WheelUp
            } else {
                ReportButton::WheelDown
            };
            for _ in 0..steps.unsigned_abs() {
                self.report_mouse(button, true, false);
            }
            return;
        }
        let Some(s) = &mut self.session else { return };
        if s.term.is_alt_screen() && s.term.modes.alt_scroll {
            // Programs on the alternate screen (less, man, vim) get arrow keys instead.
            let key: &[u8] = match (steps > 0, s.term.modes.app_cursor) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            let bytes = key.repeat(steps.unsigned_abs());
            s.pty.write(&bytes);
        } else {
            s.term.scroll_display(steps);
            s.window.request_redraw();
        }
    }
}

/// After scrollback or the grid shrinks, hand the memory the dropped rows freed back to
/// the OS: glibc keeps freed heap pages otherwise.
fn release_freed_memory(term: &mut Terminal) {
    if term.take_shrunk() {
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        // SAFETY: malloc_trim only releases free heap memory; it has no preconditions.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

fn grid_size(window: &Window, fonts: &Fonts) -> (usize, usize) {
    grid_size_for(window.inner_size(), window.scale_factor(), fonts)
}

fn grid_size_for(size: PhysicalSize<u32>, scale: f64, fonts: &Fonts) -> (usize, usize) {
    let pad = 2.0 * PADDING * scale as f32;
    let cols = ((size.width as f32 - pad) / fonts.metrics.width)
        .floor()
        .max(2.0) as usize;
    let rows = ((size.height as f32 - pad) / fonts.metrics.height)
        .floor()
        .max(1.0) as usize;
    (cols, rows)
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.session.is_some() {
            return;
        }
        match self.start(event_loop) {
            Ok(session) => {
                self.session = Some(session);
                self.process_output();
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Output => self.output_ready(),
            UserEvent::Exit => {
                self.process_output();
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        if let Some(log) = self.session.as_mut().and_then(|s| s.latency.as_mut()) {
            if log.finished(now) {
                event_loop.exit();
                return;
            }
            if let Some(bytes) = log.probe(now) {
                self.write_input(bytes);
            }
        }
        if self.session.as_ref().is_some_and(|s| s.resize.ready(now)) {
            self.resize_grid();
        }
        if self
            .session
            .as_ref()
            .and_then(|s| s.deferred)
            .is_some_and(|since| now >= since + MAX_DEFER)
        {
            self.process_output();
        }
        let deadline = self.session.as_ref().and_then(|s| {
            s.deferred
                .map(|since| since + MAX_DEFER)
                .into_iter()
                .chain(s.resize.deadline)
                .chain(s.latency.as_ref().and_then(|l| l.wake_deadline(now)))
                .min()
        });
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(s) = &mut self.session {
                    s.renderer.resize(size.width, size.height);
                    s.resize.schedule(Instant::now());
                    s.window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => self.set_font_size(self.font_size),
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                if let Some(s) = &mut self.session {
                    if s.term.modes.focus_events {
                        s.pty.write(if focused { b"\x1b[I" } else { b"\x1b[O" });
                    }
                    s.window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed || self.shortcut(&event) {
                    return;
                }
                let Some(s) = &self.session else { return };
                let base = event.key_without_modifiers();
                let bytes = input::encode_key(
                    &event.logical_key,
                    &base,
                    event.text.as_deref(),
                    self.modifiers,
                    s.term.modes.app_cursor,
                    self.options.option_as_alt,
                );
                if let Some(bytes) = bytes {
                    self.write_input(&bytes);
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) => self.write_input(text.as_bytes()),
            WindowEvent::MouseInput { state, button, .. } => self.mouse_input(state, button),
            WindowEvent::CursorMoved { position, .. } => self.cursor_moved(position),
            WindowEvent::MouseWheel { delta, .. } => self.mouse_wheel(delta),
            WindowEvent::RedrawRequested => {
                if let Some(s) = &mut self.session {
                    let pad = PADDING * s.window.scale_factor() as f32;
                    let mut pre_present = || {
                        if let Some(log) = &mut s.latency {
                            log.pre_present();
                        }
                    };
                    if s.renderer.render(
                        &s.term,
                        &mut s.fonts,
                        pad,
                        self.focused,
                        Some(&mut pre_present),
                    ) {
                        if let Some(log) = &mut s.latency {
                            log.frame(Instant::now(), s.since_frame);
                        }
                        s.since_frame = 0;
                    }
                }
            }
            _ => {}
        }
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("show") {
        std::process::exit(show::main(std::env::args().skip(2)));
    }
    let options = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("terminal: {e}");
            std::process::exit(2);
        }
    };
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("terminal: cannot start event loop: {e}");
            std::process::exit(1);
        }
    };
    let mut app = App::new(options, event_loop.create_proxy());
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("terminal: {e}");
        std::process::exit(1);
    }
    if let Some(e) = app.error {
        eprintln!("terminal: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_throttle_leads_trails_and_resets() {
        let now = Instant::now();
        let mut resize = ResizeThrottle::default();
        assert!(!resize.ready(now));
        assert_eq!(resize.deadline, None);
        resize.schedule(now);
        assert!(resize.ready(now));
        resize.applied(now);
        resize.schedule(now + Duration::from_millis(10));
        assert!(!resize.ready(now + Duration::from_millis(99)));
        assert!(resize.ready(now + RESIZE_INTERVAL));
        resize.applied(now + Duration::from_millis(50));
        assert_eq!(resize.deadline, None);
        assert!(!resize.ready(now + RESIZE_INTERVAL));
        resize.schedule(now + RESIZE_INTERVAL);
        assert_eq!(resize.deadline, Some(now + Duration::from_millis(150)));
        resize.applied(now + Duration::from_millis(150));
        resize.schedule(now + Duration::from_millis(300));
        assert!(resize.ready(now + Duration::from_millis(300)));
    }

    #[test]
    fn resize_throttle_keeps_deadline_during_drag() {
        let now = Instant::now();
        let mut resize = ResizeThrottle::default();
        resize.applied(now);
        for millis in (1..100).step_by(8) {
            resize.schedule(now + Duration::from_millis(millis));
            assert_eq!(resize.deadline, Some(now + RESIZE_INTERVAL));
        }
        assert!(!resize.ready(now + Duration::from_millis(99)));
        assert!(resize.ready(now + RESIZE_INTERVAL));
    }

    #[test]
    fn output_markers_reset_modes_before_prompt_bytes() {
        let mut term = Terminal::new(10, 2);
        let setup = b"\x1b[?1000;1004;2004h";
        let prompt = b"\x1b[?2004h$ ";
        advance_output(
            &mut term,
            pty::Output {
                bytes: [
                    setup.as_slice(),
                    prompt.as_slice(),
                    setup.as_slice(),
                    prompt.as_slice(),
                ]
                .concat(),
                shell_returns: vec![setup.len(), setup.len() * 2 + prompt.len()],
            },
        );
        assert_eq!(term.modes.mouse, MouseMode::Off);
        assert!(!term.modes.focus_events);
        assert!(term.modes.bracketed_paste);
        advance_output(
            &mut term,
            pty::Output {
                bytes: Vec::new(),
                shell_returns: Vec::new(),
            },
        );
        assert!(term.modes.bracketed_paste);
    }

    #[test]
    fn guessed_grid_uses_physical_size_and_scaled_padding() {
        let probe = Fonts::new(None, DEFAULT_FONT_SIZE).unwrap();
        let logical = LogicalSize::new(
            100.0 * probe.metrics.width + 2.0 * PADDING,
            30.0 * probe.metrics.height + 2.0 * PADDING,
        );
        for (scale, expected) in [(1.0, (100, 30)), (2.0, (94, 30))] {
            let fonts = Fonts::new(None, DEFAULT_FONT_SIZE * scale as f32).unwrap();
            assert_eq!(
                grid_size_for(logical.to_physical(scale), scale, &fonts),
                expected
            );
            assert_eq!(
                grid_size_for(PhysicalSize::new(0, 0), scale, &fonts),
                (2, 1)
            );
        }
    }
    #[test]
    fn output_is_held_back_only_during_a_flood() {
        assert!(!defer_output(false, 0, false));
        assert!(!defer_output(false, DEFER_AFTER - 1, false));
        assert!(defer_output(false, DEFER_AFTER, false));
        assert!(defer_output(true, 0, false));
        assert!(!defer_output(false, DEFER_AFTER, true));
        assert!(!defer_output(true, 0, true));
    }
}
