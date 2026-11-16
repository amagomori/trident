macro_rules! diag {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

mod app_icon;
#[cfg(target_os = "linux")]
mod appearance;
mod clipboard;
mod close_confirm;
mod command_notify;
mod config;
mod drop_files;
mod flow_control;
#[cfg(any(target_os = "linux", test))]
mod frame_clock;
mod hover_preview;
mod ime;
mod input;
mod latency;
mod osc_notify;
mod paste;
mod pointer;
#[cfg(target_os = "macos")]
mod present_macos;
#[cfg(target_os = "linux")]
mod present_wayland;
mod progress;
mod pty;
mod search;
#[cfg(any(target_os = "macos", test))]
mod secure_input;
mod show;
mod terminfo;
mod theme;

use config::Action;
use input::MouseButton as ReportButton;
use pty::Pty;
use std::collections::HashSet;
use std::fs::{File, FileType};
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, IntoRawFd};
use std::os::unix::fs::FileTypeExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use term_core::{ClipboardRequest, MouseMode, Point, SelectionKind, Terminal};
#[cfg(test)]
use term_render::PADDING;
use term_render::{Fonts, Renderer};
mod links;
#[cfg(target_os = "macos")]
mod native_tabs;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, PhysicalKey};
use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;
use winit::window::{Fullscreen, Theme, Window, WindowId};

const DEFAULT_FONT_SIZE: f32 = 14.0;
const STARTUP_STDERR: &str = "TERMINAL_STARTUP_STDERR";
const WORKING_DIRECTORY: &str = "TERMINAL_WORKING_DIRECTORY";
/// Bounds how long startup waits for the portal's color scheme before the
/// first PTY output is parsed; the read overlaps window and GPU setup.
#[cfg(target_os = "linux")]
const INITIAL_APPEARANCE_WAIT: Duration = Duration::from_millis(100);

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
const SYNC_TIMEOUT: Duration = Duration::from_millis(150);
/// SGR 5 blinking text shows for one half period and hides for the next.
const BLINK_HALF_PERIOD: Duration = Duration::from_millis(500);
/// Half a cursor blink period.
const CURSOR_BLINK_HALF: Duration = Duration::from_millis(530);
/// A blinking cursor rests, visible and with no timer, this long after the last input.
const CURSOR_BLINK_IDLE: Duration = Duration::from_secs(15);
const EXIT_TIMEOUT: Duration = Duration::from_millis(500);
/// Output quiet time after which the app icons are set up.
const ICONS_QUIET: Duration = Duration::from_millis(50);
/// Latest time after launch to set up the app icons, even under steady output.
const ICONS_LATEST: Duration = Duration::from_secs(1);

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
    #[cfg(target_os = "linux")]
    Appearance(u64, Option<Theme>),
    /// A long command finished in the tab with this pane id.
    CommandFinished(u64, command_notify::Finished),
    ConfigChanged,
    HoverPreview(hover_preview::Ready),
    /// The PTY of the tab with this pane id has output.
    Output(u64),
    /// The PTY of the tab with this pane id exited.
    Exit(u64),
    #[cfg(target_os = "macos")]
    Presented,
    /// The native tab bar's + button was clicked.
    #[cfg(target_os = "macos")]
    NewTab,
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
    working_directory: Option<PathBuf>,
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
        "{bin} {} - a fast GPU-rendered terminal emulator\n\n\
         USAGE: {bin} [OPTIONS] [-e COMMAND [ARGS...]]\n       \
         {bin} show [--width N] [--height N] [--fit] FILE...\n\n\
         OPTIONS:\n  \
           --config <PATH>     Override the TOML config file location\n  \
           --font <PATH>       TrueType/OpenType font file (default: bundled JetBrains Mono)\n  \
           --font-size <SIZE>  Font size in points (default: {DEFAULT_FONT_SIZE})\n  \
           --option-as-alt     On macOS, make Option act as Alt/Meta\n  \
           --scrollback <LINES> History lines (default: 10000; 0 disables; max: 1000000)\n  \
           --copy-on-select    Copy selected text to the clipboard on mouse release\n  \
           -e, --command ...   Run this command instead of your shell\n  \
           --install-terminfo  Install the xterm-trident terminfo entry with tic\n  \
           -h, --help          Show this help\n  \
           -V, --version       Show the version\n\n\
         `{bin} show` draws images and prints text files in the terminal it runs in\n\
         (see {bin} show --help).",
        env!("CARGO_PKG_VERSION"),
        bin = env!("CARGO_BIN_NAME"),
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
        working_directory: None,
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
    composition: ime::Composition,
    link_pressed: bool,
    hover: Option<term_core::Link>,
    ime_cursor_area: Option<[f32; 4]>,
    scroll: PixelScroll,
    pty_cell: (f32, f32),
    window: Arc<Window>,
    /// The window's inner size from its last `Resized` event; on X11
    /// `Window::inner_size` is a blocking server round trip.
    size: PhysicalSize<u32>,
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
    /// When the cursor last restarted blinking: input or focus.
    blink_since: Instant,
    /// The next cursor blink flip.
    cursor_blink_at: Option<Instant>,
    #[cfg(target_os = "linux")]
    presentation: Option<present_wayland::Feedback>,
    latency: Option<latency::Latency>,
    resize: ResizeThrottle,
    synchronized_since: Option<Instant>,
    redraw_pending: bool,
    #[cfg(target_os = "macos")]
    drawable_wait: Option<Instant>,
    output: PendingOutput,
    blink_epoch: Instant,
    /// When blinking text on screen next flips.
    blink_at: Option<Instant>,
    /// The title set by OSC 0/2, shown after any OSC 9;4 progress.
    title: String,
    progress: Option<term_core::Progress>,
    /// Which tab's pane the terminal fields belong to.
    pane: u64,
    /// The tab bar's height in physical pixels, 0 while it is hidden.
    tab_bar: u32,
}

/// A tab in the background: the per-terminal state `Session::swap_pane` trades with the
/// active tab's.
struct Pane {
    id: u64,
    composition: ime::Composition,
    link_pressed: bool,
    hover: Option<term_core::Link>,
    scroll: PixelScroll,
    pty_cell: (f32, f32),
    term: Terminal,
    pty: Pty,
    since_frame: usize,
    deferred: Option<Instant>,
    synchronized_since: Option<Instant>,
    output: PendingOutput,
    title: String,
    progress: Option<term_core::Progress>,
    #[cfg(target_os = "macos")]
    window: TabWindow,
}

/// A native macOS tab's own window and what draws into it, traded with the active tab's
/// by `Session::swap_pane` along with the terminal.
#[cfg(target_os = "macos")]
struct TabWindow {
    window: Arc<Window>,
    size: PhysicalSize<u32>,
    renderer: Renderer,
    fonts: Fonts,
    ime_cursor_area: Option<[f32; 4]>,
    redraw_at: Option<(Instant, Instant)>,
    target: Option<Instant>,
    cursor_blink_at: Option<Instant>,
    blink_at: Option<Instant>,
    resize: ResizeThrottle,
    redraw_pending: bool,
    drawable_wait: Option<Instant>,
    occluded: bool,
}

impl Pane {
    fn new(
        id: u64,
        term: Terminal,
        pty: Pty,
        pty_cell: (f32, f32),
        #[cfg(target_os = "macos")] window: TabWindow,
    ) -> Pane {
        Pane {
            id,
            composition: ime::Composition::default(),
            link_pressed: false,
            hover: None,
            scroll: PixelScroll::default(),
            pty_cell,
            term,
            pty,
            since_frame: 0,
            deferred: None,
            synchronized_since: None,
            output: PendingOutput::default(),
            title: String::new(),
            progress: None,
            #[cfg(target_os = "macos")]
            window,
        }
    }
}

/// Whether Trident draws its own tab bar; macOS shows native AppKit tabs instead.
fn tab_bar_drawn(shown: bool) -> bool {
    shown && !cfg!(target_os = "macos")
}

/// The title shown on a tab: the terminal's title, or a placeholder until it sets one.
fn tab_title(title: &str) -> String {
    if title.trim().is_empty() {
        "Terminal".into()
    } else {
        title.into()
    }
}

/// The tab to show after closing tab `index` of `count`: the one to its right, else
/// the one to its left.
fn neighbor_tab(index: usize, count: usize) -> usize {
    if index + 1 < count {
        index + 1
    } else {
        index.saturating_sub(1)
    }
}

/// The tab index a select-tab shortcut picks: the ninth always selects the last tab.
fn shortcut_tab(index: usize, count: usize) -> usize {
    if index == 8 {
        count.saturating_sub(1)
    } else {
        index
    }
}

/// Runs every byte of `batch` through `term`, applying shell-return resets; the bytes
/// processed.
fn advance_all(term: &mut Terminal, batch: pty::Output) -> usize {
    let mut start = 0;
    for offset in batch.shell_returns {
        while start < offset {
            start += term.advance_until_sync_end(&batch.bytes[start..offset]);
        }
        term.reset_input_modes();
    }
    while start < batch.bytes.len() {
        start += term.advance_until_sync_end(&batch.bytes[start..]);
    }
    start
}

#[cfg(target_os = "macos")]
type QuitChecks = std::rc::Rc<std::cell::RefCell<Vec<(u64, Box<dyn Fn() -> bool>)>>>;

#[derive(Default)]
struct PendingOutput {
    bytes: Vec<u8>,
    shell_returns: Vec<usize>,
    waiting_since: Option<Instant>,
}

impl PendingOutput {
    fn drain_on_exit(&mut self, term: &mut Terminal, queued: pty::Output) -> usize {
        let leftover = pty::Output {
            bytes: std::mem::take(&mut self.bytes),
            shell_returns: std::mem::take(&mut self.shell_returns),
        };
        let mut processed = 0;
        for batch in [leftover, queued] {
            processed += advance_all(term, batch);
        }
        term.modes.synchronized_output = false;
        self.waiting_since = Some(Instant::now());
        processed
    }

    fn can_advance(&self, paused: bool) -> bool {
        !paused && self.waiting_since.is_none()
    }

    fn pace_frame(&self, synchronized: bool) -> bool {
        synchronized || self.waiting_since.is_some()
    }

    fn advance(
        &mut self,
        term: &mut Terminal,
        paused: bool,
        take_pty: impl FnOnce() -> pty::Output,
    ) -> Option<(usize, bool)> {
        if !self.can_advance(paused) {
            return None;
        }
        let from_leftover = !self.is_empty();
        let mut batch = self.next_batch(take_pty);
        let processed = advance_output(term, &mut batch);
        self.retain(
            batch,
            processed,
            term.synchronized_output_ended(),
            Instant::now(),
        );
        Some((processed, from_leftover))
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty() && self.shell_returns.is_empty()
    }

    fn next_batch(&mut self, take_pty: impl FnOnce() -> pty::Output) -> pty::Output {
        if self.is_empty() {
            take_pty()
        } else {
            pty::Output {
                bytes: std::mem::take(&mut self.bytes),
                shell_returns: std::mem::take(&mut self.shell_returns),
            }
        }
    }

    fn retain(&mut self, mut batch: pty::Output, consumed: usize, frame_ended: bool, now: Instant) {
        self.bytes = batch.bytes.split_off(consumed);
        self.shell_returns = batch
            .shell_returns
            .into_iter()
            .map(|offset| offset - consumed)
            .collect();
        self.waiting_since = (frame_ended || !self.is_empty()).then_some(now);
    }

    fn coalesce(&mut self, queued: pty::Output) -> bool {
        let offset = self.bytes.len();
        self.bytes.extend(queued.bytes);
        self.shell_returns
            .extend(queued.shell_returns.into_iter().map(|at| offset + at));
        if memchr::memmem::rfind(&self.bytes, SYNC_RESET).is_none() {
            return false;
        }
        self.waiting_since = None;
        true
    }

    fn presented(&mut self) -> bool {
        self.waiting_since = None;
        !self.is_empty()
    }

    fn finish_presentation(&mut self, rendered: bool, now: Instant) -> bool {
        let waiting = self.waiting_since.is_some();
        if rendered {
            self.presented();
        } else if self.waiting_since.is_some() {
            self.waiting_since = Some(now);
        }
        rendered && waiting
    }
}

const SYNC_RESET: &[u8] = b"\x1b[?2026l";

/// Applies output up to the last synchronized frame already complete in `output`, so a
/// backlog of finished frames is drawn once instead of one presentation per frame.
fn advance_output(term: &mut Terminal, output: &mut pty::Output) -> usize {
    let bytes = &output.bytes;
    let mut last_reset = None;
    let mut start = 0;
    let mut applied = 0;
    let mut ended = false;
    for &offset in &output.shell_returns {
        start += advance_frames(term, bytes, start, offset, &mut last_reset);
        if start == offset {
            term.reset_input_modes();
            applied += 1;
        }
        ended = term.synchronized_output_ended() && start >= last_reset_in(bytes, &mut last_reset);
        if ended {
            break;
        }
    }
    output.shell_returns.drain(..applied);
    if !ended {
        start += advance_frames(
            term,
            &output.bytes,
            start,
            output.bytes.len(),
            &mut last_reset,
        );
    }
    start
}

/// Advances `bytes[start..end]` frame by frame until the frame end at or past the last
/// one in `bytes`, returning the bytes consumed.
fn advance_frames(
    term: &mut Terminal,
    bytes: &[u8],
    start: usize,
    end: usize,
    last_reset: &mut Option<usize>,
) -> usize {
    let mut consumed = start;
    loop {
        consumed += term.advance_until_sync_end(&bytes[consumed..end]);
        if !term.synchronized_output_ended()
            || consumed == end
            || consumed >= last_reset_in(bytes, last_reset)
        {
            return consumed - start;
        }
    }
}

/// Scrolls to the bottom and clears the selection before input reaches the PTY, returning
/// whether that changed what is on screen. Otherwise the echo, not the keypress, changes the
/// screen, and drawing now would block the event loop for a frame just as the echo arrives.
fn reset_view_for_input(term: &mut Terminal, scroll: &mut PixelScroll) -> bool {
    let changed = term.display_offset() != 0 || scroll.offset != 0.0 || term.selection.is_some();
    term.scroll_to_bottom();
    scroll.reset();
    term.selection = None;
    changed
}

/// The end of the last frame reset in `bytes`, searched only once a frame has ended.
fn last_reset_in(bytes: &[u8], last_reset: &mut Option<usize>) -> usize {
    *last_reset.get_or_insert_with(|| {
        memchr::memmem::rfind(bytes, SYNC_RESET).map_or(0, |start| start + SYNC_RESET.len())
    })
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
    /// Makes `pane` the active tab, leaving the previous active tab's state in `pane`.
    fn swap_pane(&mut self, pane: &mut Pane) {
        use std::mem::swap;
        swap(&mut self.pane, &mut pane.id);
        swap(&mut self.composition, &mut pane.composition);
        swap(&mut self.link_pressed, &mut pane.link_pressed);
        swap(&mut self.hover, &mut pane.hover);
        swap(&mut self.scroll, &mut pane.scroll);
        swap(&mut self.pty_cell, &mut pane.pty_cell);
        swap(&mut self.term, &mut pane.term);
        swap(&mut self.pty, &mut pane.pty);
        swap(&mut self.since_frame, &mut pane.since_frame);
        swap(&mut self.deferred, &mut pane.deferred);
        swap(&mut self.synchronized_since, &mut pane.synchronized_since);
        swap(&mut self.output, &mut pane.output);
        swap(&mut self.title, &mut pane.title);
        swap(&mut self.progress, &mut pane.progress);
        #[cfg(target_os = "macos")]
        {
            let tab = &mut pane.window;
            swap(&mut self.window, &mut tab.window);
            swap(&mut self.size, &mut tab.size);
            swap(&mut self.renderer, &mut tab.renderer);
            swap(&mut self.fonts, &mut tab.fonts);
            swap(&mut self.ime_cursor_area, &mut tab.ime_cursor_area);
            swap(&mut self.redraw_at, &mut tab.redraw_at);
            swap(&mut self.target, &mut tab.target);
            swap(&mut self.cursor_blink_at, &mut tab.cursor_blink_at);
            swap(&mut self.blink_at, &mut tab.blink_at);
            swap(&mut self.resize, &mut tab.resize);
            swap(&mut self.redraw_pending, &mut tab.redraw_pending);
            swap(&mut self.drawable_wait, &mut tab.drawable_wait);
        }
    }

    /// The part of the window below the tab bar, where the grid goes.
    fn grid_px(&self) -> PhysicalSize<u32> {
        below_tab_bar(self.size, self.tab_bar)
    }

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

/// Whether a cursor blinking since `since` shows at `now`, and when that next changes.
fn cursor_blink(blinking: bool, since: Instant, now: Instant) -> (bool, Option<Instant>) {
    let elapsed = now.saturating_duration_since(since);
    if !blinking || elapsed >= CURSOR_BLINK_IDLE {
        return (true, None);
    }
    let phase = elapsed.as_nanos() / CURSOR_BLINK_HALF.as_nanos();
    let next = since + CURSOR_BLINK_HALF * (phase as u32 + 1);
    (
        phase.is_multiple_of(2),
        Some(next.min(since + CURSOR_BLINK_IDLE)),
    )
}

/// When to set up the app icons: once output has been quiet for `ICONS_QUIET` after the
/// first frame, and by `latest` in any case.
fn icons_due(first_frame_at: Option<Instant>, output_at: Instant, latest: Instant) -> Instant {
    match first_frame_at {
        Some(drawn) => latest.min(drawn.max(output_at) + ICONS_QUIET),
        None => latest,
    }
}

fn wake_deadline(
    frame: Option<Instant>,
    resize: Option<Instant>,
    probe: Option<Instant>,
) -> Option<Instant> {
    [frame, resize, probe].into_iter().flatten().min()
}

/// The alternate screen has no scrollback, so these scroll actions would do
/// nothing there; let the key reach the program instead (vim, emacs, less).
fn passes_through_on_alt_screen(action: &Action) -> bool {
    matches!(
        action,
        Action::LineUp | Action::LineDown | Action::Top | Action::Bottom
    )
}

fn shell_shortcuts_available(term: &Terminal) -> bool {
    term.has_shell_marks() && !term.is_alt_screen()
}

#[derive(Default)]
struct HeldAtFocus(HashSet<PhysicalKey>);

impl HeldAtFocus {
    fn observe(&mut self, key: PhysicalKey, state: ElementState, synthetic: bool) -> bool {
        match state {
            ElementState::Released => {
                self.0.remove(&key);
            }
            ElementState::Pressed if synthetic => {
                self.0.insert(key);
            }
            ElementState::Pressed => {}
        }
        self.0.contains(&key)
    }
}

fn suppress_shortcut_repeat(action: &Action, repeat: bool, held_at_focus: bool) -> bool {
    (repeat || held_at_focus)
        && matches!(
            action,
            Action::NewWindow | Action::NewTab | Action::CloseTab | Action::ToggleFullscreen
        )
}

/// The fullscreen state `toggle_fullscreen` asks for: borderless on the current monitor,
/// or windowed again.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn toggled_fullscreen(current: Option<Fullscreen>) -> Option<Fullscreen> {
    match current {
        Some(_) => None,
        None => Some(Fullscreen::Borderless(None)),
    }
}

/// The directory a new window should start in: the shell's OSC 7 directory when it is
/// on this machine and still exists.
fn local_working_directory(cwd: Option<&term_core::WorkingDirectory>) -> Option<PathBuf> {
    let cwd = cwd?;
    let local = cwd.host.is_empty()
        || cwd.host.eq_ignore_ascii_case("localhost")
        || hostname().is_some_and(|name| same_host(&name, &cwd.host));
    let path = PathBuf::from(&cwd.path);
    (local && path.is_absolute() && path.is_dir()).then_some(path)
}

fn same_host(a: &str, b: &str) -> bool {
    let short = |name: &str| {
        name.split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    a.eq_ignore_ascii_case(b) || (!a.contains('.') || !b.contains('.')) && short(a) == short(b)
}

fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname writes at most that many bytes.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..len].to_vec()).ok()
}

fn inherit_new_window_stderr(kind: Option<FileType>) -> bool {
    kind.is_some_and(|kind| kind.is_file() || kind.is_char_device())
}

fn pipe_new_window_stderr(kind: Option<FileType>) -> bool {
    kind.is_some_and(|kind| kind.is_fifo() || kind.is_socket())
}

fn new_window_stderr() -> (Stdio, bool) {
    let kind = std::io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .and_then(|fd| File::from(fd).metadata())
        .map(|metadata| metadata.file_type())
        .ok();
    if inherit_new_window_stderr(kind) {
        (Stdio::inherit(), false)
    } else if pipe_new_window_stderr(kind) {
        (Stdio::piped(), true)
    } else {
        (Stdio::null(), false)
    }
}

fn forward_new_window_stderr(mut reader: impl Read, mut writer: impl Write) -> std::io::Result<()> {
    let mut chunk = [0; 4096];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(len) => {
                let _ = writer.write_all(&chunk[..len]);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

fn finish_startup_stderr() -> std::io::Result<()> {
    let null = File::options().write(true).open("/dev/null")?;
    if null.as_raw_fd() == libc::STDERR_FILENO {
        let _ = null.into_raw_fd();
        return Ok(());
    }
    // SAFETY: both descriptors are valid; dup2 atomically replaces stderr.
    if unsafe { libc::dup2(null.as_raw_fd(), libc::STDERR_FILENO) } == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
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

/// Whether blinking text shows at `now`, and when that next flips.
fn blink_phase(epoch: Instant, now: Instant) -> (bool, Instant) {
    let half = BLINK_HALF_PERIOD.as_nanos();
    let elapsed = now.saturating_duration_since(epoch).as_nanos();
    let flips = elapsed / half;
    let next = epoch + Duration::from_nanos(((flips + 1) * half) as u64);
    (flips.is_multiple_of(2), next)
}

fn sync_expired(since: Option<Instant>, now: Instant) -> bool {
    since.is_some_and(|since| now.duration_since(since) >= SYNC_TIMEOUT)
}

fn exit_ready(deadline: Option<Instant>, presented: bool, now: Instant) -> bool {
    deadline.is_some_and(|deadline| presented || now >= deadline)
}

fn exit_wakeup(deadline: Option<Instant>, exit: Option<Instant>) -> Option<Instant> {
    [deadline, exit].into_iter().flatten().min()
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
#[derive(Default)]
struct ExitPresentation {
    target: Option<usize>,
    first_target: Option<usize>,
    ready_at: Option<Instant>,
    submitted_at: Option<Instant>,
    submissions: usize,
    /// A second final drawable has been requested after an untimed Metal callback or proactively.
    #[cfg(any(target_os = "macos", test))]
    untimed_retried: bool,
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
impl ExitPresentation {
    fn needs_submission(&self) -> bool {
        self.target.is_none()
    }

    #[cfg(any(target_os = "linux", test))]
    fn needs_wayland_submission(&self) -> bool {
        self.needs_submission() || self.submissions == 1
    }

    #[cfg(any(target_os = "macos", test))]
    fn needs_metal_submission(&self, presented: usize) -> bool {
        self.needs_submission() || (presented == 0 && self.submissions == 1)
    }

    fn submitted(&mut self, target: usize, now: Instant) {
        self.target = Some(target);
        self.first_target.get_or_insert(target);
        self.submitted_at = Some(now);
        self.submissions += 1;
        #[cfg(any(target_os = "macos", test))]
        if self.submissions > 1 {
            self.untimed_retried = true;
        }
    }

    fn retry(&mut self) {
        self.target = None;
        self.submitted_at = None;
        self.ready_at = None;
    }

    fn missing_feedback(&mut self, now: Instant, refresh: u32) -> bool {
        let Some(submitted) = self.submitted_at else {
            return false;
        };
        let period = Duration::from_secs_f64(
            1000.0 / f64::from(if refresh == 0 { 60_000 } else { refresh }),
        );
        if self.ready_at.is_some() || now < submitted + period {
            return false;
        }
        // Feedback can be absent even for visible frames. Retry once, then allow one
        // refresh for the retry instead of waiting for a callback that may never arrive.
        if self.submissions == 1 {
            #[cfg(any(target_os = "macos", test))]
            {
                self.untimed_retried = true;
            }
            self.retry();
            return true;
        }
        self.finish(now);
        false
    }

    #[cfg(any(target_os = "macos", test))]
    fn update_metal(
        &mut self,
        presented: usize,
        skipped: [usize; 2],
        now: Instant,
        refresh: u32,
    ) -> bool {
        // Without any nonzero presentedTime, a zero cannot tell a shown drawable from a
        // discarded one: submit the final frame twice, not just one skipped drawable.
        if presented == 0 && self.target.is_some_and(|target| skipped[1] >= target) {
            if !self.untimed_retried {
                self.untimed_retried = true;
                self.retry();
                return true;
            }
            if self
                .target
                .is_some_and(|target| skipped[0] >= target.saturating_sub(1))
            {
                self.finish(now);
                return false;
            }
            return self.missing_feedback(now, refresh);
        }
        self.update(presented, skipped[1], now, refresh)
    }

    fn finish(&mut self, now: Instant) {
        self.ready_at.get_or_insert(now);
    }

    fn update(&mut self, presented: usize, skipped: usize, now: Instant, refresh: u32) -> bool {
        if self
            .first_target
            .or(self.target)
            .is_some_and(|target| presented >= target)
        {
            self.finish(now);
            return false;
        }
        if let Some(target) = self.target
            && skipped >= target
        {
            self.retry();
            return true;
        }
        self.missing_feedback(now, refresh)
    }

    fn ready(&self, now: Instant) -> bool {
        self.ready_at.is_some_and(|until| now >= until)
    }

    fn wakeup(&self, now: Instant) -> Instant {
        self.ready_at.unwrap_or(now + Duration::from_millis(4))
    }
}

fn sync_wakeup(
    deadline: Option<Instant>,
    waiting_since: Option<Instant>,
    synchronized_since: Option<Instant>,
) -> Option<Instant> {
    [
        deadline,
        waiting_since.map(|since| since + MAX_DEFER),
        synchronized_since.map(|since| since + SYNC_TIMEOUT),
    ]
    .into_iter()
    .flatten()
    .min()
}

struct App {
    startup_stderr: bool,
    search: Option<search::Search>,
    appearance: Option<Theme>,
    #[cfg(target_os = "linux")]
    appearance_watch: Option<appearance::Watch>,
    #[cfg(target_os = "linux")]
    appearance_generation: u64,
    config: config::Config,
    config_error: Option<String>,
    icons: app_icon::Shown,
    /// Until the startup icons are set up: the latest time to do it.
    icons_latest: Option<Instant>,
    /// When PTY output was last parsed.
    output_at: Instant,
    /// When the first frame was seen drawn, while the startup icons wait.
    first_frame_at: Option<Instant>,
    _watcher: Option<config::ConfigWatcher>,
    options: Options,
    proxy: EventLoopProxy<UserEvent>,
    session: Option<Session>,
    /// One slot per tab, in tab bar order; the active tab's slot is empty because its
    /// state lives in `session`.
    tabs: Vec<Option<Pane>>,
    active_tab: usize,
    next_pane: u64,
    tab_hover: Option<term_render::TabHit>,
    /// The pointer in window coordinates; `mouse_pos` is relative to the grid area.
    tab_pointer: PhysicalPosition<f64>,
    /// A press landed on the tab bar, so its release is not the terminal's.
    tab_click: bool,
    /// The tab a close confirmation is for; none when it is for the window.
    pending_close_tab: Option<u64>,
    /// The tabs, by pane id, of the native window a close confirmation is for while other
    /// windows hold the rest.
    #[cfg(target_os = "macos")]
    pending_close_window: Option<Vec<u64>>,
    /// Per tab pane id, whether a program other than the shell runs there; asked before
    /// the app quits.
    #[cfg(target_os = "macos")]
    quit_checks: QuitChecks,
    font_size: f32,
    modifiers: ModifiersState,
    held_at_focus: HeldAtFocus,
    focused: bool,
    #[cfg(target_os = "macos")]
    secure_input: secure_input::SecureInput,
    /// The window is fully hidden (winit `Occluded`), for OSC 99 `o=invisible`.
    occluded: bool,
    notification_throttle: osc_notify::Throttle,
    mouse_pos: PhysicalPosition<f64>,
    typing_pointer: pointer::TypingPointer,
    hover_preview: hover_preview::HoverPreview,
    left_down: bool,
    /// The left press landed on the pinned command header; swallow its release.
    ledger_click: bool,
    /// Button held while a program tracks the mouse, for drag reports.
    reported_button: Option<ReportButton>,
    last_click: Option<(Instant, Point, u8)>,
    scroll_accum: f64,
    clipboard: clipboard::Clipboard,
    /// Clipboard text waiting for the user to confirm (paste protection).
    pending_paste: Option<String>,
    /// Set while a window close waits for confirmation, with the running program's name.
    pending_close: Option<Option<String>>,
    error: Option<String>,
    exit_deadline: Option<Instant>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    exit_presentation: ExitPresentation,
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
            Ok(config) => {
                for warning in &config.warnings {
                    diag!("{warning}");
                }
                (config, None)
            }
            Err(error) => {
                diag!("{error}");
                (config::Config::default(), Some(error))
            }
        };
        let path = std::path::absolute(&options.config_path).unwrap_or(options.config_path.clone());
        let wake = proxy.clone();
        let watcher = config::watch(path, move || {
            let _ = wake.send_event(UserEvent::ConfigChanged);
        })
        .map_err(|e| diag!("trident: config reload unavailable: {e}"))
        .ok();
        App {
            startup_stderr: false,
            search: None,
            appearance: None,
            #[cfg(target_os = "linux")]
            appearance_watch: None,
            #[cfg(target_os = "linux")]
            appearance_generation: 0,
            font_size: options.size(&config),
            config,
            config_error,
            icons: app_icon::Shown::default(),
            icons_latest: None,
            output_at: Instant::now(),
            first_frame_at: None,
            _watcher: watcher,
            options,
            proxy,
            session: None,
            tabs: Vec::new(),
            active_tab: 0,
            next_pane: 0,
            tab_hover: None,
            tab_pointer: PhysicalPosition::default(),
            tab_click: false,
            pending_close_tab: None,
            #[cfg(target_os = "macos")]
            pending_close_window: None,
            #[cfg(target_os = "macos")]
            quit_checks: QuitChecks::default(),
            modifiers: ModifiersState::empty(),
            held_at_focus: HeldAtFocus::default(),
            focused: true,
            #[cfg(target_os = "macos")]
            secure_input: Default::default(),
            occluded: false,
            notification_throttle: osc_notify::Throttle::default(),
            mouse_pos: PhysicalPosition::new(0.0, 0.0),
            typing_pointer: pointer::TypingPointer::default(),
            hover_preview: hover_preview::HoverPreview::new(),
            left_down: false,
            ledger_click: false,
            reported_button: None,
            last_click: None,
            scroll_accum: 0.0,
            clipboard: clipboard::Clipboard::default(),
            pending_paste: None,
            pending_close: None,
            error: None,
            exit_deadline: None,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            exit_presentation: ExitPresentation::default(),
        }
    }

    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<Session, String> {
        let probe = match Fonts::new(self.options.font(&self.config), self.font_size) {
            Ok(fonts) => fonts,
            Err(error) if self.options.font.is_none() => {
                let error = format!(
                    "trident: {}:1:1: font: {error}",
                    self.options.config_path.display()
                );
                diag!("{error}");
                self.config_error = Some(error);
                self.config = config::Config::default();
                self.font_size = self.options.size(&self.config);
                Fonts::new(None, self.font_size)?
            }
            Err(error) => return Err(error),
        };
        self.watch_appearance();
        let font = self.options.font(&self.config);
        let m = probe.metrics;
        let logical_size = LogicalSize::new(
            100.0 * m.width + 2.0 * self.config.padding,
            30.0 * m.height + 2.0 * self.config.padding + self.tab_bar_height(1, &probe) as f32,
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
            below_tab_bar(
                logical_size.to_physical(guess),
                self.tab_bar_height(1, &guessed_fonts),
            ),
            guess,
            &guessed_fonts,
            self.config.padding,
        );
        let spawn_cell = (guessed_fonts.metrics.width, guessed_fonts.metrics.height);
        let pty = self.spawn_pty(
            0,
            self.options.working_directory.as_deref(),
            spawn_cols,
            spawn_rows,
            spawn_cell,
        )?;
        let attrs = Window::default_attributes()
            .with_title("Trident")
            .with_inner_size(logical_size);
        #[cfg(target_os = "linux")]
        let attrs = winit::platform::wayland::WindowAttributesExtWayland::with_name(
            attrs, "trident", "trident",
        );
        #[cfg(target_os = "macos")]
        let attrs = winit::platform::macos::WindowAttributesExtMacOS::with_tabbing_identifier(
            attrs,
            native_tabs::TABBING_IDENTIFIER,
        );
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
        let renderer = Renderer::new(window.clone(), size.width, size.height, self.config.vsync)?;
        diag!("trident: rendering with {}", renderer.adapter_info);
        warn_vsync_unsupported(&self.config, &renderer);

        let tab_bar = self.tab_bar_height(1, &fonts);
        let (cols, rows) = grid_size(&window, &fonts, self.config.padding, tab_bar);
        let mut term = Terminal::new(cols, rows);
        self.appearance = window.theme().or(self.appearance);
        #[cfg(target_os = "linux")]
        if let Some(theme) = self
            .appearance_watch
            .as_mut()
            .and_then(|watch| watch.initial(INITIAL_APPEARANCE_WAIT))
        {
            self.appearance = theme;
        }
        term.set_palette(self.config.themes.palette(self.appearance).clone());
        configure_term(
            &mut term,
            &self.config,
            self.options.scrollback(&self.config),
        );
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
        }
        #[cfg(target_os = "macos")]
        {
            let (start, lines) = latency
                .as_ref()
                .map(|log| {
                    let (start, lines) = log.metal_log();
                    (start, Some(lines))
                })
                .unwrap_or_else(|| (Instant::now(), None));
            if let Err(error) =
                present_macos::install(renderer.gpu_device(), start, lines, self.proxy.clone())
            {
                diag!("trident: presentation feedback unavailable: {error}");
            }
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
                        diag!("trident: presentation feedback unavailable: {error}");
                        None
                    }
                }
            } else {
                None
            }
        };
        Ok(Session {
            composition: ime::Composition::default(),
            link_pressed: false,
            hover: None,
            ime_cursor_area: None,
            scroll: PixelScroll::default(),
            pty_cell: cell,
            size,
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
            blink_since: Instant::now(),
            cursor_blink_at: None,
            #[cfg(target_os = "linux")]
            presentation,
            synchronized_since: None,
            redraw_pending: false,
            #[cfg(target_os = "macos")]
            drawable_wait: None,
            output: PendingOutput::default(),
            blink_epoch: Instant::now(),
            blink_at: None,
            title: String::new(),
            progress: None,
            pane: 0,
            tab_bar,
        })
    }

    /// Starts the shell for the tab with pane id `id`, its events tagged with that id.
    fn spawn_pty(
        &self,
        id: u64,
        cwd: Option<&std::path::Path>,
        cols: usize,
        rows: usize,
        cell: (f32, f32),
    ) -> Result<Pty, String> {
        let proxy = self.proxy.clone();
        let command_proxy = self.proxy.clone();
        let mut env = self.config.env.clone();
        if !env.contains_key("TERM") {
            let term = terminfo::term(&self.config.term, &self.config.env);
            env.insert("TERM".into(), term.into());
        }
        let mut pty = Pty::spawn(
            self.options.command(&self.config),
            &env,
            cwd,
            cols,
            rows,
            cell,
            move |exited| {
                let _ = proxy.send_event(if exited {
                    UserEvent::Exit(id)
                } else {
                    UserEvent::Output(id)
                });
            },
            move |finished| {
                let _ = command_proxy.send_event(UserEvent::CommandFinished(id, finished));
            },
        )?;
        pty.set_command_notifications(self.config.notify_long_commands_after != 0)?;
        Ok(pty)
    }

    /// The tab bar's height with `tabs` tabs open, 0 when it is hidden.
    fn tab_bar_height(&self, tabs: usize, fonts: &Fonts) -> u32 {
        if tab_bar_drawn(self.config.tab_bar.shown(tabs)) {
            term_render::tab_bar_height(fonts.metrics.height)
        } else {
            0
        }
    }

    /// Where tab `index`'s PTY is, whether it is the active tab or in the background.
    fn tab_pty(&self, index: usize) -> Option<&Pty> {
        if index == self.active_tab {
            self.session.as_ref().map(|s| &s.pty)
        } else {
            self.tabs.get(index)?.as_ref().map(|pane| &pane.pty)
        }
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        if self.session.as_ref().is_some_and(|s| s.pane == id) {
            return Some(self.active_tab);
        }
        self.tabs
            .iter()
            .position(|pane| pane.as_ref().is_some_and(|pane| pane.id == id))
    }

    /// Opens a tab after the active one, in the active tab's OSC 7 directory, and shows it.
    fn new_tab(&mut self, event_loop: &ActiveEventLoop) {
        #[cfg(target_os = "macos")]
        let window = match self.open_tab_window(event_loop) {
            Ok(window) => window,
            Err(error) => {
                diag!("trident: new tab: {error}");
                return;
            }
        };
        #[cfg(not(target_os = "macos"))]
        let _ = event_loop;
        let Some(s) = &self.session else { return };
        let cwd = local_working_directory(s.term.working_directory())
            .or_else(|| self.options.working_directory.clone());
        let (cols, rows) = (s.term.cols(), s.term.rows());
        let cell = (s.fonts.metrics.width, s.fonts.metrics.height);
        self.next_pane += 1;
        let id = self.next_pane;
        let pty = match self.spawn_pty(id, cwd.as_deref(), cols, rows, cell) {
            Ok(pty) => pty,
            Err(error) => {
                diag!("trident: new tab: {error}");
                return;
            }
        };
        let mut term = Terminal::new(cols, rows);
        term.set_palette(self.config.themes.palette(self.appearance).clone());
        configure_term(
            &mut term,
            &self.config,
            self.options.scrollback(&self.config),
        );
        term.set_cell_pixel_size(cell.0, cell.1);
        #[cfg(target_os = "macos")]
        self.quit_checks
            .borrow_mut()
            .push((id, Box::new(pty.running_program_check())));
        let index = (self.active_tab + 1).min(self.tabs.len());
        self.tabs.insert(
            index,
            Some(Pane::new(
                id,
                term,
                pty,
                cell,
                #[cfg(target_os = "macos")]
                window,
            )),
        );
        if index <= self.active_tab {
            self.active_tab += 1;
        }
        self.select_tab(index);
    }

    /// Creates the window for a new native tab and adds it to the active window's tab group.
    #[cfg(target_os = "macos")]
    fn open_tab_window(&self, event_loop: &ActiveEventLoop) -> Result<TabWindow, String> {
        use winit::platform::macos::WindowAttributesExtMacOS;
        let s = self.session.as_ref().ok_or("no window")?;
        let attrs = Window::default_attributes()
            .with_title("Trident")
            .with_inner_size(s.size)
            .with_visible(false)
            .with_tabbing_identifier(native_tabs::TABBING_IDENTIFIER);
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| format!("cannot create window: {e}"))?,
        );
        window.set_ime_allowed(true);
        native_tabs::add_tab(&s.window, &window);
        let size = window.inner_size();
        let fonts = Fonts::new(
            self.options.font(&self.config),
            self.font_size * window.scale_factor() as f32,
        )?;
        let renderer = Renderer::new(window.clone(), size.width, size.height, self.config.vsync)?;
        Ok(TabWindow {
            window,
            size,
            renderer,
            fonts,
            ime_cursor_area: None,
            redraw_at: None,
            target: None,
            cursor_blink_at: None,
            blink_at: None,
            resize: ResizeThrottle::default(),
            redraw_pending: false,
            drawable_wait: None,
            occluded: false,
        })
    }

    /// The window tab `index` draws into.
    #[cfg(target_os = "macos")]
    fn tab_window(&self, index: usize) -> Option<&Window> {
        if index == self.active_tab {
            self.session.as_ref().map(|s| &*s.window)
        } else {
            self.tabs
                .get(index)?
                .as_ref()
                .map(|pane| &*pane.window.window)
        }
    }

    /// The tabs of the active window's native tab group, in tab bar order.
    #[cfg(target_os = "macos")]
    fn native_tab_order(&self) -> Vec<usize> {
        let Some(s) = &self.session else {
            return Vec::new();
        };
        let keys: Vec<Option<usize>> = (0..self.tabs.len())
            .map(|index| self.tab_window(index).and_then(native_tabs::key))
            .collect();
        native_tabs::tab_order(&s.window)
            .into_iter()
            .filter_map(|key| keys.iter().position(|&k| k == Some(key)))
            .collect()
    }

    #[cfg(target_os = "macos")]
    fn step_native_tab(&mut self, step: isize) {
        if let Some(tab) = native_tabs::step_in(&self.native_tab_order(), self.active_tab, step) {
            self.select_tab(tab);
        }
    }

    /// Runs `f` with background tab `index` standing in as the active one, as when its
    /// window, torn out of the tab group, is drawn or resized on its own.
    #[cfg(target_os = "macos")]
    fn with_tab_shown(&mut self, index: usize, f: impl FnOnce(&mut App)) {
        let Some(mut pane) = self.tabs.get_mut(index).and_then(Option::take) else {
            return;
        };
        let Some(s) = &mut self.session else {
            self.tabs[index] = Some(pane);
            return;
        };
        s.swap_pane(&mut pane);
        std::mem::swap(&mut self.occluded, &mut pane.window.occluded);
        let latency = s.latency.take();
        let search = self.search.take();
        let focused = std::mem::replace(&mut self.focused, false);
        let pending_paste = self.pending_paste.take();
        let pending_close = self.pending_close.take();
        let tab_hover = self.tab_hover.take();
        f(self);
        self.tab_hover = tab_hover;
        self.pending_close = pending_close;
        self.pending_paste = pending_paste;
        self.focused = focused;
        self.search = search;
        std::mem::swap(&mut self.occluded, &mut pane.window.occluded);
        if let Some(s) = &mut self.session {
            s.latency = latency;
            s.swap_pane(&mut pane);
        }
        self.tabs[index] = Some(pane);
    }

    /// Events for a background tab's window: AppKit selecting or closing its tab, or the
    /// window resizing or redrawing while torn out of the tab group.
    #[cfg(target_os = "macos")]
    fn background_window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        event: WindowEvent,
    ) {
        let Some(index) = self.tabs.iter().position(|pane| {
            pane.as_ref()
                .is_some_and(|pane| pane.window.window.id() == id)
        }) else {
            return;
        };
        match event {
            WindowEvent::Focused(true) => {
                self.cancel_prompts();
                self.select_tab(index);
                self.window_event(event_loop, id, WindowEvent::Focused(true));
            }
            WindowEvent::CloseRequested => match close_confirm::take_wide_close() {
                Some(close_confirm::WideClose::Quit) => self.close_requested(event_loop),
                Some(close_confirm::WideClose::Window) => self.close_window(event_loop),
                None => self.close_tab(index, event_loop),
            },
            WindowEvent::ThemeChanged(theme) => self.set_appearance(Some(theme)),
            WindowEvent::Occluded(occluded) => {
                if let Some(pane) = &mut self.tabs[index] {
                    pane.window.occluded = occluded;
                    if !occluded {
                        pane.window.window.request_redraw();
                    }
                }
            }
            WindowEvent::Resized(size) => self.with_tab_shown(index, |app| {
                if let Some(s) = &mut app.session {
                    s.size = size;
                    s.renderer.resize(size.width, size.height);
                }
                app.resize_grid();
            }),
            WindowEvent::ScaleFactorChanged { .. } => {
                self.with_tab_shown(index, |app| app.set_font_size(app.font_size))
            }
            WindowEvent::RedrawRequested => self.with_tab_shown(index, |app| {
                if !app.occluded
                    && app
                        .session
                        .as_ref()
                        .is_some_and(|s| s.window.is_visible() != Some(false))
                {
                    app.present_frame();
                }
            }),
            _ => {}
        }
    }

    /// Shows tab `index`; the tab it replaces keeps running in the background.
    fn select_tab(&mut self, index: usize) {
        if index == self.active_tab || index >= self.tabs.len() {
            return;
        }
        let Some(s) = &mut self.session else { return };
        let Some(mut pane) = self.tabs[index].take() else {
            return;
        };
        if self.focused && s.term.modes.focus_events {
            s.pty.write(b"\x1b[O");
        }
        s.swap_pane(&mut pane);
        #[cfg(not(target_os = "macos"))]
        s.renderer.forget_frame();
        #[cfg(target_os = "macos")]
        {
            std::mem::swap(&mut self.occluded, &mut pane.window.occluded);
            native_tabs::show(&s.window);
        }
        self.tabs[self.active_tab] = Some(pane);
        self.active_tab = index;
        if self.focused && s.term.modes.focus_events {
            s.pty.write(b"\x1b[I");
        }
        self.search = None;
        self.left_down = false;
        self.ledger_click = false;
        self.last_click = None;
        self.reported_button = None;
        s.ime_cursor_area = None;
        s.window
            .set_title(&progress::window_title(&s.title, s.progress));
        s.request_frame();
        s.window.request_redraw();
        self.resize_grid();
        self.refresh_link();
        self.process_output();
    }

    /// Closes tab `index`, asking first while a program other than the shell runs in it.
    /// Closing the last tab closes the window.
    fn close_tab(&mut self, index: usize, event_loop: &ActiveEventLoop) {
        if self.tabs.len() <= 1 {
            self.close_requested(event_loop);
            return;
        }
        let id = if index == self.active_tab {
            self.session.as_ref().map(|s| s.pane)
        } else {
            self.tabs
                .get(index)
                .and_then(Option::as_ref)
                .map(|pane| pane.id)
        };
        // While a prompt is up only the tab it asks about closes, a second close confirming it.
        if self.pending_close.is_some() || self.pending_paste.is_some() {
            if id.is_none() || id != self.pending_close_tab {
                return;
            }
            self.pending_close = None;
            self.pending_close_tab = None;
            self.remove_tab(index);
            return;
        }
        let running = self
            .tab_pty(index)
            .filter(|_| self.config.confirm_close)
            .and_then(Pty::running_program);
        if let Some(pid) = running {
            self.select_tab(index);
            self.pending_close = Some(close_confirm::process_name(pid));
            self.pending_close_tab = self.session.as_ref().map(|s| s.pane);
            if let Some(s) = &self.session {
                s.window.request_redraw();
            }
            return;
        }
        self.remove_tab(index);
    }

    /// Drops a pending paste or close prompt as Escape would, since AppKit moved to another
    /// tab and the prompt belongs to the one it left.
    #[cfg(target_os = "macos")]
    fn cancel_prompts(&mut self) {
        self.pending_paste = None;
        if self.pending_close_window.take().is_some() {
            self.pending_close = None;
            return;
        }
        if self.pending_close.take().is_some() && self.pending_close_tab.take().is_none() {
            close_confirm::answer_quit(false);
        }
    }

    /// Closes the tabs of the active tab's native window, asking first while a program other
    /// than the shell runs in one, and a second Close Window confirming; the app as a whole
    /// when that window holds every tab.
    #[cfg(target_os = "macos")]
    #[cold]
    fn close_window(&mut self, event_loop: &ActiveEventLoop) {
        let group = self.native_tab_order();
        if group.is_empty() || group.len() >= self.tabs.len() {
            self.close_requested(event_loop);
            return;
        }
        let ids: Vec<u64> = group
            .iter()
            .filter_map(|&index| {
                if index == self.active_tab {
                    self.session.as_ref().map(|s| s.pane)
                } else {
                    self.tabs.get(index)?.as_ref().map(|pane| pane.id)
                }
            })
            .collect();
        if self.pending_close_window.as_ref() == Some(&ids) {
            self.pending_close_window = None;
            self.pending_close = None;
            self.remove_tabs(&ids);
            return;
        }
        // A paste or quit prompt keeps the window; a tab prompt gives way to this one.
        let app_prompt = self.pending_close.is_some()
            && self.pending_close_tab.is_none()
            && self.pending_close_window.is_none();
        if self.pending_paste.is_some() || app_prompt {
            return;
        }
        let running = group
            .iter()
            .filter(|_| self.config.confirm_close)
            .find_map(|&index| self.tab_pty(index).and_then(Pty::running_program));
        if let Some(pid) = running {
            self.pending_close_tab = None;
            self.pending_close = Some(close_confirm::process_name(pid));
            self.pending_close_window = Some(ids);
            if let Some(s) = &self.session {
                s.window.request_redraw();
            }
            return;
        }
        self.pending_close_tab = None;
        self.pending_close = None;
        self.remove_tabs(&ids);
    }

    /// Drops the tabs with pane `ids`, the active one last so the tab shown next is one
    /// that stays.
    #[cfg(target_os = "macos")]
    fn remove_tabs(&mut self, ids: &[u64]) {
        let active = self.session.as_ref().map(|s| s.pane);
        let (shown, hidden): (Vec<u64>, Vec<u64>) = ids.iter().partition(|&&id| Some(id) == active);
        for id in hidden.into_iter().chain(shown) {
            if let Some(index) = self.tab_index(id) {
                self.remove_tab(index);
            }
        }
        if let Some(s) = &self.session {
            s.window.request_redraw();
        }
    }

    /// Drops tab `index` and its PTY; the others keep running.
    fn remove_tab(&mut self, index: usize) {
        if self.tabs.len() <= 1 || index >= self.tabs.len() {
            return;
        }
        if index == self.active_tab {
            #[cfg(target_os = "macos")]
            let neighbor = native_tabs::neighbor_in(&self.native_tab_order(), index)
                .unwrap_or_else(|| neighbor_tab(index, self.tabs.len()));
            #[cfg(not(target_os = "macos"))]
            let neighbor = neighbor_tab(index, self.tabs.len());
            self.select_tab(neighbor);
        }
        let removed = self.tabs.remove(index);
        if index < self.active_tab {
            self.active_tab -= 1;
        }
        #[cfg(target_os = "macos")]
        if let Some(pane) = &removed {
            self.quit_checks
                .borrow_mut()
                .retain(|(id, _)| *id != pane.id);
        }
        drop(removed);
        self.tab_hover = None;
        self.resize_grid();
    }

    /// Tab output while the tab is in the background: parsed right away so its program
    /// never blocks on a full PTY, and drawn once the tab is shown.
    fn background_output(&mut self, id: u64) {
        let Some(pane) = self.tabs.iter_mut().flatten().find(|pane| pane.id == id) else {
            return;
        };
        pane.pty.refresh_flow();
        if pane.pty.flow.paused {
            return;
        }
        pane.pty.wake_pending.store(false, Ordering::Release);
        // Bytes the tab had queued when it was switched away from come first.
        let queued = pty::Output {
            bytes: std::mem::take(&mut pane.output.bytes),
            shell_returns: std::mem::take(&mut pane.output.shell_returns),
        };
        pane.output.waiting_since = None;
        let batch = pane.pty.take_output();
        update_terminal(&mut pane.term, &mut pane.scroll, |term| {
            advance_all(term, queued);
            advance_all(term, batch);
        });
        if pane.term.take_synchronized_output_started() {
            pane.synchronized_since = Some(Instant::now());
        }
        if !pane.term.modes.synchronized_output {
            pane.synchronized_since = None;
        }
        let responses = pane.term.take_responses();
        pane.pty.write(&responses);
        for request in pane.term.take_clipboard_requests() {
            match request {
                ClipboardRequest::Store { primary, text } => self.clipboard.set(text, primary),
                ClipboardRequest::Load { primary, .. } if self.config.clipboard_read => {
                    let text = self.clipboard.get(primary).unwrap_or_default();
                    if let Some(reply) = request.reply(&text) {
                        pane.pty.write(&reply);
                    }
                }
                ClipboardRequest::Load { .. } => {}
            }
        }
        let mut changed = false;
        if let Some(title) = pane.term.take_title() {
            changed |= pane.title != title;
            pane.title = title;
        }
        if let Some(progress) = pane.term.take_progress() {
            changed |= pane.progress != progress;
            pane.progress = progress;
        }
        #[cfg(target_os = "macos")]
        {
            if changed {
                pane.window
                    .window
                    .set_title(&progress::window_title(&pane.title, pane.progress));
            }
            if !pane.window.occluded {
                pane.window.window.request_redraw();
            }
        }
        let bell = std::mem::take(&mut pane.term.bell);
        for notification in pane.term.take_notifications() {
            if self.config.desktop_notifications
                && osc_notify::wanted(notification.occasion, self.focused, || Some(false))
                && self.notification_throttle.allow(Instant::now())
            {
                osc_notify::show(notification);
            }
        }
        let Some(s) = &self.session else { return };
        if bell && !self.focused {
            s.window
                .request_user_attention(Some(winit::window::UserAttentionType::Informational));
        }
        if changed && s.tab_bar > 0 {
            s.window.request_redraw();
        }
    }

    /// Handles a click on the tab bar; false when the click is the terminal's.
    fn tab_mouse_input(
        &mut self,
        state: ElementState,
        button: MouseButton,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        use term_render::TabHit;
        if state == ElementState::Released {
            return std::mem::take(&mut self.tab_click);
        }
        let Some(s) = &self.session else { return false };
        if self.left_down || self.tab_pointer.y >= s.tab_bar as f64 {
            return false;
        }
        self.tab_click = true;
        if self.pending_close.is_some() || self.pending_paste.is_some() {
            return true;
        }
        let layout = term_render::tab_layout(
            self.tabs.len(),
            s.size.width as f32,
            s.fonts.metrics.width,
            s.fonts.metrics.height,
        );
        let hit = layout.hit(self.tab_pointer.x as f32, self.tab_pointer.y as f32);
        match (button, hit) {
            (MouseButton::Left, Some(TabHit::Tab(i))) => self.select_tab(i),
            (MouseButton::Left, Some(TabHit::Close(i)))
            | (MouseButton::Middle, Some(TabHit::Tab(i) | TabHit::Close(i))) => {
                self.close_tab(i, event_loop)
            }
            (MouseButton::Left | MouseButton::Middle, Some(TabHit::New)) => {
                self.new_tab(event_loop)
            }
            _ => {}
        }
        true
    }

    /// Tracks the pointer over the tab bar, and passes it on to the grid in grid
    /// coordinates.
    fn pointer_moved(&mut self, pos: PhysicalPosition<f64>) {
        self.tab_pointer = pos;
        let Some(s) = &self.session else { return };
        let tab_bar = s.tab_bar as f64;
        let over_bar = pos.y < tab_bar;
        let hover = over_bar
            .then(|| {
                term_render::tab_layout(
                    self.tabs.len(),
                    s.size.width as f32,
                    s.fonts.metrics.width,
                    s.fonts.metrics.height,
                )
                .hit(pos.x as f32, pos.y as f32)
            })
            .flatten();
        if hover != self.tab_hover {
            self.tab_hover = hover;
            s.window.request_redraw();
        }
        let grid_pos = PhysicalPosition::new(pos.x, pos.y - tab_bar);
        if over_bar && !self.left_down {
            self.mouse_pos = grid_pos;
            self.refresh_link();
        } else {
            self.cursor_moved(grid_pos);
        }
    }

    fn resize_grid(&mut self) {
        let tab_bar_shown = tab_bar_drawn(self.config.tab_bar.shown(self.tabs.len()));
        let Some(s) = &mut self.session else { return };
        s.scroll.reset();
        s.resize.applied(Instant::now());
        s.tab_bar = if tab_bar_shown {
            term_render::tab_bar_height(s.fonts.metrics.height)
        } else {
            0
        };
        let (cols, rows) = grid_size(&s.window, &s.fonts, self.config.padding, s.tab_bar);
        let cell = (s.fonts.metrics.width, s.fonts.metrics.height);
        // Native macOS tabs each size to their own window, on their own resize or once shown.
        #[cfg(not(target_os = "macos"))]
        for pane in self.tabs.iter_mut().flatten() {
            pane.term.set_cell_pixel_size(cell.0, cell.1);
            if (cols, rows) != (pane.term.cols(), pane.term.rows()) {
                let editing = pane.pty.line_editing();
                search::Search::resize(None, &mut pane.term, cols, rows, editing);
                release_freed_memory(&mut pane.term);
                pane.scroll.reset();
                pane.pty.resize(cols, rows, cell);
                pane.pty_cell = cell;
            } else if pty_resize_needed(false, pane.pty_cell, cell) {
                pane.pty.resize(cols, rows, cell);
                pane.pty_cell = cell;
            }
        }
        let grid_changed = (cols, rows) != (s.term.cols(), s.term.rows());
        s.term
            .set_cell_pixel_size(s.fonts.metrics.width, s.fonts.metrics.height);
        let covered = find_bar_rows(
            s.window.inner_size().height.saturating_sub(s.tab_bar) as f32,
            self.config.padding * s.window.scale_factor() as f32,
            s.fonts.metrics.height,
            rows,
        );
        let covered_changed = self
            .search
            .as_mut()
            .is_some_and(|search| std::mem::replace(&mut search.covered_rows, covered) != covered);
        if grid_changed || covered_changed {
            let editing = s.pty.line_editing();
            search::Search::resize(self.search.as_ref(), &mut s.term, cols, rows, editing);
        }
        if grid_changed {
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
        #[cfg(target_os = "macos")]
        for pane in self.tabs.iter_mut().flatten() {
            let tab = &mut pane.window;
            tab.fonts
                .set_size(self.font_size * tab.window.scale_factor() as f32);
            tab.renderer.clear_glyph_cache();
        }
        self.resize_grid();
        // A tab torn out into its own window stays visible, so its grid follows the font now.
        #[cfg(target_os = "macos")]
        for index in 0..self.tabs.len() {
            if self.tabs[index].is_some() {
                self.with_tab_shown(index, App::resize_grid);
            }
        }
    }

    fn output_ready(&mut self) {
        let Some(s) = &mut self.session else { return };
        s.pty.refresh_flow();
        if s.pty.flow.paused {
            s.deferred = None;
        }
        if !s.output.can_advance(s.pty.flow.paused) {
            return;
        }
        if defer_output(s.deferred.is_some(), s.since_frame, s.pty.output_full()) {
            s.deferred.get_or_insert_with(Instant::now);
        } else {
            self.process_output();
        }
    }

    /// Sets up the Dock and menu bar icons once startup settles, or returns when to check
    /// again. On macOS that is ~15 ms of main-thread IPC with the Dock and the menu bar, which
    /// would otherwise hold back the first frame and the first output.
    fn show_icons_when_idle(&mut self) -> Option<Instant> {
        let latest = self.icons_latest?;
        let s = self.session.as_ref()?;
        if self.exit_deadline.is_some() {
            return None;
        }
        let now = Instant::now();
        if s.sequence > 0 {
            self.first_frame_at.get_or_insert(now);
        }
        let due = icons_due(self.first_frame_at, self.output_at, latest);
        if now < due {
            return Some(due);
        }
        self.icons_latest = None;
        self.icons
            .apply(&s.window, self.config.app_icon, self.config.menu_bar_icon);
        None
    }

    fn process_output(&mut self) {
        if let Some(search) = &mut self.search {
            search.dirty = true;
        }
        let Some(s) = &mut self.session else { return };
        s.pty.refresh_flow();
        if s.pty.flow.paused {
            s.deferred = None;
        }
        if !s.output.can_advance(s.pty.flow.paused) {
            return;
        }
        s.deferred = None;
        let was_synchronized = s.term.modes.synchronized_output;
        let mut advanced = None;
        update_terminal(&mut s.term, &mut s.scroll, |term| {
            advanced = s.output.advance(term, s.pty.flow.paused, || {
                s.pty.wake_pending.store(false, Ordering::Release);
                s.pty.take_output()
            });
        });
        let Some((processed, from_leftover)) = advanced else {
            return;
        };
        s.since_frame += processed;
        self.output_at = Instant::now();
        let synchronized_started = s.term.take_synchronized_output_started();
        if s.term.modes.mouse == MouseMode::Off {
            self.reported_button = None;
        }
        if s.term.modes.synchronized_output {
            if synchronized_started {
                s.synchronized_since = Some(Instant::now());
            }
        } else {
            s.synchronized_since = None;
            if was_synchronized || s.redraw_pending {
                s.request_frame();
            }
        }
        let responses = s.term.take_responses();
        s.pty.write(&responses);
        for request in s.term.take_clipboard_requests() {
            match request {
                ClipboardRequest::Store { primary, text } => self.clipboard.set(text, primary),
                ClipboardRequest::Load { primary, .. } if self.config.clipboard_read => {
                    let text = self.clipboard.get(primary).unwrap_or_default();
                    if let Some(reply) = request.reply(&text) {
                        s.pty.write(&reply);
                    }
                }
                ClipboardRequest::Load { .. } => {}
            }
        }
        let title = s.term.take_title();
        let progress = s.term.take_progress();
        if title.is_some() || progress.is_some() {
            if let Some(title) = title {
                s.title = title;
            }
            if let Some(progress) = progress {
                s.progress = progress;
                progress::set_dock_badge(progress::badge(progress).as_deref());
            }
            s.window
                .set_title(&progress::window_title(&s.title, s.progress));
        }
        if std::mem::take(&mut s.term.bell) && !self.focused {
            s.window
                .request_user_attention(Some(winit::window::UserAttentionType::Informational));
        }
        let visible = std::cell::OnceCell::new();
        for notification in s.term.take_notifications() {
            if self.config.desktop_notifications
                && osc_notify::wanted(notification.occasion, self.focused, || {
                    *visible.get_or_init(|| {
                        if self.occluded {
                            return Some(false);
                        }
                        let shown = s.window.is_minimized().map(|minimized| !minimized);
                        #[cfg(target_os = "macos")]
                        if shown == Some(true) && osc_notify::covered(&s.window) {
                            return Some(false);
                        }
                        shown
                    })
                })
                && self.notification_throttle.allow(Instant::now())
            {
                osc_notify::show(notification);
            }
        }
        if processed > 0 {
            if let Some(log) = &mut s.latency {
                log.output(processed);
            }
            if s.output.pace_frame(s.term.modes.synchronized_output) {
                s.request_frame();
            } else {
                s.window.request_redraw();
            }
        }
        if from_leftover && s.output.is_empty() && s.output.waiting_since.is_none() {
            self.process_output();
        }
    }

    fn present_frame(&mut self) {
        // During a flood, keep parsing instead of blocking in drawable acquisition until
        // the previous frame's vsync; `UserEvent::Presented` brings the frame back.
        // Some Metal drawables never report back, so the wait gives up after `MAX_DEFER`.
        #[cfg(target_os = "macos")]
        if self.exit_deadline.is_none()
            && let Some(s) = &mut self.session
        {
            if s.since_frame >= DEFER_AFTER
                && s.output.waiting_since.is_none()
                && s.drawable_wait
                    .is_none_or(|since| since.elapsed() < MAX_DEFER)
                && present_macos::wait_for_drawable()
            {
                s.drawable_wait.get_or_insert_with(Instant::now);
                return;
            }
            s.drawable_wait = None;
        }
        #[cfg(target_os = "linux")]
        if self.exit_deadline.is_some() && !self.exit_presentation.needs_wayland_submission() {
            return;
        }
        #[cfg(target_os = "macos")]
        if self.exit_deadline.is_some()
            && !self
                .exit_presentation
                .needs_metal_submission(present_macos::feedback().0)
        {
            return;
        }
        self.render_frame();
        #[cfg(target_os = "linux")]
        if self.exit_deadline.is_some()
            && self.exit_presentation.target.is_some()
            && self.exit_presentation.needs_wayland_submission()
        {
            self.render_frame();
        }
        #[cfg(target_os = "macos")]
        if self.exit_deadline.is_some()
            && self.exit_presentation.target.is_some()
            && self
                .exit_presentation
                .needs_metal_submission(present_macos::feedback().0)
        {
            // Submit the redundant final drawable in this turn, not at the next
            // window redraw (which can cost another refresh after shell exit).
            self.render_frame();
        }
    }

    fn render_frame(&mut self) {
        if self.exit_deadline.is_none()
            && let Some(s) = &mut self.session
        {
            s.pty.refresh_flow();
            if s.output.waiting_since.is_some() && !s.pty.flow.paused {
                s.pty.wake_pending.store(false, Ordering::Release);
                let queued = s.pty.take_output();
                if s.output.coalesce(queued) {
                    self.process_output();
                }
            }
        }
        if let (Some(s), Some(search)) = (&mut self.session, &mut self.search)
            && search.dirty
        {
            search.refresh(&mut s.term, false);
        }
        self.update_ime_cursor_area();
        let floating = self.hover_preview_overlay();
        let hover = self.hover_link();
        let Some(s) = &mut self.session else { return };
        s.renderer.set_floating(floating);
        if s.term.modes.synchronized_output {
            s.redraw_pending = true;
            return;
        }
        s.redraw_pending = false;
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
        let banner = match &self.pending_paste {
            Some(text) => Some(paste::banner(text, banner)),
            None => banner,
        };
        let banner = match &self.pending_close {
            Some(program) => Some(close_confirm::banner(
                program.as_deref(),
                self.pending_close_tab.is_some(),
                banner,
            )),
            None => banner,
        };
        let started = Instant::now();
        #[cfg(target_os = "linux")]
        let exit_target = (self.exit_deadline.is_some()
            && self.exit_presentation.needs_wayland_submission()
            && s.presentation.is_some())
        .then_some(s.sequence + 1);
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
        #[cfg(target_os = "macos")]
        let exit_target = (self.exit_deadline.is_some()
            && self
                .exit_presentation
                .needs_metal_submission(present_macos::feedback().0))
        .then(present_macos::arm_exit);
        let (blink_visible, blink_flip) = blink_phase(s.blink_epoch, started);
        s.renderer.blink_visible = blink_visible;
        let blinking = s.term.cursor_blink && s.term.modes.show_cursor && self.focused;
        let (cursor_visible, cursor_blink_at) =
            cursor_blink(blinking, s.blink_since, Instant::now());
        s.renderer.cursor_visible = cursor_visible;
        s.cursor_blink_at = cursor_blink_at;
        let tab_bar = (s.tab_bar > 0).then(|| term_render::TabBar {
            titles: self
                .tabs
                .iter()
                .map(|pane| tab_title(pane.as_ref().map_or(&s.title, |pane| &pane.title)))
                .collect(),
            active: self.active_tab,
            hover: self.tab_hover,
        });
        s.renderer.set_tab_bar(tab_bar, s.tab_bar);
        let rendered = s.renderer.render(
            &s.term,
            &mut s.fonts,
            pad,
            s.scroll.offset as f32,
            self.focused,
            banner.as_deref(),
            search.as_ref(),
            s.composition.preedit().filter(|_| search.is_none()),
            hover.as_ref(),
            Some(&mut pre_present),
        );
        s.blink_at = s.renderer.blinking().then_some(blink_flip);
        if rendered {
            if self.startup_stderr {
                if let Err(error) = finish_startup_stderr() {
                    let _ = writeln!(std::io::stderr(), "trident: startup stderr: {error}");
                    std::process::exit(1);
                }
                self.startup_stderr = false;
            }
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            if let Some(target) = exit_target {
                self.exit_presentation.submitted(target, Instant::now());
            }
            if let Some(log) = &mut s.latency {
                log.frame(Instant::now(), s.since_frame);
            }
            s.since_frame = 0;
            s.sequence += 1;
        }
        if s.output.finish_presentation(rendered, Instant::now()) {
            self.process_output();
        }
    }

    fn hide_pointer_for_typing(&mut self) {
        if let Some(s) = &self.session {
            self.typing_pointer
                .typed(self.config.hide_mouse_when_typing, &s.window);
        }
    }

    fn write_input(&mut self, bytes: &[u8]) {
        let Some(s) = &mut self.session else { return };
        if let Some(log) = &mut s.latency {
            log.input(Instant::now());
        }
        let visible_change = reset_view_for_input(&mut s.term, &mut s.scroll);
        s.pty.write(bytes);
        s.blink_since = Instant::now();
        if visible_change
            || !s.renderer.cursor_visible
            || s.cursor_blink_at.is_none() && s.term.cursor_blink
        {
            s.request_frame();
        }
    }

    fn paste(&mut self, primary: bool) {
        // A waiting paste keeps the banner until Enter or Escape resolves it.
        if self.pending_paste.is_some() {
            return;
        }
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
        if self.config.paste_protection
            && paste::needs_confirmation(&text, s.term.modes.bracketed_paste)
        {
            self.pending_paste = Some(text);
            s.window.request_redraw();
            return;
        }
        self.send_paste(&text);
    }

    fn send_paste(&mut self, text: &str) {
        let Some(s) = &self.session else { return };
        let bytes = paste::encode(text, s.term.modes.bracketed_paste);
        self.write_input(&bytes);
    }

    /// Enter sends the waiting paste and Escape drops it; other keys are ignored.
    #[cold]
    fn confirm_paste_key(&mut self, event: &KeyEvent) {
        use winit::keyboard::NamedKey;
        let confirmed = match event.logical_key {
            Key::Named(NamedKey::Enter) => true,
            Key::Named(NamedKey::Escape) => false,
            _ => return,
        };
        if let Some(text) = self.pending_paste.take() {
            if confirmed {
                self.send_paste(&text);
            }
            // `write_input` only redraws for a view change, so clear the banner here.
            if let Some(s) = &self.session {
                s.window.request_redraw();
            }
        }
    }

    /// Closes at once unless a program other than the shell holds the foreground; then
    /// the banner asks first, and a second close request confirms.
    #[cold]
    fn close_requested(&mut self, event_loop: &ActiveEventLoop) {
        let running = self
            .session
            .as_ref()
            .filter(|_| {
                self.config.confirm_close
                    && (self.pending_close.is_none()
                        || self.pending_close_tab.is_some()
                        || self.window_prompt())
            })
            .and_then(|s| {
                s.pty.running_program().or_else(|| {
                    self.tabs
                        .iter()
                        .flatten()
                        .find_map(|pane| pane.pty.running_program())
                })
            });
        match (running, &self.session) {
            (Some(pid), Some(s)) => {
                // A window close while a tab close is being asked about asks about the window.
                self.pending_close_tab = None;
                #[cfg(target_os = "macos")]
                {
                    self.pending_close_window = None;
                }
                self.pending_close = Some(close_confirm::process_name(pid));
                if s.window.is_minimized() == Some(true) {
                    s.window.set_minimized(false);
                }
                s.window.request_redraw();
            }
            _ => {
                if !close_confirm::answer_quit(true) {
                    event_loop.exit();
                }
            }
        }
    }

    /// Whether the close prompt is for one native window's tabs rather than the app.
    fn window_prompt(&self) -> bool {
        #[cfg(target_os = "macos")]
        return self.pending_close_window.is_some();
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// Clears a close prompt for one native window's tabs; whether there was one.
    fn take_window_prompt(&mut self) -> bool {
        #[cfg(target_os = "macos")]
        return self.pending_close_window.take().is_some();
        #[cfg(not(target_os = "macos"))]
        false
    }

    /// Enter closes the window and Escape keeps it; other keys are ignored.
    #[cold]
    fn confirm_close_key(&mut self, event: &KeyEvent, event_loop: &ActiveEventLoop) {
        use winit::keyboard::NamedKey;
        match event.logical_key {
            Key::Named(NamedKey::Enter) => {
                #[cfg(target_os = "macos")]
                if let Some(ids) = self.pending_close_window.take() {
                    self.pending_close = None;
                    self.remove_tabs(&ids);
                    return;
                }
                if let Some(id) = self.pending_close_tab.take() {
                    self.pending_close = None;
                    if let Some(index) = self.tab_index(id) {
                        self.remove_tab(index);
                    }
                    if let Some(s) = &self.session {
                        s.window.request_redraw();
                    }
                } else if !close_confirm::answer_quit(true) {
                    event_loop.exit();
                }
            }
            Key::Named(NamedKey::Escape) => {
                self.pending_close = None;
                if self.pending_close_tab.take().is_none() && !self.take_window_prompt() {
                    close_confirm::answer_quit(false);
                }
                if let Some(s) = &self.session {
                    s.window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn dropped_file(&mut self, path: PathBuf) {
        if self.pending_paste.is_some() {
            return;
        }
        // Flush any deferred echo before inserting rows above the current prompt.
        self.process_output();
        let Some(s) = &mut self.session else { return };
        let Some(bytes) = drop_files::quoted_path(&path) else {
            diag!("trident: cannot drop a path containing control characters");
            return;
        };
        // `write_input` can no longer see this scroll or the preview, so track them here.
        let mut visible_change = s.term.display_offset() != 0;
        s.term.scroll_to_bottom();
        if drop_files::preview_enabled(self.config.drop_preview, &s.term)
            && let Some((image, rows)) =
                show::drop_preview(&path, s.term.cols(), s.term.rows(), s.pty_cell)
        {
            drop_files::insert_preview(&mut s.term, &image, rows);
            visible_change = true;
        }
        // Only the literal, quoted path goes to the shell: never a show command or Enter.
        self.write_input(&bytes);
        if visible_change && let Some(s) = &mut self.session {
            s.request_frame();
        }
    }

    fn copy(&mut self, primary: bool) {
        let Some(s) = &self.session else { return };
        if let Some(text) = s.term.selection_text().filter(|t| !t.is_empty()) {
            self.clipboard.set(text, primary);
        }
    }

    fn watch_appearance(&mut self) {
        #[cfg(target_os = "linux")]
        if !self.config.themes.automatic() {
            self.appearance_watch = None;
            self.appearance = None;
        } else if self.appearance_watch.is_none() {
            self.appearance_generation += 1;
            let generation = self.appearance_generation;
            let proxy = self.proxy.clone();
            self.appearance_watch = appearance::watch(move |theme| {
                let _ = proxy.send_event(UserEvent::Appearance(generation, theme));
            })
            .map_err(|e| diag!("trident: cannot watch OS appearance: {e}"))
            .ok();
        }
    }

    fn set_appearance(&mut self, appearance: Option<Theme>) {
        if self.appearance == appearance {
            return;
        }
        self.appearance = appearance;
        if self.config.themes.automatic()
            && let Some(s) = &mut self.session
        {
            s.term
                .set_palette(self.config.themes.palette(appearance).clone());
            let responses = s.term.take_responses();
            s.pty.write(&responses);
            s.window.request_redraw();
            for pane in self.tabs.iter_mut().flatten() {
                pane.term
                    .set_palette(self.config.themes.palette(appearance).clone());
                let responses = pane.term.take_responses();
                pane.pty.write(&responses);
            }
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
                            "trident: {}:1:1: font: {e}",
                            self.options.config_path.display()
                        )
                    })?,
                )
            } else {
                None
            };
            if next.shell != self.config.shell
                || next.term != self.config.term
                || next.env != self.config.env
            {
                diag!("trident: shell/term/env changes apply on the next launch");
            }
            let next_vsync_off = !next.vsync && self.config.vsync;
            for warning in &next.warnings {
                diag!("{warning}");
            }
            self.config = next;
            if !self.config.hide_mouse_when_typing {
                self.typing_pointer.show();
            }
            #[cfg(target_os = "macos")]
            close_confirm::QUIT_CONFIRMS.store(self.config.confirm_close, Ordering::Relaxed);
            #[cfg(target_os = "macos")]
            ime::set_command_chords(&self.config.bindings);
            self.watch_appearance();
            if let Some(s) = &mut self.session {
                self.icons_latest = None;
                self.icons
                    .apply(&s.window, self.config.app_icon, self.config.menu_bar_icon);
                s.pty
                    .set_command_notifications(self.config.notify_long_commands_after != 0)?;
                s.term
                    .set_palette(self.config.themes.palette(self.appearance).clone());
                let responses = s.term.take_responses();
                s.pty.write(&responses);
                let scrollback = self.options.scrollback(&self.config);
                configure_term(&mut s.term, &self.config, scrollback);
                if let Some(search) = &mut self.search {
                    search.dirty = true;
                }
                release_freed_memory(&mut s.term);
                for pane in self.tabs.iter_mut().flatten() {
                    pane.pty
                        .set_command_notifications(self.config.notify_long_commands_after != 0)?;
                    pane.term
                        .set_palette(self.config.themes.palette(self.appearance).clone());
                    let responses = pane.term.take_responses();
                    pane.pty.write(&responses);
                    configure_term(&mut pane.term, &self.config, scrollback);
                    release_freed_memory(&mut pane.term);
                }
                if next_vsync_off {
                    warn_vsync_unsupported(&self.config, &s.renderer);
                }
                s.renderer.set_vsync(self.config.vsync);
                if let Some(fonts) = fonts {
                    s.fonts = fonts;
                    s.renderer.clear_glyph_cache();
                    #[cfg(target_os = "macos")]
                    for pane in self.tabs.iter_mut().flatten() {
                        let tab = &mut pane.window;
                        let px = size * tab.window.scale_factor() as f32;
                        match Fonts::new(self.config.font.as_deref(), px) {
                            Ok(fonts) => {
                                tab.fonts = fonts;
                                tab.renderer.clear_glyph_cache();
                            }
                            Err(error) => diag!("trident: font: {error}"),
                        }
                    }
                }
                #[cfg(target_os = "macos")]
                for pane in self.tabs.iter_mut().flatten() {
                    pane.window.renderer.set_vsync(self.config.vsync);
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
            diag!("{error}");
        }
        if let Some(s) = &self.session {
            s.window.request_redraw();
        }
    }

    /// Handles terminal shortcuts; true if the key was consumed.
    fn shortcut(
        &mut self,
        event: &KeyEvent,
        held_at_focus: bool,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        let mods = self.modifiers;
        let Some(chord) = shortcut_chord(event.key_without_modifiers(), mods) else {
            return false;
        };
        let Some(action) = self.config.bindings.get(&chord).cloned() else {
            return cfg!(target_os = "macos") && mods.super_key();
        };
        if suppress_shortcut_repeat(&action, event.repeat, held_at_focus) {
            return true;
        }
        match action {
            Action::Search => {
                let search = self.search.get_or_insert_with(search::Search::default);
                if let Some(s) = &self.session {
                    search.covered_rows = find_bar_rows(
                        s.window.inner_size().height.saturating_sub(s.tab_bar) as f32,
                        self.config.padding * s.window.scale_factor() as f32,
                        s.fonts.metrics.height,
                        s.term.rows(),
                    );
                }
                if let Some((text, cursor)) =
                    self.session.as_mut().and_then(|s| s.composition.take())
                {
                    search.set_preedit(text, cursor);
                }
                self.refresh_link();
                self.update_ime_cursor_area();
                if let Some(s) = &self.session {
                    s.window.request_redraw();
                }
            }
            Action::NewTab => self.new_tab(event_loop),
            Action::CloseTab => self.close_tab(self.active_tab, event_loop),
            #[cfg(target_os = "macos")]
            Action::NextTab => self.step_native_tab(1),
            #[cfg(target_os = "macos")]
            Action::PreviousTab => self.step_native_tab(-1),
            #[cfg(target_os = "macos")]
            Action::SelectTab(index) => {
                let order = self.native_tab_order();
                if let Some(&tab) = order.get(shortcut_tab(index, order.len())) {
                    self.select_tab(tab);
                }
            }
            #[cfg(not(target_os = "macos"))]
            Action::NextTab => self.select_tab((self.active_tab + 1) % self.tabs.len().max(1)),
            #[cfg(not(target_os = "macos"))]
            Action::PreviousTab => self.select_tab(
                (self.active_tab + self.tabs.len().max(1) - 1) % self.tabs.len().max(1),
            ),
            #[cfg(not(target_os = "macos"))]
            Action::SelectTab(index) => self.select_tab(shortcut_tab(index, self.tabs.len())),
            Action::NewWindow => {
                let working_directory = self
                    .session
                    .as_ref()
                    .and_then(|s| local_working_directory(s.term.working_directory()));
                if let Err(error) =
                    std::thread::Builder::new()
                        .name("new-window".into())
                        .spawn(move || {
                            let result = std::env::current_exe().and_then(|exe| {
                                let (stderr, startup_stderr) = new_window_stderr();
                                let mut command = std::process::Command::new(exe);
                                command
                                    .args(std::env::args_os().skip(1))
                                    .stdin(Stdio::null())
                                    .stdout(Stdio::null())
                                    .stderr(stderr);
                                if startup_stderr {
                                    command.env(STARTUP_STDERR, "1");
                                }
                                if let Some(dir) = working_directory {
                                    command.env(WORKING_DIRECTORY, dir);
                                }
                                command.spawn()
                            });
                            match result {
                                Ok(mut child) => {
                                    if let Some(stderr) = child.stderr.take()
                                        && let Err(error) =
                                            forward_new_window_stderr(stderr, std::io::stderr())
                                    {
                                        let _ = writeln!(std::io::stderr(), "new window: {error}");
                                    }
                                    match child.wait() {
                                        Ok(status) if !status.success() => {
                                            let _ = writeln!(
                                                std::io::stderr(),
                                                "trident: new window exited with {status}"
                                            );
                                        }
                                        Err(error) => {
                                            let _ =
                                                writeln!(std::io::stderr(), "new window: {error}");
                                        }
                                        Ok(_) => {}
                                    }
                                }
                                Err(error) => {
                                    let _ = writeln!(std::io::stderr(), "new window: {error}");
                                }
                            }
                        })
                {
                    let _ = writeln!(std::io::stderr(), "new window: {error}");
                }
            }
            Action::ToggleFullscreen => {
                // The window's `Resized` events then resize the grid through `s.resize`.
                if let Some(s) = &self.session {
                    #[cfg(target_os = "macos")]
                    native_tabs::toggle_fullscreen(&s.window);
                    #[cfg(not(target_os = "macos"))]
                    s.window
                        .set_fullscreen(toggled_fullscreen(s.window.fullscreen()));
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
                    self.hide_pointer_for_typing();
                }
            }
            Action::JumpToPromptUp | Action::JumpToPromptDown | Action::SelectCommandOutput => {
                if !self
                    .session
                    .as_ref()
                    .is_some_and(|s| shell_shortcuts_available(&s.term))
                {
                    return false;
                }
                if let Some(s) = &mut self.session {
                    s.scroll.reset();
                    if action == Action::SelectCommandOutput {
                        s.term.select_command_output();
                    } else {
                        s.term.jump_to_prompt(action == Action::JumpToPromptUp);
                    }
                    s.request_frame();
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
            Action::LineUp
            | Action::LineDown
            | Action::PageUp
            | Action::PageDown
            | Action::Top
            | Action::Bottom => {
                if let Some(s) = &mut self.session {
                    if passes_through_on_alt_screen(&action) && s.term.is_alt_screen() {
                        return false;
                    }
                    let page = s.term.rows() as isize - 1;
                    let delta = match action {
                        Action::LineUp => 1,
                        Action::LineDown => -1,
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
        let size = s.grid_px();
        let mut area = if let Some(search) = &self.search {
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
            let (row, col) = s
                .term
                .input_cursor_position()
                .unwrap_or((s.term.rows() - 1, 0));
            let col = s.composition.preedit().map_or(col, |p| {
                let layout = p.layout(col, s.term.cols());
                layout.caret.unwrap_or(layout.column)
            });
            let pad = self.config.padding * s.window.scale_factor() as f32;
            cursor_area(
                size,
                pad,
                (row, col),
                s.scroll.offset as f32,
                (m.width, m.height),
            )
        };
        area[1] += s.tab_bar as f32;
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
            .is_some_and(|s| input::tracks_mouse(s.term.modes.mouse, self.modifiers))
    }

    fn hover_link(&self) -> Option<term_core::Link> {
        if !self.focused || !links::command_modifier(self.modifiers) {
            return None;
        }
        let s = self.session.as_ref()?;
        if pointer_in_search_bar(
            self.search.is_some(),
            self.mouse_pos,
            s.grid_px(),
            (s.fonts.metrics.width, s.fonts.metrics.height),
        ) {
            return None;
        }
        let pad = self.config.padding * s.window.scale_factor() as f32;
        if term_render::ledger_bar(&s.term, pad, s.scroll.offset as f32, s.fonts.metrics.height)
            .is_some_and(|bar| bar.contains(self.mouse_pos.y as f32))
        {
            return None;
        }
        if !pointer_in_grid(
            self.mouse_pos,
            self.config.padding as f64 * s.window.scale_factor(),
            (s.term.cols(), s.term.rows()),
            (s.fonts.metrics.width, s.fonts.metrics.height),
        ) {
            return None;
        }
        let (row, col) = self.cell_at(self.mouse_pos)?;
        if self.search.is_none()
            && let Some(preedit) = s.composition.preedit()
            && let Some((cursor_row, cursor_col)) = s.term.input_cursor_position()
            && cell_in_preedit(
                &preedit.layout(cursor_col, s.term.cols()),
                cursor_row,
                (row, col),
            )
        {
            return None;
        }
        s.term.link_at(s.term.viewport_to_point_signed(row, col))
    }

    fn refresh_link(&mut self) {
        let link = self.hover_link();
        if let Some(s) = &mut self.session
            && s.hover != link
        {
            s.window.set_cursor(if link.is_some() {
                winit::window::CursorIcon::Pointer
            } else {
                winit::window::CursorIcon::Default
            });
            s.hover = link;
            s.request_frame();
        }
        self.refresh_hover_preview();
    }

    fn report_mouse(&mut self, button: ReportButton, pressed: bool, motion: bool) {
        let Some((row, col)) = self.cell_at(self.mouse_pos) else {
            return;
        };
        let Some(s) = &mut self.session else { return };
        let sgr = s.term.modes.sgr_mouse;
        if let Some(bytes) = input::encode_mouse_report(
            s.term.modes.mouse,
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

    /// Clicking the pinned command header scrolls to that command's prompt.
    fn click_ledger(&mut self) -> bool {
        let Some(s) = &mut self.session else {
            return false;
        };
        let pad = self.config.padding * s.window.scale_factor() as f32;
        let Some(line) =
            term_render::ledger_bar(&s.term, pad, s.scroll.offset as f32, s.fonts.metrics.height)
                .filter(|bar| bar.contains(self.mouse_pos.y as f32))
                .map(|bar| bar.pinned.command.prompt_line)
        else {
            return false;
        };
        s.scroll.reset();
        s.term.scroll_to_line(line);
        s.request_frame();
        true
    }

    fn mouse_input(&mut self, state: ElementState, button: MouseButton) {
        let pressed = state == ElementState::Pressed;
        if button == MouseButton::Left {
            if pressed && !self.left_down && self.click_ledger() {
                self.ledger_click = true;
                return;
            }
            if !pressed && std::mem::take(&mut self.ledger_click) {
                return;
            }
        }
        match links::mouse_action(
            button,
            pressed,
            self.session.as_ref().is_some_and(|s| s.link_pressed),
            || self.hover_link(),
        ) {
            Some(links::MouseAction::Release) => {
                self.session.as_mut().unwrap().link_pressed = false;
                return;
            }
            Some(links::MouseAction::Open(link)) => {
                self.session.as_mut().unwrap().link_pressed = true;
                links::open(link.uri);
                return;
            }
            None => {}
        }
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
        self.refresh_link();
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

/// Hand memory from dropped rows or recycled buffers back to the OS: glibc keeps
/// freed heap pages otherwise.
/// Applies the config settings every tab's terminal shares, at startup, in new tabs and on reload.
fn configure_term(term: &mut Terminal, config: &config::Config, scrollback: usize) {
    term.set_scrollback_limit(scrollback);
    term.set_command_budget(config.command_budget, config.command_share);
    term.set_compare_runs(config.compare_runs);
    term.set_compare_ignore(&config.compare_ignore);
}

fn release_freed_memory(term: &mut Terminal) -> bool {
    let shrunk = term.take_shrunk();
    let recycled = term.take_recycled_memory_freed();
    if shrunk || recycled {
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

fn warn_vsync_unsupported(config: &config::Config, renderer: &Renderer) {
    if !config.vsync && !renderer.vsync_optional {
        diag!("trident: vsync = false: this GPU surface only supports vsync; keeping it on");
    }
}

fn cell_in_preedit(
    layout: &term_render::PreeditLayout,
    preedit_row: usize,
    (row, col): (isize, usize),
) -> bool {
    row == preedit_row as isize && (layout.column..layout.column + layout.width).contains(&col)
}

fn pointer_in_search_bar(
    visible: bool,
    pos: PhysicalPosition<f64>,
    size: PhysicalSize<u32>,
    cell: (f32, f32),
) -> bool {
    if !visible {
        return false;
    }
    let layout = term_render::search_layout(
        0,
        0..0,
        0,
        size.width as f32,
        size.height as f32,
        cell.0,
        cell.1,
    );
    pos.x >= 0.0
        && pos.x < size.width as f64
        && pos.y >= layout.cursor_area[1] as f64
        && pos.y < size.height as f64
}

/// How many of the grid's `rows` bottom rows the Find bar is drawn over, wholly or in part.
fn find_bar_rows(height: f32, padding: f32, cell_height: f32, rows: usize) -> usize {
    let bar_top =
        term_render::search_layout(0, 0..0, 0, cell_height, height, cell_height, cell_height)
            .cursor_area[1];
    let clear = ((bar_top - padding) / cell_height).floor().max(0.0) as usize;
    rows.saturating_sub(clear)
}

fn pointer_in_grid(
    pos: PhysicalPosition<f64>,
    padding: f64,
    grid: (usize, usize),
    cell: (f32, f32),
) -> bool {
    pos.x >= padding
        && pos.x < padding + grid.0 as f64 * cell.0 as f64
        && pos.y >= padding
        && pos.y < padding + grid.1 as f64 * cell.1 as f64
}

/// Where the IME candidate window goes for the cell at `(row, col)`, kept inside `size`.
fn cursor_area(
    size: PhysicalSize<u32>,
    pad: f32,
    (row, col): (usize, usize),
    offset: f32,
    (width, height): (f32, f32),
) -> [f32; 4] {
    [
        (pad + col as f32 * width).min((size.width as f32 - width).max(0.0)),
        (pad + row as f32 * height + offset).min((size.height as f32 - height).max(0.0)),
        width,
        height,
    ]
}

fn below_tab_bar(size: PhysicalSize<u32>, tab_bar: u32) -> PhysicalSize<u32> {
    PhysicalSize::new(size.width, size.height.saturating_sub(tab_bar))
}

fn grid_size(window: &Window, fonts: &Fonts, padding: f32, tab_bar: u32) -> (usize, usize) {
    grid_size_for(
        below_tab_bar(window.inner_size(), tab_bar),
        window.scale_factor(),
        fonts,
        padding,
    )
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
                self.icons_latest = Some(Instant::now() + ICONS_LATEST);
                #[cfg(target_os = "macos")]
                close_confirm::QUIT_CONFIRMS.store(self.config.confirm_close, Ordering::Relaxed);
                #[cfg(target_os = "macos")]
                close_confirm::route_app_quit_through_close(&session.window, {
                    let checks = self.quit_checks.clone();
                    move || checks.borrow().iter().any(|(_, running)| running())
                });
                #[cfg(target_os = "macos")]
                self.quit_checks
                    .borrow_mut()
                    .push((session.pane, Box::new(session.pty.running_program_check())));
                #[cfg(target_os = "macos")]
                ime::set_command_chords(&self.config.bindings);
                #[cfg(target_os = "macos")]
                ime::forward_command_chords(&session.window);
                #[cfg(target_os = "macos")]
                native_tabs::handle_new_tab_button(&session.window, self.proxy.clone());
                #[cfg(target_os = "macos")]
                native_tabs::install_window_menu();
                self.clipboard = clipboard::Clipboard::new(session.window.clone());
                self.session = Some(session);
                self.tabs = vec![None];
                self.active_tab = 0;
                self.process_output();
            }
            Err(e) => {
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }

    /// A macOS app quit ends the process from AppKit without returning from the event loop,
    /// so the latency log is written here instead of when the session drops.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(s) = &mut self.session {
            s.latency.take();
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            #[cfg(target_os = "macos")]
            UserEvent::NewTab => self.new_tab(event_loop),
            #[cfg(target_os = "linux")]
            UserEvent::Appearance(generation, theme) => {
                if self.appearance_watch.is_some() && generation == self.appearance_generation {
                    self.set_appearance(theme);
                }
            }
            UserEvent::CommandFinished(id, finished) => {
                let watched = self.session.as_ref().is_some_and(|s| s.pane == id);
                if command_notify::should_notify(
                    self.focused && watched,
                    finished.duration,
                    self.config.notify_long_commands_after,
                ) {
                    command_notify::notify(finished);
                }
            }
            UserEvent::ConfigChanged => self.reload_config(),
            UserEvent::HoverPreview(ready) => self.hover_preview_ready(ready),
            UserEvent::Output(id) if self.session.as_ref().is_some_and(|s| s.pane == id) => {
                self.output_ready()
            }
            UserEvent::Output(id) => self.background_output(id),
            #[cfg(target_os = "macos")]
            UserEvent::Presented => {
                if self.exit_deadline.is_none()
                    && let Some(s) = &self.session
                {
                    s.window.request_redraw();
                }
            }
            UserEvent::Exit(id) if self.tabs.len() > 1 => {
                if let Some(index) = self.tab_index(id) {
                    if self.pending_close_tab == Some(id) {
                        self.pending_close_tab = None;
                        self.pending_close = None;
                    }
                    self.remove_tab(index);
                }
            }
            UserEvent::Exit(id) if self.session.as_ref().is_some_and(|s| s.pane != id) => {}
            UserEvent::Exit(_) => {
                self.exit_deadline
                    .get_or_insert_with(|| Instant::now() + EXIT_TIMEOUT);
                if let Some(s) = &mut self.session {
                    #[cfg(target_os = "macos")]
                    {
                        present_macos::begin_exit();
                        s.renderer.prepare_exit();
                    }
                    let queued = s.pty.take_output();
                    let mut processed = 0;
                    update_terminal(&mut s.term, &mut s.scroll, |term| {
                        processed = s.output.drain_on_exit(term, queued);
                    });
                    s.since_frame += processed;
                    if let Some(log) = &mut s.latency {
                        log.output(processed);
                    }
                    s.synchronized_since = None;
                    s.deferred = None;
                    if s.since_frame == 0 && s.sequence > 0 {
                        // Do not replace an already submitted final frame with an unchanged commit.
                        s.output.presented();
                        #[cfg(target_os = "linux")]
                        if s.presentation.is_some() {
                            self.exit_presentation.submitted(s.sequence, Instant::now());
                        }
                        #[cfg(target_os = "macos")]
                        self.exit_presentation
                            .submitted(present_macos::arm_exit() - 1, Instant::now());
                    }
                }
                let pending_final = self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.since_frame > 0 || s.sequence == 0);
                #[cfg(target_os = "linux")]
                let pending_final = pending_final
                    || self
                        .session
                        .as_ref()
                        .is_some_and(|s| s.presentation.is_some())
                        && self.exit_presentation.needs_wayland_submission();
                #[cfg(target_os = "macos")]
                let pending_final = pending_final
                    || self
                        .exit_presentation
                        .needs_metal_submission(present_macos::feedback().0);
                if pending_final {
                    self.present_frame();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(s) = &mut self.session {
            s.pty.refresh_flow();
        }
        #[cfg(target_os = "macos")]
        self.secure_input.set(
            self.config.auto_secure_input
                && self.focused
                && self
                    .session
                    .as_ref()
                    .is_some_and(|s| s.pty.password_input()),
        );
        self.refresh_link();
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
        if exit_ready(self.exit_deadline, false, now) {
            event_loop.exit();
            return;
        }
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
        if self.session.as_ref().is_some_and(|s| {
            s.output
                .waiting_since
                .is_some_and(|since| since.elapsed() >= MAX_DEFER)
        }) {
            self.present_frame();
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
        let output_done = self
            .session
            .as_ref()
            .is_none_or(|s| s.output.is_empty() && s.output.waiting_since.is_none());
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let output_done = {
            #[cfg(target_os = "macos")]
            let feedback = Some(present_macos::feedback());
            #[cfg(target_os = "linux")]
            let feedback = self
                .session
                .as_ref()
                .and_then(|s| s.presentation.as_ref())
                .map(|feedback| feedback.exit_feedback());
            if self.exit_deadline.is_some()
                && let Some((presented, skipped)) = feedback
            {
                let refresh = self
                    .session
                    .as_ref()
                    .and_then(|s| s.window.current_monitor())
                    .and_then(|m| m.refresh_rate_millihertz())
                    .filter(|rate| *rate > 0)
                    .unwrap_or(60_000);
                #[cfg(target_os = "macos")]
                let retry = self.exit_presentation.update_metal(
                    presented,
                    skipped,
                    Instant::now(),
                    refresh,
                );
                #[cfg(target_os = "linux")]
                let retry =
                    self.exit_presentation
                        .update(presented, skipped, Instant::now(), refresh);
                if retry {
                    self.present_frame();
                }
            }
            output_done && (feedback.is_none() || self.exit_presentation.ready(Instant::now()))
        };
        if exit_ready(self.exit_deadline, output_done, Instant::now()) {
            event_loop.exit();
            return;
        }
        let searching = match (&mut self.session, &mut self.search) {
            (Some(s), Some(search)) if search.searching() => {
                if search.dirty {
                    search.refresh(&mut s.term, false);
                    s.window.request_redraw();
                } else if search.advance(&mut s.term, Some(Instant::now() + search::SLICE)) {
                    s.window.request_redraw();
                }
                search.searching()
            }
            _ => false,
        };
        self.update_ime_cursor_area();
        let mut deadline = None;
        if let Some(s) = &mut self.session {
            if sync_expired(s.synchronized_since, Instant::now()) {
                s.term.modes.synchronized_output = false;
                s.synchronized_since = None;
                s.window.request_redraw();
            }
            deadline = wake_deadline(
                frame_wakeup(s.redraw_at.map(|(latch, _)| latch), s.deferred),
                s.resize.deadline,
                s.latency.as_ref().and_then(|l| l.wake_deadline(now)),
            );
            deadline = sync_wakeup(deadline, s.output.waiting_since, s.synchronized_since);
            if let Some(flip) = s.blink_at {
                if now >= flip {
                    s.blink_at = None;
                    s.window.request_redraw();
                } else {
                    deadline = Some(deadline.map_or(flip, |deadline| deadline.min(flip)));
                }
            }
            #[cfg(target_os = "macos")]
            if let Some(since) = s.drawable_wait {
                let retry = since + MAX_DEFER;
                if now >= retry {
                    s.window.request_redraw();
                } else {
                    deadline = Some(deadline.map_or(retry, |deadline| deadline.min(retry)));
                }
            }
            if s.pty.flow.paused {
                let poll = now + Duration::from_millis(100);
                deadline = Some(deadline.map_or(poll, |deadline| deadline.min(poll)));
            }
            if let Some(blink) = s.cursor_blink_at {
                if now >= blink {
                    s.cursor_blink_at = None;
                    s.window.request_redraw();
                } else {
                    deadline = Some(deadline.map_or(blink, |deadline| deadline.min(blink)));
                }
            }
        }
        #[cfg(target_os = "macos")]
        if self.config.auto_secure_input && self.focused && self.session.is_some() {
            let poll = now + secure_input::POLL;
            deadline = Some(deadline.map_or(poll, |deadline| deadline.min(poll)));
        }
        if let Some(due) = self.show_icons_when_idle() {
            deadline = Some(deadline.map_or(due, |deadline| deadline.min(due)));
        }
        deadline = exit_wakeup(deadline, self.exit_deadline);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if self.exit_deadline.is_some() && self.exit_presentation.target.is_some() {
            deadline = exit_wakeup(
                deadline,
                Some(self.exit_presentation.wakeup(Instant::now())),
            );
        }
        event_loop.set_control_flow(if searching {
            ControlFlow::Poll
        } else {
            deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil)
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        #[cfg(target_os = "macos")]
        if self.session.as_ref().is_some_and(|s| s.window.id() != id) {
            self.background_window_event(event_loop, id, event);
            return;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = id;
        match event {
            WindowEvent::ThemeChanged(theme) => self.set_appearance(Some(theme)),
            #[cfg(target_os = "macos")]
            WindowEvent::CloseRequested => match close_confirm::take_wide_close() {
                Some(close_confirm::WideClose::Quit) => self.close_requested(event_loop),
                Some(close_confirm::WideClose::Window) => self.close_window(event_loop),
                None => self.close_tab(self.active_tab, event_loop),
            },
            #[cfg(not(target_os = "macos"))]
            WindowEvent::CloseRequested => self.close_requested(event_loop),
            WindowEvent::Resized(size) => {
                if let Some(search) = &mut self.search {
                    search.dirty = true;
                }
                if let Some(s) = &mut self.session {
                    s.size = size;
                    s.renderer.resize(size.width, size.height);
                    s.resize.schedule(Instant::now());
                    s.window.request_redraw();
                }
                self.update_ime_cursor_area();
            }
            WindowEvent::ScaleFactorChanged { .. } => self.set_font_size(self.font_size),
            WindowEvent::Focused(focused) => {
                self.typing_pointer.show();
                let changed = std::mem::replace(&mut self.focused, focused) != focused;
                if let Some(s) = &mut self.session {
                    s.blink_since = Instant::now();
                    if !focused {
                        s.composition.clear();
                        if let Some(search) = &mut self.search {
                            search.clear_preedit();
                        }
                        s.ime_cursor_area = None;
                    }
                    if changed && s.term.modes.focus_events {
                        s.pty.write(if focused { b"\x1b[I" } else { b"\x1b[O" });
                    }
                    s.window.request_redraw();
                }
            }
            WindowEvent::Occluded(occluded) => {
                self.occluded = occluded;
                // wgpu acquires no drawable while the window is occluded, which includes a new
                // window before macOS first shows it, so draw again once it is visible.
                if !occluded && let Some(s) = &self.session {
                    s.window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
                self.refresh_link();
            }
            WindowEvent::DroppedFile(path) => self.dropped_file(path),
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                let held_at_focus =
                    self.held_at_focus
                        .observe(event.physical_key, event.state, is_synthetic);
                if event.state != ElementState::Pressed {
                    return;
                }
                if self.pending_close.is_some() {
                    self.confirm_close_key(&event, event_loop);
                    return;
                }
                if self.pending_paste.is_some() {
                    self.confirm_paste_key(&event);
                    return;
                }
                if self.shortcut(&event, held_at_focus, event_loop) {
                    return;
                }
                let find_preedit = self.search.as_ref().map(|f| f.preedit.as_str());
                if self
                    .session
                    .as_ref()
                    .is_some_and(|s| ime::holds_keys(&s.composition, find_preedit))
                {
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
                    self.hide_pointer_for_typing();
                }
            }
            WindowEvent::Ime(Ime::Commit(text)) if self.search.is_some() => {
                if self.pending_paste.is_some() || self.pending_close.is_some() {
                    // Like typed keys, committed text never reaches the shell past the banner.
                } else if let (Some(search), Some(s)) = (&mut self.search, &mut self.session) {
                    search.insert(&text, &mut s.term);
                    s.scroll.reset();
                    s.window.request_redraw();
                } else {
                    self.write_input(text.as_bytes());
                }
                self.update_ime_cursor_area();
            }
            WindowEvent::Ime(event) if self.search.is_some() => {
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
            WindowEvent::Ime(event) => {
                if let Some(s) = &mut self.session {
                    s.composition.update(&event);
                    if matches!(event, Ime::Enabled | Ime::Disabled) {
                        s.ime_cursor_area = None;
                    }
                    if matches!(&event, Ime::Preedit(text, _) if !text.is_empty()) {
                        s.term.scroll_to_bottom();
                        s.scroll.reset();
                    }
                    s.window.request_redraw();
                }
                if let Ime::Commit(text) = event
                    && self.pending_paste.is_none()
                    && self.pending_close.is_none()
                {
                    self.write_input(text.as_bytes());
                    self.hide_pointer_for_typing();
                }
                self.refresh_link();
                self.update_ime_cursor_area();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                self.typing_pointer.show();
                if !self.tab_mouse_input(state, button, event_loop) {
                    self.mouse_input(state, button);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // Hiding the pointer can report a motion event at the same position.
                if position != self.tab_pointer {
                    self.typing_pointer.show();
                }
                self.pointer_moved(position)
            }
            WindowEvent::CursorLeft { .. } => {
                if self.tab_hover.take().is_some()
                    && let Some(s) = &self.session
                {
                    s.window.request_redraw();
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.typing_pointer.show();
                self.mouse_wheel(delta)
            }
            WindowEvent::RedrawRequested => {
                self.present_frame();
            }
            _ => {}
        }
    }
}

fn main() {
    // The app menu's About/Hide/Quit items name the process; outside a bundle that is
    // the lowercase binary name.
    #[cfg(target_os = "macos")]
    objc2_foundation::NSProcessInfo::processInfo()
        .setProcessName(&objc2_foundation::NSString::from_str("Trident"));
    let startup_stderr = std::env::var_os(STARTUP_STDERR).is_some();
    // SAFETY: main has not started any threads or spawned the PTY yet.
    unsafe { std::env::remove_var(STARTUP_STDERR) };
    let working_directory = std::env::var_os(WORKING_DIRECTORY).map(PathBuf::from);
    // SAFETY: as above.
    unsafe { std::env::remove_var(WORKING_DIRECTORY) };
    if std::env::args().nth(1).as_deref() == Some("show") {
        std::process::exit(show::main(std::env::args().skip(2)));
    }
    if std::env::args().nth(1).as_deref() == Some("--install-terminfo") {
        std::process::exit(terminfo::install());
    }
    let options = match parse_args() {
        Ok(o) => Options {
            working_directory: working_directory.filter(|dir| dir.is_dir()),
            ..o
        },
        Err(e) => {
            diag!("trident: {e}");
            std::process::exit(2);
        }
    };
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(l) => l,
        Err(e) => {
            diag!("trident: cannot start event loop: {e}");
            std::process::exit(1);
        }
    };
    let mut app = App::new(options, event_loop.create_proxy());
    app.startup_stderr = startup_stderr;
    if let Err(e) = event_loop.run_app(&mut app) {
        diag!("trident: {e}");
        std::process::exit(1);
    }
    if let Some(e) = app.error {
        diag!("trident: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drawn_tab_bar_gives_way_to_native_tabs_on_macos() {
        assert!(!tab_bar_drawn(false));
        assert_eq!(tab_bar_drawn(true), !cfg!(target_os = "macos"));
    }

    #[test]
    fn link_hit_test_excludes_padding_and_leftover_space() {
        let padding = 12.0;
        let grid = (80, 24);
        let cell = (10.0, 20.0);
        for (x, y) in [(12.0, 12.0), (811.9, 491.9), (400.0, 250.0)] {
            assert!(pointer_in_grid(
                PhysicalPosition::new(x, y),
                padding,
                grid,
                cell
            ));
        }
        for (x, y) in [
            (11.9, 250.0),
            (812.0, 250.0),
            (400.0, 11.9),
            (400.0, 492.0),
            (0.0, 0.0),
            (900.0, 600.0),
            (f64::NAN, 250.0),
        ] {
            assert!(!pointer_in_grid(
                PhysicalPosition::new(x, y),
                padding,
                grid,
                cell
            ));
        }
        for grid in [(0, 24), (80, 0), (0, 0)] {
            assert!(!pointer_in_grid(
                PhysicalPosition::new(12.0, 12.0),
                padding,
                grid,
                cell
            ));
        }
    }

    #[test]
    fn input_requests_frame_only_when_view_changes() {
        let mut term = Terminal::new(20, 3);
        term.advance(b"1\r\n2\r\n3\r\n4\r\n5\r\n6");
        let mut scroll = PixelScroll::default();
        assert!(!reset_view_for_input(&mut term, &mut scroll));

        term.scroll_display(2);
        assert!(reset_view_for_input(&mut term, &mut scroll));
        assert_eq!(term.display_offset(), 0);
        assert!(!reset_view_for_input(&mut term, &mut scroll));

        scroll.offset = 0.5;
        assert!(reset_view_for_input(&mut term, &mut scroll));
        assert_eq!(scroll.offset, 0.0);

        let point = term.viewport_to_point(0, 0);
        term.start_selection(point, SelectionKind::Simple);
        assert!(reset_view_for_input(&mut term, &mut scroll));
        assert!(term.selection.is_none());
        assert!(!reset_view_for_input(&mut term, &mut scroll));
    }

    #[test]
    fn shell_shortcuts_follow_primary_screen_marks() {
        let mut term = Terminal::new(20, 3);
        assert!(!shell_shortcuts_available(&term));
        term.advance(b"\x1b]133;A\x07");
        assert!(shell_shortcuts_available(&term));
        term.advance(b"\x1b[?1049h");
        assert!(term.has_shell_marks());
        assert!(!shell_shortcuts_available(&term));
        term.advance(b"\x1b[?1049l");
        assert!(shell_shortcuts_available(&term));
    }

    #[test]
    fn diagnostics_do_not_panic_when_stderr_is_closed() {
        const CHILD: &str = "TERMINAL_TEST_CLOSED_STDERR";
        if std::env::var_os(CHILD).is_some() {
            let mut ready = [0];
            std::io::stdin().read_exact(&mut ready).unwrap();
            diag!("diagnostic to a closed pipe");
            return;
        }
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::diagnostics_do_not_panic_when_stderr_is_closed",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stderr.take());
        child.stdin.take().unwrap().write_all(b"1").unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn new_window_stderr_forwards_empty_and_large_output() {
        for input in [
            Vec::new(),
            b"startup failed\n".to_vec(),
            vec![b'x'; 100_000],
        ] {
            let mut output = Vec::new();
            forward_new_window_stderr(input.as_slice(), &mut output).unwrap();
            assert_eq!(output, input);
        }
    }

    #[test]
    fn new_window_stderr_drains_after_writer_closes() {
        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut input = std::io::Cursor::new(vec![b'x'; 100_000]);
        forward_new_window_stderr(&mut input, Closed).unwrap();
        assert_eq!(input.position(), 100_000);
    }

    #[test]
    fn new_window_stderr_retries_interrupted_reads_and_reports_errors() {
        struct Errors(bool);
        impl Read for Errors {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                let kind = if self.0 {
                    self.0 = false;
                    std::io::ErrorKind::Interrupted
                } else {
                    std::io::ErrorKind::Other
                };
                Err(kind.into())
            }
        }
        assert_eq!(
            forward_new_window_stderr(Errors(true), Vec::new())
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::Other
        );
    }

    #[test]
    fn new_windows_start_in_local_osc7_directories() {
        let dir = std::env::temp_dir();
        let cwd = |host: &str, path: &std::path::Path| term_core::WorkingDirectory {
            host: host.into(),
            path: path.to_str().unwrap().into(),
        };
        let here = super::hostname().unwrap();
        for host in ["", "localhost", "LOCALHOST", &here, &here.to_uppercase()] {
            assert_eq!(
                super::local_working_directory(Some(&cwd(host, &dir))).as_deref(),
                Some(dir.as_path()),
                "{host:?}"
            );
        }
        assert_eq!(super::local_working_directory(None), None);
        let missing = dir.join("terminal-osc7-missing-dir");
        assert_eq!(
            super::local_working_directory(Some(&cwd("", &missing))),
            None
        );
        assert_eq!(
            super::local_working_directory(Some(&cwd("some-other-host.example", &dir))),
            None
        );
        assert!(super::same_host("mac.local", "MAC"));
        assert!(!super::same_host("mac.local", "box.local"));
        assert!(!super::same_host("dev.office.example", "dev.cloud.example"));
    }

    #[test]
    fn bash_integration_reports_non_ascii_directories() {
        let dir =
            std::env::temp_dir().join(format!("terminal-osc7 \u{e9}%x-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../shell-integration/terminal.bash"
        );
        let output = std::process::Command::new("bash")
            .args([
                "--norc",
                "-ic",
                r#"source "$1"; _terminal_report_cwd"#,
                "bash",
                script,
            ])
            .current_dir(&dir)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .unwrap();
        let mut terminal = term_core::Terminal::new(10, 2);
        terminal.advance(&output.stdout);
        let reported = terminal
            .working_directory()
            .and_then(|cwd| std::fs::canonicalize(&cwd.path).ok());
        let expected = dir.canonicalize().ok();
        std::fs::remove_dir(&dir).unwrap();
        assert!(
            reported.is_some() && reported == expected,
            "{:?}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    #[test]
    fn new_window_stderr_inherits_only_files_and_character_devices() {
        use std::os::unix::net::UnixStream;
        let file = File::open(std::env::current_exe().unwrap()).unwrap();
        let null = File::open("/dev/null").unwrap();
        let directory = File::open(".").unwrap();
        let (_, pipe) = std::io::pipe().unwrap();
        let (socket, _) = UnixStream::pair().unwrap();
        for (fd, inherit, pipe) in [
            (file.as_fd(), true, false),
            (null.as_fd(), true, false),
            (directory.as_fd(), false, false),
            (pipe.as_fd(), false, true),
            (socket.as_fd(), false, true),
        ] {
            let kind = File::from(fd.try_clone_to_owned().unwrap())
                .metadata()
                .unwrap()
                .file_type();
            assert_eq!(inherit_new_window_stderr(Some(kind)), inherit);
            assert_eq!(pipe_new_window_stderr(Some(kind)), pipe);
        }
        assert!(!inherit_new_window_stderr(None));
        assert!(!pipe_new_window_stderr(None));
    }

    #[test]
    fn keys_held_at_focus_stay_guarded_until_released() {
        use winit::keyboard::KeyCode;
        let n = PhysicalKey::Code(KeyCode::KeyN);
        let other = PhysicalKey::Code(KeyCode::ArrowUp);
        let mut held = HeldAtFocus::default();
        assert!(!held.observe(n, ElementState::Pressed, false));
        assert!(!held.observe(n, ElementState::Released, false));
        assert!(held.observe(n, ElementState::Pressed, true));
        assert!(held.observe(n, ElementState::Pressed, false));
        assert!(!held.observe(other, ElementState::Released, false));
        assert!(held.observe(n, ElementState::Pressed, false));
        assert!(!held.observe(n, ElementState::Released, false));
        assert!(!held.observe(n, ElementState::Pressed, false));
        assert!(held.observe(n, ElementState::Pressed, true));
        assert!(!held.observe(n, ElementState::Released, true));
        assert!(!held.observe(n, ElementState::Pressed, false));
    }

    #[test]
    fn tab_helpers() {
        assert_eq!(neighbor_tab(0, 3), 1);
        assert_eq!(neighbor_tab(1, 3), 2);
        assert_eq!(neighbor_tab(2, 3), 1);
        assert_eq!(neighbor_tab(0, 1), 0);
        assert_eq!(shortcut_tab(2, 5), 2);
        assert_eq!(shortcut_tab(8, 5), 4);
        assert_eq!(shortcut_tab(8, 12), 11);
        assert_eq!(tab_title(""), "Terminal");
        assert_eq!(tab_title("  "), "Terminal");
        assert_eq!(tab_title("vim"), "vim");
        assert_eq!(
            below_tab_bar(PhysicalSize::new(800, 600), 30),
            PhysicalSize::new(800, 570)
        );
        assert_eq!(
            below_tab_bar(PhysicalSize::new(800, 20), 30),
            PhysicalSize::new(800, 0)
        );
    }

    #[test]
    fn only_new_window_suppresses_shortcut_repeats() {
        for action in [
            Action::NewWindow,
            Action::NewTab,
            Action::CloseTab,
            Action::NextTab,
            Action::SelectTab(0),
            Action::LineUp,
            Action::LineDown,
            Action::Increase,
            Action::Decrease,
            Action::Copy,
            Action::ToggleFullscreen,
            Action::None,
        ] {
            assert!(!suppress_shortcut_repeat(&action, false, false));
            for (repeat, synthetic) in [(true, false), (false, true), (true, true)] {
                assert_eq!(
                    suppress_shortcut_repeat(&action, repeat, synthetic),
                    matches!(
                        action,
                        Action::NewWindow
                            | Action::NewTab
                            | Action::CloseTab
                            | Action::ToggleFullscreen
                    )
                );
            }
        }
    }

    #[test]
    fn toggle_fullscreen_goes_borderless_and_back() {
        assert!(matches!(
            toggled_fullscreen(None),
            Some(Fullscreen::Borderless(None))
        ));
        assert!(toggled_fullscreen(Some(Fullscreen::Borderless(None))).is_none());
    }

    #[test]
    fn line_and_edge_scrolls_pass_through_on_alt_screen() {
        for action in [
            Action::LineUp,
            Action::LineDown,
            Action::Top,
            Action::Bottom,
        ] {
            assert!(passes_through_on_alt_screen(&action));
        }
        for action in [Action::PageUp, Action::PageDown, Action::NewWindow] {
            assert!(!passes_through_on_alt_screen(&action));
        }
    }

    #[test]
    fn window_and_line_scroll_defaults_dispatch() {
        use winit::keyboard::NamedKey;
        for macos in [false, true] {
            let bindings = config::default_bindings_for(macos);
            let command = if macos {
                ModifiersState::SUPER
            } else {
                ModifiersState::CONTROL | ModifiersState::SHIFT
            };
            for (key, mods, action) in [
                (Key::Character("n".into()), command, Action::NewWindow),
                (
                    Key::Named(NamedKey::ArrowUp),
                    command | ModifiersState::ALT,
                    Action::LineUp,
                ),
                (
                    Key::Named(NamedKey::ArrowDown),
                    command | ModifiersState::ALT,
                    Action::LineDown,
                ),
            ] {
                assert_eq!(
                    bindings.get(&shortcut_chord(key, mods).unwrap()),
                    Some(&action)
                );
            }
        }
    }

    #[test]
    fn preedit_hides_the_cells_it_covers_from_link_hover() {
        let preedit = term_render::Preedit {
            text: "日本",
            cursor: None,
        };
        let layout = preedit.layout(3, 10);
        assert!(cell_in_preedit(&layout, 2, (2, 3)));
        assert!(cell_in_preedit(&layout, 2, (2, 6)));
        assert!(!cell_in_preedit(&layout, 2, (2, 2)));
        assert!(!cell_in_preedit(&layout, 2, (2, 7)));
        assert!(!cell_in_preedit(&layout, 2, (1, 4)));
        assert!(!cell_in_preedit(&layout, 2, (-1, 4)));
    }

    #[test]
    fn find_bar_rows_counts_rows_it_overlaps() {
        // 6 px padding, 20 px cells: 10 rows end at 206, the bar starts at 200 and clips row 9.
        assert_eq!(find_bar_rows(220.0, 6.0, 20.0, 10), 1);
        // A full spare row below the grid leaves every row clear.
        assert_eq!(find_bar_rows(240.0, 6.0, 20.0, 10), 0);
        // Row 8 ends exactly at the bar's top edge, so only row 9 is covered.
        assert_eq!(find_bar_rows(206.0, 6.0, 20.0, 10), 1);
        assert_eq!(find_bar_rows(10.0, 6.0, 20.0, 1), 1);
    }

    #[test]
    fn search_bar_hit_test_uses_visible_physical_bounds() {
        let size = PhysicalSize::new(800, 600);
        let cell = (10.0, 20.0);
        for (x, y) in [(0.0, 580.0), (799.9, 599.9), (400.0, 590.0)] {
            let pos = PhysicalPosition::new(x, y);
            assert!(pointer_in_search_bar(true, pos, size, cell));
            assert!(!pointer_in_search_bar(false, pos, size, cell));
        }
        for (x, y) in [
            (-0.1, 590.0),
            (800.0, 590.0),
            (400.0, 579.9),
            (400.0, 600.0),
        ] {
            assert!(!pointer_in_search_bar(
                true,
                PhysicalPosition::new(x, y),
                size,
                cell,
            ));
        }
        assert!(pointer_in_search_bar(
            true,
            PhysicalPosition::new(0.0, 0.0),
            PhysicalSize::new(1, 1),
            cell,
        ));
        assert!(!pointer_in_search_bar(
            true,
            PhysicalPosition::new(0.0, 0.0),
            PhysicalSize::new(0, 0),
            cell,
        ));
    }

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
            let shifted = shortcut_chord(
                Key::Named(key),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
            )
            .unwrap();
            assert_eq!(
                config.bindings.get(&shifted),
                config::default_bindings().get(&shifted)
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
            working_directory: None,
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

    fn batch(bytes: &[u8]) -> pty::Output {
        pty::Output {
            bytes: bytes.to_vec(),
            shell_returns: Vec::new(),
        }
    }

    #[test]
    fn exit_drains_many_small_frames_with_shell_returns() {
        let (mut term, mut output, queued) = exit_frame_batch(64_000);
        let bytes = queued.bytes.len();
        assert!(bytes >= 1 << 20);
        assert_eq!(output.drain_on_exit(&mut term, queued), bytes);
        let text: String = term
            .viewport_row(0)
            .cells
            .iter()
            .map(|cell| cell.c)
            .collect();
        assert!(text.starts_with("final"));
        assert!(!term.modes.synchronized_output);
        assert_eq!(term.modes.mouse, MouseMode::Off);
        assert!(!term.modes.focus_events);
        assert!(output.is_empty());
    }

    fn exit_frame_batch(frames: usize) -> (Terminal, PendingOutput, pty::Output) {
        let mut bytes = Vec::new();
        let mut shell_returns = Vec::new();
        for _ in 0..frames {
            bytes.extend_from_slice(b"\x1b[?2026h\rx\x1b[?2026l");
            shell_returns.push(bytes.len());
        }
        bytes.extend_from_slice(b"\x1b[?1000;1004h");
        shell_returns.push(bytes.len());
        bytes.extend_from_slice(b"\x1b[?2026h\rfinal");
        (
            Terminal::new(80, 2),
            PendingOutput::default(),
            pty::Output {
                bytes,
                shell_returns,
            },
        )
    }

    #[test]
    #[ignore]
    fn time_exit_frame_drain() {
        for frames in [64_000, 512_000] {
            let (mut term, mut output, queued) = exit_frame_batch(frames);
            let bytes = queued.bytes.len();
            let start = Instant::now();
            output.drain_on_exit(&mut term, queued);
            diag!(
                "EXIT_DRAIN_TIMING frames={frames} bytes={bytes} elapsed_ms={:.3}",
                start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    #[test]
    fn exit_drains_open_frame_and_waits_for_successful_presentation() {
        let mut term = Terminal::new(80, 2);
        let mut output = PendingOutput::default();
        output.advance(&mut term, false, || batch(b"\x1b[?2026h"));
        let marker = b"EXIT-DRAIN: final open-frame output";
        assert_eq!(output.drain_on_exit(&mut term, batch(marker)), marker.len());
        assert!(!term.modes.synchronized_output);
        let text: String = term
            .viewport_row(0)
            .cells
            .iter()
            .map(|cell| cell.c)
            .collect();
        assert!(text.starts_with(std::str::from_utf8(marker).unwrap()));
        assert!(output.is_empty());
        let now = Instant::now();
        assert!(!output.finish_presentation(false, now));
        assert_eq!(output.waiting_since, Some(now));
        assert!(output.finish_presentation(true, now));
        assert!(output.waiting_since.is_none());
    }

    #[test]
    fn exit_drains_leftovers_before_queued_frames_even_when_waiting() {
        let mut term = Terminal::new(80, 2);
        let mut output = PendingOutput::default();
        output.advance(&mut term, false, || {
            batch(b"\x1b[?2026hfirst\x1b[?2026l\x1b[?2026hsecond")
        });
        assert!(output.waiting_since.is_some());
        output.drain_on_exit(
            &mut term,
            pty::Output {
                bytes: b"\x1b[?2026l\x1b[?2026hthird".to_vec(),
                shell_returns: vec![8],
            },
        );
        let text: String = term
            .viewport_row(0)
            .cells
            .iter()
            .map(|cell| cell.c)
            .collect();
        assert!(text.starts_with("firstsecondthird"));
        assert!(output.is_empty());
        assert!(!term.modes.synchronized_output);
        assert!(output.waiting_since.is_some());
    }

    #[test]
    fn metal_exit_without_real_feedback_requires_two_drawables() {
        let now = Instant::now();
        let mut state = ExitPresentation::default();
        assert!(!state.update_metal(0, [0, 1], now, 60_000));
        assert!(!state.ready(now));
        state.target = Some(2);
        assert!(!state.update_metal(0, [0, 1], now, 60_000));
        assert!(!state.ready(now));
        assert!(
            state.update_metal(0, [1, 2], now, 60_000),
            "one untimed drawable is not a successful final present"
        );
        assert_eq!(state.target, None);
        assert!(!state.ready(now + EXIT_TIMEOUT));
        state.target = Some(3);
        assert!(!state.update_metal(0, [1, 2], now, 60_000));
        assert!(!state.ready(now + EXIT_TIMEOUT));
        let second = now + Duration::from_millis(16);
        assert!(!state.update_metal(0, [2, 3], second, 60_000));
        let until = state.wakeup(second);
        assert_eq!(until, second);
        assert!(!state.ready(until - Duration::from_nanos(1)));
        assert!(state.ready(until));
        assert!(until < now + EXIT_TIMEOUT);
        let deadline = Some(now + EXIT_TIMEOUT);
        assert!(exit_ready(deadline, state.ready(second), second));
        assert!(exit_ready(deadline, state.ready(until), until));
    }

    #[test]
    fn metal_exit_switches_to_real_feedback_after_untimed_retry() {
        let now = Instant::now();
        let mut state = ExitPresentation {
            target: Some(2),
            ..ExitPresentation::default()
        };
        assert!(state.update_metal(0, [1, 2], now, 60_000));
        state.target = Some(3);
        assert!(!state.update_metal(3, [1, 2], now, 60_000));
        assert!(state.ready(now));
    }

    #[test]
    fn metal_exit_missing_second_callback_finishes_after_one_refresh() {
        let start = Instant::now();
        let period = Duration::from_secs_f64(1.0 / 60.0);
        let mut state = ExitPresentation::default();
        state.submitted(2, start);
        assert!(state.update_metal(0, [1, 2], start + period, 60_000));
        state.submitted(3, start + period);
        assert!(!state.update_metal(0, [1, 2], start + period * 2, 60_000));
        assert!(!state.missing_feedback(start + period * 2, 60_000));
        assert!(state.ready(start + period * 2));
        assert!(state.ready(state.wakeup(start + period * 2)));
        assert!(state.wakeup(start + period * 2) < start + EXIT_TIMEOUT);
    }

    #[test]
    fn metal_exit_proactive_retry_needs_second_drawable_feedback() {
        let start = Instant::now();
        let mut state = ExitPresentation::default();
        assert!(state.needs_metal_submission(0));
        state.submitted(2, start);
        assert!(state.needs_metal_submission(0));
        assert!(!state.needs_metal_submission(1));
        state.submitted(3, start + Duration::from_millis(1));
        assert!(!state.needs_metal_submission(0));
        assert!(!state.update_metal(0, [1, 2], start + Duration::from_millis(16), 60_000));
        assert!(!state.ready(start + EXIT_TIMEOUT));
        let second = start + Duration::from_millis(32);
        assert!(!state.update_metal(0, [2, 3], second, 60_000));
        assert!(state.ready(second));
    }

    #[test]
    fn metal_exit_proactive_retry_cannot_finish_from_one_untimed_callback() {
        let start = Instant::now();
        let mut state = ExitPresentation::default();
        state.submitted(2, start);
        state.submitted(3, start);
        assert!(!state.update_metal(0, [1, 3], start, 60_000));
        assert!(!state.ready(start));
        assert!(!state.update_metal(0, [1, 3], start, 60_000));
        assert!(!state.ready(start));
        assert!(!state.update_metal(0, [2, 3], start, 60_000));
        assert!(state.ready(start));
    }

    #[test]
    fn metal_exit_with_real_feedback_retries_skip_without_extra_delay() {
        let now = Instant::now();
        let mut state = ExitPresentation {
            target: Some(2),
            ..ExitPresentation::default()
        };
        assert!(state.update_metal(1, [1, 2], now, 60_000));
        assert_eq!(state.target, None);
        assert!(!state.ready(now));
        state.target = Some(3);
        assert!(!state.update_metal(3, [1, 2], now, 60_000));
        assert!(state.ready(now));
    }

    #[test]
    fn metal_exit_cap_applies_without_submission_or_after_unrendered_retry() {
        let now = Instant::now();
        for presented in [0, 1] {
            let mut state = ExitPresentation::default();
            if presented != 0 {
                state.submitted(2, now);
                assert!(state.update_metal(presented, [1, 2], now, 60_000));
            }
            let deadline = Some(now + EXIT_TIMEOUT);
            assert!(!exit_ready(deadline, state.ready(now), now));
            assert!(exit_ready(deadline, state.ready(now), now + EXIT_TIMEOUT));
        }
    }

    #[test]
    fn exit_missing_feedback_requires_two_submissions_and_refreshes() {
        let start = Instant::now();
        let period = Duration::from_secs_f64(1.0 / 60.0);
        let mut state = ExitPresentation::default();
        state.submitted(2, start);
        assert!(!state.missing_feedback(start + period - Duration::from_nanos(1), 60_000));
        assert!(state.missing_feedback(start + period, 60_000));
        assert_eq!(state.target, None);
        assert!(!state.ready(start + period));

        let second = start + period;
        state.submitted(3, second);
        assert!(!state.missing_feedback(second + period - Duration::from_nanos(1), 60_000));
        assert!(!state.missing_feedback(second + period, 60_000));
        assert!(state.ready(second + period));
        let until = second + period;
        assert_eq!(state.wakeup(second + period), until);
        assert!(state.ready(until));
        assert!(until < start + EXIT_TIMEOUT);
    }

    #[test]
    fn wayland_exit_submits_retry_without_waiting_a_refresh() {
        let start = Instant::now();
        let mut state = ExitPresentation::default();
        assert!(state.needs_wayland_submission());
        state.submitted(2, start);
        assert!(state.needs_wayland_submission());
        assert!(!state.ready(start));
        state.submitted(3, start);
        assert!(!state.needs_wayland_submission());
        assert!(!state.update(0, 0, start, 60_000));
        assert!(!state.ready(start));
        assert!(!state.update(0, 0, start + Duration::from_millis(17), 60_000));
        assert!(state.ready(start + Duration::from_millis(17)));
        assert!(state.update(0, 3, start + Duration::from_millis(17), 60_000));
        assert!(state.needs_wayland_submission());
        assert!(!state.ready(start + Duration::from_millis(17)));
    }

    #[test]
    fn queued_completed_frames_replace_an_unsubmitted_frame() {
        let mut term = Terminal::new(10, 2);
        let mut output = PendingOutput::default();
        output.advance(&mut term, false, || batch(b"\x1b[?2026hOLD\x1b[?2026l"));
        let mut queued = Vec::new();
        for n in 0..100 {
            queued.extend_from_slice(format!("\x1b[?2026h\r{n:03}\x1b[?2026l").as_bytes());
        }
        queued.extend_from_slice(b"\x1b[?2026h\rHALF");
        assert!(output.coalesce(batch(&queued)));
        output.advance(&mut term, false, || panic!("coalesced output first"));
        let text: String = term.viewport_row(0).cells[..3]
            .iter()
            .map(|c| c.c)
            .collect();
        assert_eq!(text, "099");
        assert!(!term.modes.synchronized_output);
        assert_eq!(output.bytes, b"\x1b[?2026h\rHALF");
        assert!(output.waiting_since.is_some());
        assert!(!output.finish_presentation(false, Instant::now()));
        assert_eq!(output.bytes, b"\x1b[?2026h\rHALF");
        assert!(output.finish_presentation(true, Instant::now()));
    }

    #[test]
    fn incomplete_queued_frame_preserves_the_pending_presentation() {
        let mut term = Terminal::new(10, 2);
        let mut output = PendingOutput::default();
        output.advance(&mut term, false, || batch(b"\x1b[?2026hOLD\x1b[?2026l"));
        let waiting = output.waiting_since;
        assert!(!output.coalesce(batch(b"")));
        assert_eq!(output.waiting_since, waiting);
        assert!(!output.coalesce(batch(b"\x1b[?2026h\rNEW\x1b[?202")));
        assert_eq!(output.waiting_since, waiting);
        assert_eq!(term.viewport_row(0).cells[0].c, 'O');
        assert!(output.coalesce(batch(b"6l")));
        output.advance(&mut term, false, || panic!("retained split reset first"));
        assert_eq!(term.viewport_row(0).cells[0].c, 'N');
        assert!(!term.modes.synchronized_output);
        assert!(output.waiting_since.is_some());
    }

    #[test]
    fn coalesced_shell_returns_follow_retained_output() {
        let mut term = Terminal::new(10, 2);
        let mut output = PendingOutput::default();
        output.advance(&mut term, false, || {
            batch(b"\x1b[?2026hOLD\x1b[?2026l\x1b[?2026h\x1b[?1000;2004h\rNEW")
        });
        assert!(!output.coalesce(pty::Output {
            bytes: Vec::new(),
            shell_returns: vec![0],
        }));
        assert_eq!(output.shell_returns, [output.bytes.len()]);
        assert!(output.coalesce(batch(b"\x1b[?2026l")));
        output.advance(&mut term, false, || panic!("coalesced output first"));
        assert_eq!(term.viewport_row(0).cells[0].c, 'N');
        assert_eq!(term.modes.mouse, MouseMode::Off);
        assert!(!term.modes.bracketed_paste);
        assert!(!term.modes.synchronized_output);
        assert!(output.is_empty());
    }

    #[test]
    fn ordinary_output_does_not_drain_after_presentation() {
        let mut term = Terminal::new(80, 2);
        let mut output = PendingOutput::default();
        for bytes in [b"".as_slice(), b"x", b"\x1b[31mred", b"\x1b[?2026l"] {
            output.advance(&mut term, false, || batch(bytes));
            assert!(output.waiting_since.is_none());
            assert!(!output.pace_frame(term.modes.synchronized_output));
            assert!(!output.finish_presentation(false, Instant::now()));
            assert!(!output.finish_presentation(true, Instant::now()));
        }
        output.advance(&mut term, false, || batch(b"\x1b[?2026hframe"));
        assert!(term.modes.synchronized_output);
        assert!(output.pace_frame(term.modes.synchronized_output));
        output.advance(&mut term, false, || batch(b"\x1b[?2026l"));
        assert!(output.waiting_since.is_some());
        assert!(output.pace_frame(term.modes.synchronized_output));
        assert!(!output.finish_presentation(false, Instant::now()));
        assert!(output.waiting_since.is_some());
        assert!(output.finish_presentation(true, Instant::now()));
        assert!(output.waiting_since.is_none());
        assert!(!output.pace_frame(term.modes.synchronized_output));
    }

    #[test]
    fn late_discard_retries_even_after_missing_feedback_fallback() {
        let start = Instant::now();
        let period = Duration::from_secs_f64(1.0 / 60.0);
        let mut state = ExitPresentation::default();
        state.submitted(2, start);
        assert!(state.missing_feedback(start + period, 60_000));
        state.submitted(3, start + period);
        assert!(!state.missing_feedback(start + period * 2, 60_000));
        assert!(state.update(0, 3, start + period * 2, 60_000));
        assert_eq!(state.target, None);
        assert!(!state.ready(start + period * 4));
        state.submitted(4, start + period * 3);
        assert!(!state.update(4, 3, start + period * 4, 60_000));
        assert!(state.ready(state.wakeup(start + period * 4)));
    }

    #[test]
    fn late_final_feedback_finishes_without_waiting_for_redundant_retry() {
        let start = Instant::now();
        let period = Duration::from_secs_f64(1.0 / 60.0);
        let mut state = ExitPresentation::default();
        state.submitted(4, start);
        assert!(state.missing_feedback(start + period, 60_000));
        state.submitted(5, start + period);
        assert!(!state.update(3, 0, start + period, 60_000));
        assert!(!state.ready(start + period));
        assert!(!state.update(4, 0, start + period, 60_000));
        assert!(state.ready(start + period));
    }

    #[test]
    fn exit_redraws_wait_until_presentation_requests_a_retry() {
        let start = Instant::now();
        let mut state = ExitPresentation::default();
        assert!(state.needs_submission());
        state.submitted(4, start);
        assert!(!state.needs_submission());
        assert!(!state.update(3, 0, start, 60_000));
        assert!(!state.needs_submission());
        assert!(state.update(3, 4, start, 60_000));
        assert!(state.needs_submission());
        state.submitted(5, start);
        assert!(!state.needs_submission());
        assert!(!state.update(5, 4, start, 60_000));
        assert!(state.ready(start));
        assert!(!state.needs_submission());
    }

    #[test]
    fn exit_presentation_requires_target_feedback_without_extra_delay() {
        let now = Instant::now();
        let mut state = ExitPresentation::default();
        assert!(!state.update(10, 10, now, 60_000));
        assert!(!state.ready(now));
        assert_eq!(state.wakeup(now), now + Duration::from_millis(4));
        state.target = Some(11);
        assert!(!state.update(10, 10, now, 60_000));
        assert!(!state.ready(now));
        assert!(!state.update(11, 10, now, 60_000));
        let until = state.wakeup(now);
        assert!(!state.ready(until - Duration::from_nanos(1)));
        assert!(state.ready(until));
        assert_eq!(until, now);
        assert!(!state.update(12, 12, until, 60_000));
        assert_eq!(state.wakeup(until), until);
    }

    #[test]
    fn exit_presentation_retries_skips_and_timeout_wins() {
        let now = Instant::now();
        let mut state = ExitPresentation {
            target: Some(2),
            ..ExitPresentation::default()
        };
        assert!(state.update(1, 2, now, 0));
        assert_eq!(state.target, None);
        assert!(!state.ready(now));
        assert!(exit_ready(
            Some(now + EXIT_TIMEOUT),
            state.ready(now),
            now + EXIT_TIMEOUT
        ));
        state.target = Some(3);
        assert!(!state.update(3, 2, now, 0));
        assert!(state.ready(now));
        let mut fast = ExitPresentation {
            target: Some(1),
            ..ExitPresentation::default()
        };
        fast.update(1, 0, now, 120_000);
        assert!(fast.ready(now));
    }

    #[test]
    fn exit_cap_survives_missing_redraws_and_failed_presentations() {
        let start = Instant::now();
        let deadline = Some(start + EXIT_TIMEOUT);
        let before = start + EXIT_TIMEOUT - Duration::from_nanos(1);
        assert!(!exit_ready(None, true, start));
        assert!(exit_ready(deadline, true, start));
        assert!(!exit_ready(deadline, false, before));
        assert!(exit_ready(deadline, false, start + EXIT_TIMEOUT));
        let mut pending = PendingOutput {
            waiting_since: Some(start),
            ..PendingOutput::default()
        };
        assert!(!pending.finish_presentation(false, before));
        assert_eq!(pending.waiting_since, Some(before));
        assert!(exit_ready(deadline, false, start + EXIT_TIMEOUT));
        assert_eq!(exit_wakeup(None, deadline), deadline);
        assert_eq!(
            exit_wakeup(Some(start + EXIT_TIMEOUT * 2), deadline),
            deadline
        );
        assert_eq!(exit_wakeup(Some(before), deadline), Some(before));
        assert_eq!(exit_wakeup(Some(before), None), Some(before));
        assert_eq!(exit_wakeup(None, None), None);
    }

    #[test]
    fn exit_without_output_still_requires_a_final_presentation() {
        let mut term = Terminal::new(80, 2);
        let mut output = PendingOutput::default();
        assert_eq!(output.drain_on_exit(&mut term, batch(b"")), 0);
        assert!(output.is_empty());
        assert!(output.waiting_since.is_some());
        assert!(output.finish_presentation(true, Instant::now()));
        assert!(output.waiting_since.is_none());
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
    fn configure_term_applies_compare_runs_to_every_terminal() {
        let mut config = config::Config::default();
        let mut term = Terminal::new(80, 24);
        configure_term(&mut term, &config, 100);
        assert!(term.compare_runs());
        assert_eq!(term.compare_ignore(), &config.compare_ignore);

        config.compare_runs = false;
        config.compare_ignore = term_core::Ignore::new(&["build #\\d+"]).unwrap();
        let mut new_tab = Terminal::new(80, 24);
        configure_term(&mut new_tab, &config, 100);
        configure_term(&mut term, &config, 100);
        for term in [&new_tab, &term] {
            assert!(!term.compare_runs());
            assert_eq!(term.compare_ignore(), &config.compare_ignore);
        }

        config.compare_runs = true;
        configure_term(&mut term, &config, 100);
        assert!(term.compare_runs());
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
    fn recycling_freed_memory_preserves_fraction() {
        let mut term = Terminal::new(1000, 5);
        term.set_scrollback_limit(100);
        for _ in 0..105 {
            term.advance(b"wide\r\n");
        }
        term.resize(80, 5);
        assert!(release_freed_memory(&mut term));
        term.advance(b"narrow\r\n");
        assert!(term.take_recycled_memory_freed());
        let mut scroll = PixelScroll { offset: 9.0 };
        update_terminal(&mut term, &mut scroll, |term| {
            term.advance(b"narrow\r\n");
        });
        assert_eq!(scroll.offset, 9.0);
        assert!(!term.take_recycled_memory_freed());
        assert!(!release_freed_memory(&mut term));
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
    fn cursor_area_follows_cell_and_stays_inside_cached_size() {
        let size = PhysicalSize::new(800, 600);
        assert_eq!(
            cursor_area(size, 4.0, (2, 3), 0.0, (10.0, 20.0)),
            [34.0, 44.0, 10.0, 20.0]
        );
        assert_eq!(
            cursor_area(size, 4.0, (2, 3), 6.5, (10.0, 20.0)),
            [34.0, 50.5, 10.0, 20.0]
        );
        assert_eq!(
            cursor_area(size, 4.0, (100, 200), 0.0, (10.0, 20.0)),
            [790.0, 580.0, 10.0, 20.0]
        );
        assert_eq!(
            cursor_area(PhysicalSize::new(5, 5), 4.0, (0, 0), 0.0, (10.0, 20.0)),
            [0.0, 0.0, 10.0, 20.0]
        );
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
            &mut pty::Output {
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
            &mut pty::Output {
                bytes: Vec::new(),
                shell_returns: Vec::new(),
            },
        );
        assert!(term.modes.bracketed_paste);
    }

    #[test]
    fn leftover_does_not_acknowledge_queued_pty_output() {
        let mut output = PendingOutput::default();
        let mut term = Terminal::new(10, 2);
        let mut first = batch(b"\x1b[?2026h028\x1b[?2026l\x1b[?2026h\x1b[?1000;1004;2004h");
        let consumed = advance_output(&mut term, &mut first);
        output.retain(
            first,
            consumed,
            term.synchronized_output_ended(),
            Instant::now(),
        );
        let wake_pending = std::sync::atomic::AtomicBool::new(false);
        let mut queued = Some(pty::Output {
            bytes: b"\r099\x1b[?2026l".to_vec(),
            shell_returns: vec![0],
        });
        assert!(!wake_pending.swap(true, Ordering::AcqRel));
        output.presented();
        let mut next = output.next_batch(|| panic!("leftover must precede PTY output"));
        advance_output(&mut term, &mut next);
        assert!(term.modes.synchronized_output);
        assert!(wake_pending.load(Ordering::Acquire));
        let mut next = output.next_batch(|| {
            wake_pending.store(false, Ordering::Release);
            queued.take().unwrap()
        });
        advance_output(&mut term, &mut next);
        assert!(!term.modes.synchronized_output);
        assert_eq!(term.viewport_row(0).cells[0].c, '0');
        assert_eq!(term.viewport_row(0).cells[1].c, '9');
        assert_eq!(term.viewport_row(0).cells[2].c, '9');
        assert!(queued.is_none());
        assert_eq!(term.modes.mouse, MouseMode::Off);
        assert!(!term.modes.focus_events);
        assert!(!term.modes.bracketed_paste);
        assert!(!wake_pending.load(Ordering::Acquire));
    }

    #[test]
    fn leftover_output_waits_for_presentation_and_preserves_order() {
        let mut output = PendingOutput::default();
        let mut term = Terminal::new(10, 2);
        let mut first =
            batch(b"\x1b[?2026hOLD\x1b[?2026l\x1b[?2026h\rNEW\x1b[?2026l\x1b[?2026h\rHAL");
        let consumed = advance_output(&mut term, &mut first);
        output.retain(
            first,
            consumed,
            term.synchronized_output_ended(),
            Instant::now(),
        );
        assert!(output.waiting_since.is_some());
        let text = |term: &Terminal| {
            term.viewport_row(0)
                .cells
                .iter()
                .take(3)
                .map(|c| c.c)
                .collect::<String>()
        };
        assert_eq!(text(&term), "NEW");
        assert!(!term.modes.synchronized_output);
        assert!(output.presented());
        assert!(output.waiting_since.is_none());
        let mut next = output.next_batch(|| panic!("leftover must precede PTY output"));
        let consumed = advance_output(&mut term, &mut next);
        output.retain(
            next,
            consumed,
            term.synchronized_output_ended(),
            Instant::now(),
        );
        assert!(term.modes.synchronized_output);
        assert!(!output.presented());
        assert!(output.waiting_since.is_none());
    }

    #[test]
    fn last_frame_end_is_searched_only_after_a_frame_ends() {
        let mut term = Terminal::new(10, 2);
        let open = b"plain\r\n\x1b[?2026hA";
        let mut last_reset = None;
        assert_eq!(
            advance_frames(&mut term, open, 0, open.len(), &mut last_reset),
            open.len()
        );
        assert_eq!(last_reset, None);
        assert!(term.modes.synchronized_output);
        let backlog = b"B\x1b[?2026l\x1b[?2026hC\x1b[?2026l\x1b[?2026hD";
        let consumed = advance_frames(&mut term, backlog, 0, backlog.len(), &mut last_reset);
        assert_eq!(consumed, backlog.len() - b"\x1b[?2026hD".len());
        assert_eq!(last_reset, Some(consumed));
        assert!(term.synchronized_output_ended());
    }

    #[test]
    fn complete_frame_backlog_is_applied_in_one_advance() {
        let mut term = Terminal::new(10, 2);
        let mut bytes = Vec::new();
        for n in 0..100 {
            bytes.extend_from_slice(format!("\x1b[?2026h\r{n:03}\x1b[?2026l").as_bytes());
        }
        let mut output = batch(&bytes);
        assert_eq!(advance_output(&mut term, &mut output), bytes.len());
        assert!(term.synchronized_output_ended());
        assert_eq!(term.viewport_row(0).cells[2].c, '9');
        assert_eq!(term.viewport_row(0).cells[1].c, '9');
    }

    #[test]
    fn retained_frame_end_waits_before_queued_unfinished_frame() {
        let mut term = Terminal::new(10, 2);
        let mut pending = PendingOutput::default();
        let mut first = batch(b"\x1b[?2026hOLD\x1b[?2026l\x1b[?2026h\rDONE\x1b[?2026l");
        let consumed = advance_output(&mut term, &mut first);
        assert_eq!(consumed, first.bytes.len());
        assert!(term.synchronized_output_ended());
        pending.retain(
            first,
            consumed,
            term.synchronized_output_ended(),
            Instant::now(),
        );
        assert!(
            pending.waiting_since.is_some(),
            "completed frame must wait for presentation"
        );
        assert_eq!(term.viewport_row(0).cells[0].c, 'D');
        assert!(!term.modes.synchronized_output);
        let wake_pending = std::sync::atomic::AtomicBool::new(true);
        let mut queued = Some(batch(b"\x1b[?2026h\rHALF"));
        assert!(wake_pending.load(Ordering::Acquire));
        assert!(queued.is_some());
        pending.presented();
        let mut next = pending.next_batch(|| {
            wake_pending.store(false, Ordering::Release);
            queued.take().unwrap()
        });
        advance_output(&mut term, &mut next);
        assert!(term.modes.synchronized_output);
        assert!(!wake_pending.load(Ordering::Acquire));
        assert!(queued.is_none());
    }

    #[test]
    fn paused_retained_frames_resume_before_queued_output() {
        let mut term = Terminal::new(10, 2);
        let mut pending = PendingOutput::default();
        let mut first = batch(b"\x1b[?2026hOLD\x1b[?2026l\x1b[?2026h\rNEW");
        let consumed = advance_output(&mut term, &mut first);
        pending.retain(first, consumed, true, Instant::now());
        pending.presented();
        let saved = pending.bytes.clone();
        let wake_pending = std::sync::atomic::AtomicBool::new(true);
        let mut queued = Some(batch(b"\rEND\x1b[?2026l"));
        for _ in 0..3 {
            assert!(
                pending
                    .advance(&mut term, true, || panic!("paused PTY drain"))
                    .is_none()
            );
            assert_eq!(term.viewport_row(0).cells[0].c, 'O');
            assert_eq!(pending.bytes, saved);
            assert!(wake_pending.load(Ordering::Acquire));
        }
        assert!(
            pending
                .advance(&mut term, false, || panic!("retained output first"))
                .is_some()
        );
        assert_eq!(term.viewport_row(0).cells[0].c, 'N');
        assert!(term.modes.synchronized_output);
        assert!(pending.waiting_since.is_none());
        assert!(
            pending
                .advance(&mut term, true, || panic!("paused queued output"))
                .is_none()
        );
        assert!(wake_pending.load(Ordering::Acquire));
        assert!(
            pending
                .advance(&mut term, false, || {
                    wake_pending.store(false, Ordering::Release);
                    queued.take().unwrap()
                })
                .is_some()
        );
        assert_eq!(term.viewport_row(0).cells[0].c, 'E');
        assert!(!wake_pending.load(Ordering::Acquire));
        assert!(queued.is_none());
        pending.presented();
        assert_eq!(
            pending.advance(&mut term, false, || batch(b"")),
            Some((0, false))
        );
    }

    #[test]
    fn shell_return_markers_follow_retained_frames() {
        let mut term = Terminal::new(10, 2);
        let mut pending = PendingOutput::default();
        let first = b"\x1b[?1000;1004;2004h\x1b[?2026hOLD\x1b[?2026l";
        let second = b"\x1b[?2026h\x1b[?1000;1004;2004h\rNEW\x1b[?2026l";
        let prompt = b"\x1b[?2004h$ ";
        let mut output = pty::Output {
            bytes: [first.as_slice(), second.as_slice(), prompt.as_slice()].concat(),
            shell_returns: vec![first.len(), first.len() + second.len()],
        };
        let consumed = advance_output(&mut term, &mut output);
        assert_eq!(consumed, first.len() + second.len());
        assert_eq!(term.modes.mouse, MouseMode::Off);
        assert!(!term.modes.focus_events);
        assert!(!term.modes.bracketed_paste);
        pending.retain(
            output,
            consumed,
            term.synchronized_output_ended(),
            Instant::now(),
        );
        assert!(pending.waiting_since.is_some());
        assert!(!pending.is_empty());
        pending.presented();
        output = pending.next_batch(|| panic!("retained prompt must precede PTY output"));
        assert_eq!(advance_output(&mut term, &mut output), prompt.len());
        assert!(term.modes.bracketed_paste);
    }

    #[test]
    fn marker_only_output_is_not_empty_or_lost() {
        let mut term = Terminal::new(10, 2);
        term.advance(b"\x1b[?1000;1004;2004h");
        let mut pending = PendingOutput::default();
        pending.retain(
            pty::Output {
                bytes: Vec::new(),
                shell_returns: vec![0],
            },
            0,
            false,
            Instant::now(),
        );
        assert!(!pending.is_empty());
        assert!(pending.presented());
        let mut output = pending.next_batch(|| panic!("marker must precede PTY output"));
        assert_eq!(advance_output(&mut term, &mut output), 0);
        pending.retain(output, 0, term.synchronized_output_ended(), Instant::now());
        assert!(pending.is_empty());
        assert_eq!(term.modes.mouse, MouseMode::Off);
        assert!(!term.modes.focus_events);
        assert!(!term.modes.bracketed_paste);
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
    fn icons_wait_for_the_first_frame_and_quiet_output() {
        let now = Instant::now();
        let latest = now + ICONS_LATEST;
        let drawn = now + Duration::from_millis(100);
        assert_eq!(icons_due(None, now, latest), latest);
        assert_eq!(icons_due(Some(drawn), now, latest), drawn + ICONS_QUIET);
        let output = drawn + Duration::from_millis(30);
        assert_eq!(icons_due(Some(drawn), output, latest), output + ICONS_QUIET);
        assert_eq!(icons_due(Some(drawn), latest, latest), latest);
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
    fn cursor_blink_flips_then_rests_when_idle() {
        let since = Instant::now();
        assert_eq!(cursor_blink(false, since, since), (true, None));
        assert_eq!(
            cursor_blink(true, since, since),
            (true, Some(since + CURSOR_BLINK_HALF))
        );
        let hidden = since + CURSOR_BLINK_HALF + Duration::from_millis(1);
        assert_eq!(
            cursor_blink(true, since, hidden),
            (false, Some(since + CURSOR_BLINK_HALF * 2))
        );
        let last = since + CURSOR_BLINK_IDLE - Duration::from_millis(1);
        assert_eq!(
            cursor_blink(true, since, last).1,
            Some(since + CURSOR_BLINK_IDLE)
        );
        assert_eq!(
            cursor_blink(true, since, since + CURSOR_BLINK_IDLE),
            (true, None)
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
    fn synchronized_output_timeout() {
        let start = Instant::now();
        assert!(!sync_expired(None, start + SYNC_TIMEOUT));
        assert!(!sync_expired(Some(start), start));
        assert!(!sync_expired(
            Some(start),
            start + SYNC_TIMEOUT - Duration::from_nanos(1)
        ));
        assert!(sync_expired(Some(start), start + SYNC_TIMEOUT));
        assert!(sync_expired(
            Some(start),
            start + Duration::from_millis(300)
        ));
    }

    #[test]
    fn blink_alternates_each_half_period() {
        let epoch = Instant::now();
        let half = BLINK_HALF_PERIOD;
        assert_eq!(blink_phase(epoch, epoch), (true, epoch + half));
        let late = epoch + half - Duration::from_nanos(1);
        assert_eq!(blink_phase(epoch, late), (true, epoch + half));
        assert_eq!(blink_phase(epoch, epoch + half), (false, epoch + half * 2));
        assert_eq!(
            blink_phase(epoch, epoch + half * 5 + Duration::from_millis(1)),
            (false, epoch + half * 6)
        );
        assert_eq!(
            blink_phase(epoch, epoch + half * 6),
            (true, epoch + half * 7)
        );
    }

    #[test]
    fn sync_deadlines_bound_vblank_waits_without_idle_timers() {
        let now = Instant::now();
        assert_eq!(sync_wakeup(None, None, None), None);
        assert_eq!(sync_wakeup(Some(now), None, None), Some(now));
        assert_eq!(
            sync_wakeup(Some(now + SYNC_TIMEOUT * 2), None, Some(now)),
            Some(now + SYNC_TIMEOUT)
        );
        assert_eq!(
            sync_wakeup(Some(now + SYNC_TIMEOUT), Some(now), Some(now)),
            Some(now + MAX_DEFER)
        );
        assert_eq!(sync_wakeup(Some(now), Some(now), Some(now)), Some(now));
    }

    #[test]
    fn failed_presentation_retains_frame_and_retries_without_draining() {
        let now = Instant::now();
        let mut pending = PendingOutput {
            bytes: b"next frame".to_vec(),
            waiting_since: Some(now),
            ..PendingOutput::default()
        };
        let retry = now + MAX_DEFER;
        assert!(!pending.finish_presentation(false, retry));
        assert_eq!(pending.waiting_since, Some(retry));
        assert_eq!(pending.bytes, b"next frame");
        assert!(pending.finish_presentation(true, retry));
        assert_eq!(pending.waiting_since, None);
        assert_eq!(pending.bytes, b"next frame");
        let mut empty = PendingOutput::default();
        assert!(!empty.finish_presentation(false, now));
        assert_eq!(empty.waiting_since, None);
        assert!(!empty.finish_presentation(true, now));
        empty.waiting_since = Some(now);
        assert!(!empty.finish_presentation(false, retry));
        assert_eq!(empty.waiting_since, Some(retry));
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
