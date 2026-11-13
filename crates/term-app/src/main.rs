mod clipboard;
mod command_notify;
mod config;
mod drop_files;
mod flow_control;
#[cfg(any(target_os = "linux", test))]
mod frame_clock;
mod input;
mod latency;
#[cfg(target_os = "linux")]
mod present_wayland;
mod pty;
mod search;
mod show;

use config::Action;
use input::MouseButton as ReportButton;
use pty::Pty;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use term_core::{MouseMode, Point, SelectionKind, Terminal};
#[cfg(test)]
use term_render::PADDING;
use term_render::{Fonts, Renderer};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{Window, WindowId};

const DEFAULT_FONT_SIZE: f32 = 14.0;

#[derive(Default)]
struct PixelScroll {
    offset: f64,
}

impl PixelScroll {
    fn reset(&mut self) {
        self.offset = 0.0;
    }

    fn update(&mut self, delta: f64, height: f64, lines: usize, limit: usize) -> isize {
        if !delta.is_finite() || !height.is_finite() || height <= 0.0 {
            return 0;
        }
        let pixels =
            (lines as f64 * height + self.offset + delta).clamp(0.0, limit as f64 * height);
        let whole = (pixels / height).floor() as usize;
        self.offset = if whole == limit {
            0.0
        } else {
            pixels - whole as f64 * height
        };
        whole as isize - lines as isize
    }
}
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
    CommandFinished(command_notify::Finished),
    ConfigChanged,
    Output,
    Exit,
}

struct Options {
    config_path: PathBuf,
    font_size_override: bool,
    scrollback: Option<usize>,
    font: Option<PathBuf>,
    font_size: f32,
    option_as_alt: bool,
    copy_on_select: bool,
    command: Vec<String>,
}

impl Options {
    fn scrollback(&self, config: &config::Config) -> usize {
        self.scrollback.unwrap_or(config.scrollback)
    }

    fn font<'a>(&'a self, config: &'a config::Config) -> Option<&'a std::path::Path> {
        self.font.as_deref().or(config.font.as_deref())
    }

    fn size(&self, config: &config::Config) -> f32 {
        if self.font_size_override {
            self.font_size
        } else {
            config.font_size
        }
    }

    fn command<'a>(&'a self, config: &'a config::Config) -> &'a [String] {
        if self.command.is_empty() {
            &config.shell
        } else {
            &self.command
        }
    }
}

fn usage() -> ! {
    println!(
        "{} {} - a fast GPU-rendered terminal emulator\n\n\
         USAGE: terminal [OPTIONS] [-e COMMAND [ARGS...]]\n       \
         terminal show [--width N] [--height N] [--fit] FILE...\n\n\
         OPTIONS:\n  \
           --config <PATH>     Override the TOML config file location\n  \
           --font <PATH>       TrueType/OpenType font file (default: bundled JetBrains Mono)\n  \
           --font-size <SIZE>  Font size in points (default: {DEFAULT_FONT_SIZE})\n  \
           --option-as-alt     On macOS, make Option act as Alt/Meta\n  \
           --scrollback <LINES> History lines (default: 10000; 0 disables; max: 1000000)\n  \
           --copy-on-select    Copy selected text to the clipboard on mouse release\n  \
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
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<Options, String> {
    let mut options = Options {
        config_path: config::default_path(),
        font_size_override: false,
        scrollback: None,
        font: None,
        font_size: DEFAULT_FONT_SIZE,
        option_as_alt: !cfg!(target_os = "macos"),
        copy_on_select: false,
        command: Vec::new(),
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" => options.config_path = args.next().ok_or("--config needs a path")?.into(),
            "--scrollback" => {
                options.scrollback = Some(
                    args.next()
                        .and_then(|s| s.parse().ok())
                        .filter(|n: &usize| *n <= 1_000_000)
                        .ok_or("--scrollback needs an integer between 0 and 1000000")?,
                );
            }
            "--font" => options.font = Some(args.next().ok_or("--font needs a path")?.into()),
            "--font-size" => {
                options.font_size_override = true;
                options.font_size = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|s: &f32| *s >= 4.0 && *s <= 200.0)
                    .ok_or("--font-size needs a number between 4 and 200")?;
            }
            "--option-as-alt" => options.option_as_alt = true,
            "--copy-on-select" => options.copy_on_select = true,
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
    scroll: PixelScroll,
    ime_cursor_area: Option<[f32; 4]>,
    pty_cell: (f32, f32),
    window: Arc<Window>,
    renderer: Renderer,
    fonts: Fonts,
    term: Terminal,
    pty: Pty,
    /// Output parsed since the last frame was drawn.
    since_frame: usize,
    /// When output was first held back.
    deferred: Option<Instant>,
    redraw_at: Option<(Instant, Instant)>,
    target: Option<Instant>,
    sequence: usize,
    #[cfg(target_os = "linux")]
    presentation: Option<present_wayland::Feedback>,
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

fn update_terminal(
    term: &mut Terminal,
    scroll: &mut PixelScroll,
    update: impl FnOnce(&mut Terminal),
) {
    let offset = term.display_offset();
    let history = term.viewport_to_point(0, 0).line + offset as u64;
    let alt = term.is_alt_screen();
    update(term);
    let shrunk = release_freed_memory(term);
    let new_history = term.viewport_to_point(0, 0).line + term.display_offset() as u64;
    if shrunk || alt || term.is_alt_screen() || new_history < history {
        scroll.reset();
        return;
    }
    let added = new_history - history;
    if offset == 0 && scroll.offset > 0.0 {
        term.scroll_display(added.min(isize::MAX as u64) as isize);
    } else if term.display_offset()
        != (offset as u64 + added).min(term.scrollback_len() as u64) as usize
    {
        scroll.reset();
    }
    if term.display_offset() == term.scrollback_len() {
        scroll.reset();
    }
}

impl Session {
    fn frame_deadline(&mut self) -> Option<(Instant, Instant)> {
        #[cfg(target_os = "linux")]
        if let Some(feedback) = &mut self.presentation {
            return feedback.clock()?.deadline(Instant::now());
        }
        None
    }

    fn request_frame(&mut self) {
        if let Some(deadline) = self.frame_deadline() {
            self.redraw_at = Some(self.redraw_at.map_or(deadline, |old| old.min(deadline)));
        } else {
            self.window.request_redraw();
        }
    }
}

/// Whether output that just arrived can be held back: only during a flood, and never once
/// the reader is stalled on a full queue.
fn defer_output(deferring: bool, since_frame: usize, queue_full: bool) -> bool {
    (deferring || since_frame >= DEFER_AFTER) && !queue_full
}

fn frame_wakeup(redraw_at: Option<Instant>, deferred: Option<Instant>) -> Option<Instant> {
    redraw_at.or_else(|| deferred.map(|since| since + MAX_DEFER))
}

fn wake_deadline(
    frame: Option<Instant>,
    resize: Option<Instant>,
    probe: Option<Instant>,
) -> Option<Instant> {
    [frame, resize, probe].into_iter().flatten().min()
}

fn shortcut_chord(key: Key, mods: ModifiersState) -> Option<config::Chord> {
    let modifiers = u8::from(mods.control_key())
        | (u8::from(mods.shift_key()) << 1)
        | (u8::from(mods.alt_key()) << 2)
        | (u8::from(mods.super_key()) << 3);
    let key = match key {
        Key::Character(c) if c == " " => "space".into(),
        Key::Character(c) => c.to_lowercase(),
        Key::Named(named) => match named {
            winit::keyboard::NamedKey::ArrowUp => "up".into(),
            winit::keyboard::NamedKey::ArrowDown => "down".into(),
            winit::keyboard::NamedKey::ArrowLeft => "left".into(),
            winit::keyboard::NamedKey::ArrowRight => "right".into(),
            _ => format!("{named:?}").to_lowercase(),
        },
        _ => return None,
    };
    Some(config::Chord { modifiers, key })
}

struct App {
    search: Option<search::Search>,
    config: config::Config,
    config_error: Option<String>,
    _watcher: Option<config::ConfigWatcher>,
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

fn select_at(term: &mut Terminal, point: Point, modifiers: ModifiersState, clicks: u8) {
    if modifiers.shift_key() && clicks == 1 && term.selection.is_some() {
        // Keep the original anchor and selection shape, including rectangular selections.
        term.update_selection(point);
    } else {
        let kind = if modifiers.alt_key() {
            SelectionKind::Block
        } else {
            match clicks {
                2 => SelectionKind::Word,
                3 => SelectionKind::Line,
                _ => SelectionKind::Simple,
            }
        };
        term.start_selection(point, kind);
    }
}

impl App {
    fn new(options: Options, proxy: EventLoopProxy<UserEvent>) -> App {
        let (config, config_error) = match config::load(&options.config_path) {
            Ok(config) => (config, None),
            Err(error) => {
                eprintln!("{error}");
                (config::Config::default(), Some(error))
            }
        };
        let path = std::path::absolute(&options.config_path).unwrap_or(options.config_path.clone());
        let wake = proxy.clone();
        let watcher = config::watch(path, move || {
            let _ = wake.send_event(UserEvent::ConfigChanged);
        })
        .map_err(|e| eprintln!("terminal: config reload unavailable: {e}"))
        .ok();
        App {
            search: None,
            font_size: options.size(&config),
            config,
            config_error,
            _watcher: watcher,
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
        let probe = match Fonts::new(self.options.font(&self.config), self.font_size) {
            Ok(fonts) => fonts,
            Err(error) if self.options.font.is_none() => {
                let error = format!(
                    "terminal: {}:1:1: font: {error}",
                    self.options.config_path.display()
                );
                eprintln!("{error}");
                self.config_error = Some(error);
                self.config = config::Config::default();
                self.font_size = self.options.size(&self.config);
                Fonts::new(None, self.font_size)?
            }
            Err(error) => return Err(error),
        };
        let font = self.options.font(&self.config);
        let m = probe.metrics;
        let logical_size = LogicalSize::new(
            100.0 * m.width + 2.0 * self.config.padding,
            30.0 * m.height + 2.0 * self.config.padding,
        );
        let guess = event_loop
            .primary_monitor()
            .map_or(1.0, |m| m.scale_factor());
        let guessed_fonts = if guess == 1.0 {
            probe
        } else {
            Fonts::new(font, self.font_size * guess as f32)?
        };
        let (spawn_cols, spawn_rows) = grid_size_for(
            logical_size.to_physical(guess),
            guess,
            &guessed_fonts,
            self.config.padding,
        );
        let spawn_cell = (guessed_fonts.metrics.width, guessed_fonts.metrics.height);
        let proxy = self.proxy.clone();
        let command_proxy = self.proxy.clone();
        let mut pty = Pty::spawn(
            self.options.command(&self.config),
            &self.config.env,
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
            move |finished| {
                let _ = command_proxy.send_event(UserEvent::CommandFinished(finished));
            },
        )?;
        pty.set_command_notifications(self.config.notify_long_commands_after != 0)?;
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
            Fonts::new(font, self.font_size * scale as f32)?
        };
        let size = window.inner_size();
        let renderer = Renderer::new(window.clone(), size.width, size.height)?;
        eprintln!("terminal: rendering with {}", renderer.adapter_info);

        let (cols, rows) = grid_size(&window, &fonts, self.config.padding);
        let mut term = Terminal::new(cols, rows);
        term.palette = self.config.palette.clone();
        term.set_scrollback_limit(self.options.scrollback(&self.config));
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
        #[cfg(target_os = "linux")]
        let presentation = {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if matches!(
                window.window_handle().map(|h| h.as_raw()),
                Ok(RawWindowHandle::Wayland(_))
            ) {
                let (start, lines) = latency
                    .as_ref()
                    .map(|log| {
                        let (start, lines) = log.wayland_log();
                        (start, Some(lines))
                    })
                    .unwrap_or_else(|| (Instant::now(), None));
                match present_wayland::Feedback::new(window.clone(), start, lines) {
                    Ok(feedback) => Some(feedback),
                    Err(error) => {
                        eprintln!("terminal: presentation feedback unavailable: {error}");
                        None
                    }
                }
            } else {
                None
            }
        };
        Ok(Session {
            scroll: PixelScroll::default(),
            ime_cursor_area: None,
            pty_cell: cell,
            latency,
            window,
            renderer,
            fonts,
            term,
            pty,
            since_frame: 0,
            deferred: None,
            resize: ResizeThrottle::default(),
            redraw_at: None,
            target: None,
            sequence: 0,
            #[cfg(target_os = "linux")]
            presentation,
        })
    }

    fn resize_grid(&mut self) {
        let Some(s) = &mut self.session else { return };
        s.scroll.reset();
        s.resize.applied(Instant::now());
        let (cols, rows) = grid_size(&s.window, &s.fonts, self.config.padding);
        let cell = (s.fonts.metrics.width, s.fonts.metrics.height);
        let grid_changed = (cols, rows) != (s.term.cols(), s.term.rows());
        s.term
            .set_cell_pixel_size(s.fonts.metrics.width, s.fonts.metrics.height);
        if grid_changed {
            s.term.resize(cols, rows);
            release_freed_memory(&mut s.term);
            if let Some(search) = &mut self.search {
                search.dirty = true;
            }
        }
        if pty_resize_needed(grid_changed, s.pty_cell, cell) {
            s.pty.resize(cols, rows, cell);
            s.pty_cell = cell;
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
        if let Some(search) = &mut self.search {
            search.dirty = true;
        }
        let Some(s) = &mut self.session else { return };
        s.pty.wake_pending.store(false, Ordering::Release);
        s.deferred = None;
        let batch = s.pty.take_output();
        let processed = batch.bytes.len();
        s.since_frame += processed;
        update_terminal(&mut s.term, &mut s.scroll, |term| {
            advance_output(term, batch)
        });
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
            s.request_frame();
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        let Some(s) = &mut self.session else { return };
        if let Some(log) = &mut s.latency {
            log.input(Instant::now());
        }
        s.term.scroll_to_bottom();
        s.scroll.reset();
        s.term.selection = None;
        s.pty.write(bytes);
        s.request_frame();
    }

    fn paste(&mut self, primary: bool) {
        let Some(text) = self.clipboard.get(primary) else {
            return;
        };
        if let (Some(search), Some(s)) = (&mut self.search, &mut self.session) {
            search.insert(&text, &mut s.term);
            s.scroll.reset();
            s.window.request_redraw();
            return;
        }
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

    fn dropped_file(&mut self, path: PathBuf) {
        // Flush any deferred echo before inserting rows above the current prompt.
        self.process_output();
        let Some(s) = &mut self.session else { return };
        let Some(bytes) = drop_files::quoted_path(&path) else {
            eprintln!("terminal: cannot drop a path containing control characters");
            return;
        };
        s.term.scroll_to_bottom();
        if drop_files::preview_enabled(self.config.drop_preview, &s.term)
            && let Some((image, rows)) =
                show::drop_preview(&path, s.term.cols(), s.term.rows(), s.pty_cell)
        {
            drop_files::insert_preview(&mut s.term, &image, rows);
        }
        // Only the literal, quoted path goes to the shell: never a show command or Enter.
        self.write_input(&bytes);
    }

    fn copy(&mut self, primary: bool) {
        let Some(s) = &self.session else { return };
        if let Some(text) = s.term.selection_text().filter(|t| !t.is_empty()) {
            self.clipboard.set(text, primary);
        }
    }

    fn reload_config(&mut self) {
        let result = config::load(&self.options.config_path).and_then(|next| {
            let font_changed = self.options.font.is_none() && next.font != self.config.font;
            let size_changed =
                !self.options.font_size_override && next.font_size != self.config.font_size;
            let size = if size_changed {
                next.font_size
            } else {
                self.font_size
            };
            let fonts = if font_changed {
                let scale = self
                    .session
                    .as_ref()
                    .map_or(1.0, |s| s.window.scale_factor());
                Some(
                    Fonts::new(next.font.as_deref(), size * scale as f32).map_err(|e| {
                        format!(
                            "terminal: {}:1:1: font: {e}",
                            self.options.config_path.display()
                        )
                    })?,
                )
            } else {
                None
            };
            if next.shell != self.config.shell || next.env != self.config.env {
                eprintln!("terminal: shell/env changes apply on the next launch");
            }
            self.config = next;
            if let Some(s) = &mut self.session {
                s.pty
                    .set_command_notifications(self.config.notify_long_commands_after != 0)?;
                s.term.palette = self.config.palette.clone();
                s.term
                    .set_scrollback_limit(self.options.scrollback(&self.config));
                if let Some(search) = &mut self.search {
                    search.dirty = true;
                }
                release_freed_memory(&mut s.term);
                if let Some(fonts) = fonts {
                    s.fonts = fonts;
                    s.renderer.clear_glyph_cache();
                }
            }
            if size_changed {
                self.set_font_size(size);
            }
            self.resize_grid();
            Ok(())
        });
        self.config_error = result.err();
        if let Some(error) = &self.config_error {
            eprintln!("{error}");
        }
        if let Some(s) = &self.session {
            s.window.request_redraw();
        }
    }

    /// Handles terminal shortcuts; true if the key was consumed.
    fn shortcut(&mut self, event: &KeyEvent) -> bool {
        let mods = self.modifiers;
        let Some(chord) = shortcut_chord(event.key_without_modifiers(), mods) else {
            return false;
        };
        let Some(action) = self.config.bindings.get(&chord).cloned() else {
            return cfg!(target_os = "macos") && mods.super_key();
        };
        match action {
            Action::Search => {
                self.search.get_or_insert_with(search::Search::default);
                self.update_ime_cursor_area();
                if let Some(s) = &self.session {
                    s.window.request_redraw();
                }
            }
            Action::None => return false,
            Action::Copy => self.copy(false),
            Action::Paste => self.paste(false),
            Action::PastePrimary => self.paste(true),
            Action::Increase => self.set_font_size(self.font_size + 1.0),
            Action::Decrease => self.set_font_size(self.font_size - 1.0),
            Action::Reset => self.set_font_size(if self.options.font_size_override {
                self.options.font_size
            } else {
                self.config.font_size
            }),
            Action::Send(text) => {
                if self.search.is_none() {
                    self.write_input(text.as_bytes());
                }
            }
            Action::Clear => {
                if self.search.is_none() {
                    self.write_input(b"\x0c");
                }
                if let Some(s) = &mut self.session {
                    s.term.advance(b"\x1b[3J");
                    if let Some(search) = &mut self.search {
                        search.dirty = true;
                    }
                    release_freed_memory(&mut s.term);
                    s.request_frame();
                }
            }
            Action::PageUp | Action::PageDown | Action::Top | Action::Bottom => {
                if let Some(s) = &mut self.session {
                    if matches!(action, Action::Top | Action::Bottom) && s.term.is_alt_screen() {
                        return false;
                    }
                    let page = s.term.rows() as isize - 1;
                    let delta = match action {
                        Action::PageUp => page,
                        Action::PageDown => -page,
                        Action::Top => isize::MAX / 2,
                        _ => -(isize::MAX / 2),
                    };
                    s.scroll.reset();
                    s.term.scroll_display(delta);
                    s.request_frame();
                }
            }
        }
        true
    }

    fn search_key(&mut self, event: &KeyEvent) {
        use winit::keyboard::NamedKey;
        if event.logical_key == Key::Named(NamedKey::Escape) {
            self.search = None;
        } else if let (Some(search), Some(s)) = (&mut self.search, &mut self.session) {
            let mut changed = false;
            let base = event.key_without_modifiers();
            let base = match &base {
                Key::Character(c) => c.as_str(),
                _ => "",
            };
            match &event.logical_key {
                Key::Named(NamedKey::Enter) => {
                    search.step(&mut s.term, !self.modifiers.shift_key())
                }
                Key::Named(NamedKey::ArrowUp) => search.step(&mut s.term, true),
                Key::Named(NamedKey::ArrowDown) => search.step(&mut s.term, false),
                Key::Named(NamedKey::Backspace) => {
                    search.query.pop();
                    changed = true;
                }
                Key::Character(c)
                    if search::toggles_regex(
                        c,
                        base,
                        self.modifiers,
                        self.options.option_as_alt,
                    ) =>
                {
                    search.regex = !search.regex;
                    changed = true;
                }
                _ if search::accepts_text(self.modifiers, self.options.option_as_alt) => {
                    if let Some(text) = &event.text {
                        search
                            .query
                            .extend(text.chars().filter(|c| !c.is_control()));
                        changed = true;
                    }
                }
                _ => {}
            }
            if changed {
                search.refresh(&mut s.term, true);
            }
            s.scroll.reset();
        }
        if let Some(s) = &self.session {
            s.window.request_redraw();
        }
        self.update_ime_cursor_area();
    }

    fn update_ime_cursor_area(&mut self) {
        let Some(s) = &mut self.session else { return };
        let m = s.fonts.metrics;
        let area = if let Some(search) = &self.search {
            let size = s.window.inner_size();
            term_render::search_layout(
                search.cursor_column(),
                search.preedit_columns(),
                search.status().chars().count(),
                size.width as f32,
                size.height as f32,
                m.width,
                m.height,
            )
            .cursor_area
        } else {
            let (row, col) = s.term.cursor_position().unwrap_or((0, 0));
            let pad = self.config.padding * s.window.scale_factor() as f32;
            [
                pad + col as f32 * m.width,
                pad + row as f32 * m.height + s.scroll.offset as f32,
                m.width,
                m.height,
            ]
        };
        if s.ime_cursor_area == Some(area) {
            return;
        }
        s.window.set_ime_cursor_area(
            PhysicalPosition::new(area[0] as f64, area[1] as f64),
            PhysicalSize::new(area[2] as f64, area[3] as f64),
        );
        s.ime_cursor_area = Some(area);
    }

    fn cell_at(&self, pos: PhysicalPosition<f64>) -> Option<(isize, usize)> {
        let s = self.session.as_ref()?;
        let pad = self.config.padding as f64 * s.window.scale_factor();
        let m = s.fonts.metrics;
        let col = ((pos.x - pad) / m.width as f64).floor().max(0.0) as usize;
        let row = ((pos.y - pad - s.scroll.offset) / m.height as f64).floor() as isize;
        Some((
            row.clamp(
                if s.scroll.offset > 0.0 { -1 } else { 0 },
                s.term.rows() as isize - 1,
            ),
            col.min(s.term.cols() - 1),
        ))
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
        if let Some(bytes) = input::encode_mouse(
            button,
            pressed,
            motion,
            col,
            row.max(0) as usize,
            self.modifiers,
            sgr,
        ) {
            s.pty.write(&bytes);
        }
    }

    fn mouse_input(&mut self, state: ElementState, button: MouseButton) {
        let pressed = state == ElementState::Pressed;
        // Finish a local drag even if Shift was released while mouse reporting is on.
        if button == MouseButton::Left && !pressed && self.left_down {
            self.left_down = false;
            self.copy(true);
            if self.options.copy_on_select {
                self.copy(false);
            }
            return;
        }
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
                if !pressed {
                    return;
                }
                self.left_down = true;
                let Some((row, col)) = self.cell_at(self.mouse_pos) else {
                    return;
                };
                let Some(s) = &mut self.session else { return };
                let point = s.term.viewport_to_point_signed(row, col);
                // Shift is also the local-selection override in mouse-tracking apps,
                // where double/triple clicks must still select words/lines.
                let modified = self.modifiers.alt_key();
                let clicks = match self.last_click.filter(|_| !modified) {
                    Some((at, p, n)) if p == point && at.elapsed() < Duration::from_millis(400) => {
                        n % 3 + 1
                    }
                    _ => 1,
                };
                self.last_click = (!modified).then_some((Instant::now(), point, clicks));
                select_at(&mut s.term, point, self.modifiers, clicks);
                s.request_frame();
            }
            MouseButton::Middle if pressed => self.paste(true),
            _ => {}
        }
    }

    fn cursor_moved(&mut self, pos: PhysicalPosition<f64>) {
        let old = self.cell_at(self.mouse_pos);
        self.mouse_pos = pos;
        let cell = self.cell_at(pos);
        if self.left_down {
            if let (Some((row, col)), Some(s)) = (cell, &mut self.session) {
                let point = s.term.viewport_to_point_signed(row, col);
                s.term.update_selection(point);
                s.request_frame();
            }
            return;
        }
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
        }
    }

    fn mouse_wheel(&mut self, delta: MouseScrollDelta) {
        let Some(s) = &self.session else { return };
        if !self.mouse_reporting() && !(s.term.is_alt_screen() && s.term.modes.alt_scroll) {
            let s = self.session.as_mut().unwrap();
            match delta {
                MouseScrollDelta::PixelDelta(p) => {
                    let steps = s.scroll.update(
                        p.y,
                        s.fonts.metrics.height as f64,
                        s.term.display_offset(),
                        s.term.scrollback_len(),
                    );
                    s.term.scroll_display(steps);
                    self.scroll_accum = 0.0;
                    s.request_frame();
                    return;
                }
                MouseScrollDelta::LineDelta(..) => s.scroll.reset(),
            }
        }
        let s = self.session.as_ref().unwrap();
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
            s.request_frame();
        }
    }
}

/// After scrollback or the grid shrinks, hand the memory the dropped rows freed back to
/// the OS: glibc keeps freed heap pages otherwise.
fn release_freed_memory(term: &mut Terminal) -> bool {
    let shrunk = term.take_shrunk();
    if shrunk {
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        // SAFETY: malloc_trim only releases free heap memory; it has no preconditions.
        unsafe {
            libc::malloc_trim(0);
        }
    }
    shrunk
}

fn pty_resize_needed(grid_changed: bool, previous_cell: (f32, f32), cell: (f32, f32)) -> bool {
    grid_changed || previous_cell != cell
}

fn grid_size(window: &Window, fonts: &Fonts, padding: f32) -> (usize, usize) {
    grid_size_for(window.inner_size(), window.scale_factor(), fonts, padding)
}

fn grid_size_for(
    size: PhysicalSize<u32>,
    scale: f64,
    fonts: &Fonts,
    padding: f32,
) -> (usize, usize) {
    let pad = 2.0 * padding * scale as f32;
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
                self.clipboard = clipboard::Clipboard::new(session.window.clone());
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
            UserEvent::CommandFinished(finished) => {
                if command_notify::should_notify(
                    self.focused,
                    finished.duration,
                    self.config.notify_long_commands_after,
                ) {
                    command_notify::notify(finished);
                }
            }
            UserEvent::ConfigChanged => self.reload_config(),
            UserEvent::Output => self.output_ready(),
            UserEvent::Exit => {
                self.process_output();
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(s) = &mut self.session
            && std::mem::take(&mut s.pty.flow_redraw)
        {
            s.request_frame();
        }
        #[cfg(target_os = "linux")]
        if let Some(feedback) = self.session.as_mut().and_then(|s| s.presentation.as_mut()) {
            let _ = feedback.clock();
        }
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
        let deadline = self
            .session
            .as_ref()
            .and_then(|s| frame_wakeup(s.redraw_at.map(|(latch, _)| latch), s.deferred));
        if deadline.is_some_and(|deadline| now >= deadline) {
            self.process_output();
            if let Some(s) = &mut self.session
                && let Some((_, target)) = s.redraw_at.take()
            {
                s.target = Some(target);
                s.window.request_redraw();
            }
        }
        let deadline = self.session.as_ref().and_then(|s| {
            wake_deadline(
                frame_wakeup(s.redraw_at.map(|(latch, _)| latch), s.deferred),
                s.resize.deadline,
                s.latency.as_ref().and_then(|l| l.wake_deadline(now)),
            )
        });
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(search) = &mut self.search {
                    search.dirty = true;
                }
                if let Some(s) = &mut self.session {
                    s.renderer.resize(size.width, size.height);
                    s.resize.schedule(Instant::now());
                    s.window.request_redraw();
                }
                self.update_ime_cursor_area();
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
            WindowEvent::DroppedFile(path) => self.dropped_file(path),
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed || self.shortcut(&event) {
                    return;
                }
                if self.search.is_some() {
                    self.search_key(&event);
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
            WindowEvent::Ime(Ime::Commit(text)) => {
                if let (Some(search), Some(s)) = (&mut self.search, &mut self.session) {
                    search.insert(&text, &mut s.term);
                    s.scroll.reset();
                    s.window.request_redraw();
                } else {
                    self.write_input(text.as_bytes());
                }
                self.update_ime_cursor_area();
            }
            WindowEvent::Ime(event) => {
                if let Some(search) = &mut self.search {
                    match event {
                        Ime::Preedit(text, cursor) => search.set_preedit(text, cursor),
                        Ime::Disabled => search.clear_preedit(),
                        _ => {}
                    }
                }
                self.update_ime_cursor_area();
                if let Some(s) = &self.session {
                    s.window.request_redraw();
                }
            }
            WindowEvent::MouseInput { state, button, .. } => self.mouse_input(state, button),
            WindowEvent::CursorMoved { position, .. } => self.cursor_moved(position),
            WindowEvent::MouseWheel { delta, .. } => self.mouse_wheel(delta),
            WindowEvent::RedrawRequested => {
                if let (Some(s), Some(search)) = (&mut self.session, &mut self.search)
                    && search.dirty
                {
                    search.refresh(&mut s.term, false);
                }
                self.update_ime_cursor_area();
                if let Some(s) = &mut self.session {
                    let input = self.search.as_ref().map(search::Search::input);
                    let status = self.search.as_ref().map(search::Search::status);
                    let search = self
                        .search
                        .as_ref()
                        .map(|search| term_render::SearchOverlay {
                            matches: &search.matches,
                            current: search.current,
                            input: input.as_deref().unwrap_or_default(),
                            status: status.as_deref().unwrap_or_default(),
                            preedit: search.preedit_columns(),
                            cursor: search.cursor_column(),
                        });
                    let target = s
                        .target
                        .take()
                        .or_else(|| s.redraw_at.take().map(|(_, target)| target));
                    let pad = self.config.padding * s.window.scale_factor() as f32;
                    s.pty.refresh_flow();
                    let banner = match (self.config_error.as_deref(), s.pty.flow.banner()) {
                        (Some(error), Some(flow)) => Some(format!("{error} | {flow}")),
                        (Some(error), None) => Some(error.to_owned()),
                        (None, flow) => flow,
                    };
                    let started = Instant::now();
                    let mut pre_present = || {
                        #[cfg(target_os = "linux")]
                        if let Some(feedback) = &mut s.presentation {
                            let cost = started.elapsed();
                            if let Some(clock) = feedback.clock() {
                                clock.submitted(s.sequence, target, cost);
                            }
                            feedback.request(s.sequence);
                        }
                        #[cfg(not(target_os = "linux"))]
                        let _ = (started, target);
                    };
                    if s.renderer.render(
                        &s.term,
                        &mut s.fonts,
                        pad,
                        s.scroll.offset as f32,
                        self.focused,
                        banner.as_deref(),
                        search.as_ref(),
                        Some(&mut pre_present),
                    ) {
                        if let Some(log) = &mut s.latency {
                            log.frame(Instant::now(), s.since_frame);
                        }
                        s.since_frame = 0;
                        s.sequence += 1;
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
    fn clipboard_shortcuts_dispatch() {
        let bindings = config::default_bindings_for(false);
        for (key, mods, action) in [
            (
                Key::Character("c".into()),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                Action::Copy,
            ),
            (
                Key::Character("C".into()),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                Action::Copy,
            ),
            (
                Key::Character("v".into()),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                Action::Paste,
            ),
            (
                Key::Character("V".into()),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                Action::Paste,
            ),
            (
                Key::Named(winit::keyboard::NamedKey::Insert),
                ModifiersState::SHIFT,
                Action::Paste,
            ),
        ] {
            assert_eq!(
                bindings.get(&shortcut_chord(key, mods).unwrap()),
                Some(&action)
            );
        }
    }
    #[test]
    fn configured_arrows_dispatch_with_named_keys() {
        use winit::keyboard::NamedKey;
        for (name, key) in [
            ("up", NamedKey::ArrowUp),
            ("down", NamedKey::ArrowDown),
            ("left", NamedKey::ArrowLeft),
            ("right", NamedKey::ArrowRight),
        ] {
            let config = config::parse(
                std::path::Path::new("test.toml"),
                &format!("[keybindings]\n'ctrl+{name}' = 'scroll_to_top'"),
            )
            .unwrap();
            let chord = shortcut_chord(Key::Named(key), ModifiersState::CONTROL).unwrap();
            assert_eq!(config.bindings.get(&chord), Some(&Action::Top));
            assert_eq!(
                config::Chord::parse(&format!("ctrl+arrow{name}")).unwrap(),
                chord
            );
            assert!(
                !config.bindings.contains_key(
                    &shortcut_chord(
                        Key::Named(key),
                        ModifiersState::CONTROL | ModifiersState::SHIFT
                    )
                    .unwrap()
                )
            );
        }
        assert!(
            shortcut_chord(
                Key::Unidentified(winit::keyboard::NativeKey::Unidentified),
                ModifiersState::empty()
            )
            .is_none()
        );
    }

    #[test]
    fn mac_font_shortcuts_dispatch_with_exact_modifiers() {
        let bindings = config::default_bindings_for(true);
        for (key, action) in [
            ("=", Action::Increase),
            ("-", Action::Decrease),
            ("0", Action::Reset),
        ] {
            for mods in [
                ModifiersState::SUPER,
                ModifiersState::SUPER | ModifiersState::SHIFT,
            ] {
                let chord = shortcut_chord(Key::Character(key.into()), mods).unwrap();
                assert_eq!(bindings.get(&chord), Some(&action));
                let extra =
                    shortcut_chord(Key::Character(key.into()), mods | ModifiersState::ALT).unwrap();
                assert!(!bindings.contains_key(&extra));
            }
        }
    }
    #[test]
    fn cli_overrides_config() {
        let config = super::config::parse(
            std::path::Path::new("test.toml"),
            "font = '/config.ttf'\nfont_size = 22\nscrollback = 42\nshell = ['zsh', '-l']",
        )
        .unwrap();
        let mut options = super::Options {
            config_path: "test.toml".into(),
            scrollback: None,
            font_size_override: false,
            font: None,
            font_size: super::DEFAULT_FONT_SIZE,
            command: Vec::new(),
            option_as_alt: true,
            copy_on_select: false,
        };
        assert_eq!(
            options.font(&config),
            Some(std::path::Path::new("/config.ttf"))
        );
        assert_eq!(options.size(&config), 22.0);
        assert_eq!(options.scrollback(&config), 42);
        assert_eq!(options.command(&config), ["zsh", "-l"]);
        options.font = Some("/cli.ttf".into());
        options.font_size_override = true;
        options.font_size = 18.0;
        options.scrollback = Some(90);
        options.command = vec!["bash".into()];
        assert_eq!(
            options.font(&config),
            Some(std::path::Path::new("/cli.ttf"))
        );
        assert_eq!(options.size(&config), 18.0);
        assert_eq!(options.scrollback(&config), 90);
        let mut reloaded = config.clone();
        reloaded.scrollback = 123;
        assert_eq!(options.scrollback(&reloaded), 90);
        assert_eq!(options.command(&config), ["bash"]);
    }

    #[test]
    fn scrollback_option() {
        let parse = |args: &[&str]| parse_args_from(args.iter().map(|s| s.to_string()));
        assert_eq!(
            parse(&[]).unwrap().scrollback(&config::Config::default()),
            10_000
        );
        for n in [0, 50, 1_000_000] {
            assert_eq!(
                parse(&["--scrollback", &n.to_string()])
                    .unwrap()
                    .scrollback(&config::Config::default()),
                n
            );
        }
        for args in [
            vec!["--scrollback"],
            vec!["--scrollback", "x"],
            vec!["--scrollback", "-1"],
            vec!["--scrollback", "1000001"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn smooth_pixels_cross_boundaries_clamp_and_reset() {
        let mut scroll = PixelScroll::default();
        let mut lines = 0;
        for (delta, expected_lines, expected_pixels) in [
            (5.0, 0, 5.0),
            (18.0, 2, 3.0),
            (-8.0, 1, 5.0),
            (-100.0, 0, 0.0),
            (100.0, 5, 0.0),
            (-5.0, 4, 5.0),
        ] {
            lines = (lines as isize + scroll.update(delta, 10.0, lines, 5)) as usize;
            assert_eq!((lines, scroll.offset), (expected_lines, expected_pixels));
        }
        scroll.reset();
        assert_eq!(scroll.offset, 0.0);
        assert_eq!(scroll.update(5.0, 10.0, 0, 0), 0);
        assert_eq!(scroll.offset, 0.0);
        assert_eq!(scroll.update(f64::NAN, 10.0, 0, 5), 0);
        assert_eq!(scroll.update(5.0, 0.0, 0, 5), 0);
    }

    fn streaming_terminal(limit: usize) -> Terminal {
        let mut term = Terminal::new(20, 5);
        term.set_scrollback_limit(limit);
        for line in 0..80 {
            term.advance(format!("line {line}\r\n").as_bytes());
        }
        release_freed_memory(&mut term);
        term
    }

    fn drawn_rows(term: &Terminal) -> Vec<String> {
        (-1..term.rows() as isize)
            .map(|row| {
                term.viewport_row_signed(row)
                    .cells
                    .iter()
                    .map(|cell| cell.c)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn streaming_output_preserves_fraction_and_rows() {
        for initial_lines in [0, 30] {
            let mut term = streaming_terminal(100);
            let mut scroll = PixelScroll::default();
            let steps = scroll.update(
                initial_lines as f64 * 20.0 + 9.0,
                20.0,
                term.display_offset(),
                term.scrollback_len(),
            );
            term.scroll_display(steps);
            let rows = drawn_rows(&term);
            for added in 1..=3 {
                update_terminal(&mut term, &mut scroll, |term| term.advance(b"\r\nnext"));
                assert_eq!(term.display_offset(), initial_lines + added);
                assert_eq!(scroll.offset, 9.0);
                assert_eq!(drawn_rows(&term), rows);
            }
        }
    }

    #[test]
    fn fractional_bottom_anchors_a_batch_of_output_lines() {
        let mut term = streaming_terminal(100);
        let mut scroll = PixelScroll { offset: 9.0 };
        let rows = drawn_rows(&term);
        update_terminal(&mut term, &mut scroll, |term| {
            term.advance(b"\r\na\r\nb\r\nc")
        });
        assert_eq!(term.display_offset(), 3);
        assert_eq!(scroll.offset, 9.0);
        assert_eq!(drawn_rows(&term), rows);
    }

    #[test]
    fn streaming_output_clamps_fraction_at_history_top() {
        for initial_lines in [0, 1] {
            let mut term = streaming_terminal(2);
            term.scroll_display(initial_lines);
            let mut scroll = PixelScroll { offset: 9.0 };
            update_terminal(&mut term, &mut scroll, |term| term.advance(b"a\r\nb\r\n"));
            assert_eq!(term.display_offset(), term.scrollback_len());
            assert_eq!(term.display_offset(), 2);
            assert_eq!(scroll.offset, 0.0);
        }
        let mut term = streaming_terminal(0);
        let mut scroll = PixelScroll { offset: 9.0 };
        update_terminal(&mut term, &mut scroll, |term| term.advance(b"a\r\nb\r\n"));
        assert_eq!(term.scrollback_len(), 0);
        assert_eq!(term.display_offset(), 0);
        assert_eq!(scroll.offset, 0.0);
    }

    #[test]
    fn streaming_output_resets_fraction_on_view_jumps() {
        for bytes in [
            b"\x1b[3J".as_slice(),
            b"\x1b[3Jnew\r\n",
            b"\x1b[?1049h",
            b"\x1bc",
        ] {
            let mut term = streaming_terminal(100);
            term.scroll_display(30);
            let mut scroll = PixelScroll { offset: 9.0 };
            update_terminal(&mut term, &mut scroll, |term| term.advance(bytes));
            assert_eq!(scroll.offset, 0.0);
        }
        let mut term = streaming_terminal(100);
        term.scroll_display(30);
        let mut scroll = PixelScroll { offset: 9.0 };
        update_terminal(&mut term, &mut scroll, Terminal::scroll_to_bottom);
        assert_eq!(term.display_offset(), 0);
        assert_eq!(scroll.offset, 0.0);
    }

    #[test]
    fn pty_resize_tracks_grid_and_cell_pixels() {
        let cell = (8.0, 19.0);
        assert!(!pty_resize_needed(false, cell, cell));
        assert!(pty_resize_needed(true, cell, cell));
        assert!(pty_resize_needed(false, cell, (17.0, 37.0)));
        assert!(pty_resize_needed(false, (17.0, 37.0), cell));
        assert!(pty_resize_needed(false, cell, (9.0, 19.0)));
        assert!(pty_resize_needed(false, cell, (8.0, 20.0)));
    }

    #[test]
    fn shift_click_extends_from_original_anchor_and_keeps_shape() {
        let mut term = Terminal::new(10, 3);
        let anchor = term.viewport_to_point(1, 4);
        let head = term.viewport_to_point(0, 1);
        for kind in [
            SelectionKind::Simple,
            SelectionKind::Word,
            SelectionKind::Line,
            SelectionKind::Block,
        ] {
            term.start_selection(anchor, kind);
            term.update_selection(term.viewport_to_point(2, 8));
            select_at(&mut term, head, ModifiersState::SHIFT, 1);
            let selection = term.selection.unwrap();
            assert_eq!(selection.anchor, anchor);
            assert_eq!(selection.head, head);
            assert_eq!(selection.kind, kind);
            let next = term.viewport_to_point(2, 6);
            select_at(&mut term, next, ModifiersState::SHIFT, 1);
            assert_eq!(term.selection.unwrap().anchor, anchor);
            assert_eq!(term.selection.unwrap().head, next);
        }
    }

    #[test]
    fn selection_modifiers_work_without_an_existing_selection() {
        let mut term = Terminal::new(10, 3);
        let point = term.viewport_to_point(1, 4);
        select_at(&mut term, point, ModifiersState::SHIFT, 1);
        assert_eq!(term.selection.unwrap().kind, SelectionKind::Simple);
        // Alt/Option starts a block even if this would otherwise be a multi-click.
        select_at(&mut term, point, ModifiersState::ALT, 3);
        assert_eq!(term.selection.unwrap().kind, SelectionKind::Block);
        term.selection = None;
        select_at(
            &mut term,
            point,
            ModifiersState::SHIFT | ModifiersState::ALT,
            1,
        );
        assert_eq!(term.selection.unwrap().kind, SelectionKind::Block);
    }

    #[test]
    fn shift_multiclick_keeps_word_and_line_selection_in_mouse_apps() {
        let mut term = Terminal::new(20, 2);
        term.advance(b"hello world\x1b[?1000h");
        let point = term.viewport_to_point(0, 2);
        select_at(&mut term, point, ModifiersState::SHIFT, 1);
        assert_eq!(term.selection_text(), None);
        select_at(&mut term, point, ModifiersState::SHIFT, 2);
        assert_eq!(term.selection_text().as_deref(), Some("hello"));
        select_at(&mut term, point, ModifiersState::SHIFT, 3);
        assert_eq!(term.selection_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn clipboard_copy_on_select_is_opt_in_and_not_a_command_argument() {
        assert!(!parse_args_from(std::iter::empty()).unwrap().copy_on_select);
        let args = [
            "--copy-on-select",
            "--option-as-alt",
            "-e",
            "echo",
            "--copy-on-select",
        ];
        let options = parse_args_from(args.into_iter().map(String::from)).unwrap();
        assert!(options.copy_on_select);
        assert!(options.option_as_alt);
        assert_eq!(options.command, ["echo", "--copy-on-select"]);
    }

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
                grid_size_for(logical.to_physical(scale), scale, &fonts, PADDING),
                expected
            );
            assert_eq!(
                grid_size_for(PhysicalSize::new(0, 0), scale, &fonts, PADDING),
                (2, 1)
            );
        }
    }
    #[test]
    fn wake_deadline_merges_frame_resize_and_probe_without_idle_timers() {
        let now = Instant::now();
        assert_eq!(wake_deadline(None, None, None), None);
        for frame in [None, Some(now + Duration::from_millis(1))] {
            for resize in [None, Some(now + Duration::from_millis(2))] {
                for probe in [None, Some(now + Duration::from_millis(3))] {
                    assert_eq!(
                        wake_deadline(frame, resize, probe),
                        frame.or(resize).or(probe)
                    );
                }
            }
        }
        assert_eq!(
            wake_deadline(Some(now + MAX_DEFER), Some(now), None),
            Some(now)
        );
        assert_eq!(
            wake_deadline(Some(now + MAX_DEFER), None, Some(now)),
            Some(now)
        );
    }

    #[test]
    fn frame_wakeup_is_idle_or_latch_or_fallback_timeout() {
        let now = Instant::now();
        let latch = now + MAX_DEFER * 2;
        assert_eq!(frame_wakeup(None, None), None);
        assert_eq!(frame_wakeup(None, Some(now)), Some(now + MAX_DEFER));
        assert_eq!(frame_wakeup(Some(latch), Some(now)), Some(latch));
        assert_eq!(frame_wakeup(Some(latch), None), Some(latch));
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
