//! Spawning the shell on a pseudo-terminal and pumping its output to the UI thread.

use crate::flow_control::FlowSettings;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Output the reader thread queues before it waits for the UI, once it also holds
/// `MIN_LINES` lines. That many lines is well past a screen plus default scrollback, so a
/// flood's batch is long enough to skip building the lines it evicts, even with long lines.
const MIN_QUEUED: usize = 1 << 20;
const MIN_LINES: usize = 32 * 1024;
/// The most the reader queues whatever the line count, which is also the most the UI
/// parses per wake-up, so huge bursts can't starve rendering.
const MAX_QUEUED: usize = 8 << 20;
const READ_BUFFER: usize = 64 * 1024;
/// How often the reader asks for the PTY's foreground process group while earlier output
/// is still queued, instead of after every read. Reads with escape sequences, which can
/// change the input modes a shell return resets, are always checked.
const FOREGROUND_INTERVAL: Duration = Duration::from_millis(5);
/// Reads starting within this many bytes of an escape byte are always checked too, since
/// they may finish a sequence split across reads.
const ESCAPE_TAIL: usize = 64;
/// How long the reader retries an empty PTY before it sleeps in `poll`. A blocking read
/// sleeps and wakes once per call, and macOS returns at most 1 KiB per read, which capped a
/// 100 MiB `cat` at about 1.4 s; a short spin picks up the writer's next chunk without that
/// round trip, and an idle PTY still costs nothing once the spin gives up.
#[cfg(unix)]
const READ_SPIN: Duration = Duration::from_micros(20);

/// Shell output read but not yet parsed, as one buffer: small reads (macOS returns about
/// 1 KiB per read) still reach the parser as one batch.
#[derive(Default)]
struct Queue {
    read_sequence: AtomicU64,
    read_bytes: AtomicU64,
    pending: Mutex<Pending>,
    room: Condvar,
    probe: Probe,
}

/// Asks whether the shell itself is the PTY's foreground process group.
#[derive(Default)]
struct Probe {
    #[cfg(unix)]
    fd: Option<std::os::fd::OwnedFd>,
    shell: Option<u32>,
    /// `argv[0]` of the default launch's login shell (`-bash`), which stays idle even when
    /// its name is not a known shell. Whatever holds the foreground counts as a running
    /// program unless its argv is that shell or another idle shell, so an explicit command
    /// (`terminal -e vim`) or a program the shell exec'd into (`exec vim`) prompts.
    login: Option<String>,
}

impl Probe {
    /// The foreground process group, when it is a program rather than an idle shell.
    #[cold]
    fn program(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            let fd = self.fd.as_ref()?;
            // SAFETY: fd is a PTY master descriptor owned by this probe.
            let group = unsafe { libc::tcgetpgrp(fd.as_raw_fd()) };
            let shell = self.shell?;
            let running = group as u32 != shell
                || !crate::close_confirm::process_args(group).is_some_and(|args| self.idle(&args));
            (group > 0 && running).then_some(group)
        }
        #[cfg(not(unix))]
        None
    }

    /// Whether the foreground process group is an idle shell, the login one or another.
    #[cold]
    fn idle_foreground(&self) -> Option<bool> {
        #[cfg(unix)]
        {
            let fd = self.fd.as_ref()?;
            // SAFETY: fd is a PTY master descriptor owned by this probe.
            let group = unsafe { libc::tcgetpgrp(fd.as_raw_fd()) };
            if group <= 0 {
                return None;
            }
            crate::close_confirm::process_args(group).map(|args| self.idle(&args))
        }
        #[cfg(not(unix))]
        None
    }

    fn idle(&self, args: &[String]) -> bool {
        is_idle_shell(args) || (self.login.is_some() && args == self.login.as_slice())
    }

    fn editor_modes(&self) -> bool {
        #[cfg(unix)]
        {
            self.fd
                .as_ref()
                .is_some_and(|fd| editor_modes(fd.as_raw_fd()))
        }
        #[cfg(not(unix))]
        false
    }

    fn shell(&self) -> Option<bool> {
        #[cfg(unix)]
        {
            let fd = self.fd.as_ref()?;
            // SAFETY: fd is a PTY master descriptor owned by this probe.
            let group = unsafe { libc::tcgetpgrp(fd.as_raw_fd()) };
            self.shell
                .filter(|_| group > 0)
                .map(|pid| group as u32 == pid)
        }
        #[cfg(not(unix))]
        {
            let _ = self.shell;
            None
        }
    }
}

#[derive(Default)]
struct Pending {
    sequence: u64,
    read_bytes: u64,
    bytes: Vec<u8>,
    shell_returns: Vec<usize>,
    lines: usize,
    closed: bool,
    foreground: Foreground,
    checked_at: Option<Instant>,
    /// Start of the queued output read since the foreground group was last checked.
    unchecked_from: Option<usize>,
    /// Bytes read since the last escape byte, if any.
    since_escape: Option<usize>,
    /// Start of the reads queued since the last one without a line editor's modes.
    editing_from: Option<usize>,
}

impl Pending {
    /// Checks the foreground group for the next read, unless earlier output is still
    /// queued and was checked less than `FOREGROUND_INTERVAL` ago: the next check, at the
    /// latest when the output is taken, marks a return at the first unchecked read.
    /// Skipped reads hold no escape sequence, so a mark anywhere among them resets the
    /// same modes as one at the exact read where the shell got the foreground back.
    fn check_foreground(
        &mut self,
        now: Instant,
        bytes: &[u8],
        shell: impl FnOnce() -> Option<bool>,
    ) -> bool {
        let open = self.since_escape.is_some_and(|n| n < ESCAPE_TAIL);
        let escape = memchr::memrchr(0x1b, bytes);
        self.since_escape = match escape {
            Some(at) => Some(bytes.len() - at - 1),
            None => self.since_escape.map(|n| n.saturating_add(bytes.len())),
        };
        if escape.is_none()
            && !open
            && !self.bytes.is_empty()
            && self
                .checked_at
                .is_some_and(|at| now.duration_since(at) < FOREGROUND_INTERVAL)
        {
            return false;
        }
        self.sample_foreground(now, shell());
        true
    }

    fn sample_foreground(&mut self, now: Instant, shell: Option<bool>) {
        self.checked_at = Some(now);
        let from = self.unchecked_from.take().unwrap_or(self.bytes.len());
        if self.foreground.sample(shell) {
            self.shell_returns.push(from);
        }
    }

    fn append(&mut self, bytes: &[u8], checked: bool, editing: bool) {
        self.editing_from = editing.then(|| self.editing_from.unwrap_or(self.bytes.len()));
        if !checked && self.unchecked_from.is_none() {
            self.unchecked_from = Some(self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        self.lines += bytes.iter().filter(|&&b| b == b'\n').count();
    }

    fn take_output(&mut self, now: Instant, shell: impl FnOnce() -> Option<bool>) -> Output {
        if self.unchecked_from.is_some() {
            self.sample_foreground(now, shell());
        }
        self.lines = 0;
        Output {
            bytes: std::mem::take(&mut self.bytes),
            shell_returns: std::mem::take(&mut self.shell_returns),
            line_editing: self.editing_from.take(),
            line_sent: false,
        }
    }

    fn full(&self) -> bool {
        self.bytes.len() >= MAX_QUEUED
            || (self.bytes.len() >= MIN_QUEUED && self.lines >= MIN_LINES)
    }
}

#[derive(Default)]
pub struct Output {
    pub bytes: Vec<u8>,
    pub shell_returns: Vec<usize>,
    /// Where the trailing reads that found a line editor's modes (see `Pty::line_editing`)
    /// start: the shell's echo and redraws, which it measures one codepoint at a time.
    pub line_editing: Option<usize>,
    /// The user sent a line since the last output was taken.
    pub line_sent: bool,
}

#[derive(Default)]
struct Foreground {
    away: bool,
}

impl Foreground {
    fn sample(&mut self, shell: Option<bool>) -> bool {
        match shell {
            Some(true) => std::mem::take(&mut self.away),
            Some(false) => {
                self.away = true;
                false
            }
            None => false,
        }
    }
}

pub struct Pty {
    pub flow: crate::flow_control::FlowControl,
    pub flow_redraw: bool,
    /// The user sent a line since the output was last taken (see `Output::line_sent`).
    line_sent: bool,
    master: Box<dyn MasterPty + Send>,
    /// Input queued for the writer thread, so a full PTY never blocks the UI.
    input: Sender<Input>,
    pause_boundary: Option<Arc<AtomicU64>>,
    child: Box<dyn Child + Send + Sync>,
    output: Arc<Queue>,
    /// Set while a wake-up is queued, so a burst of output wakes the UI only once.
    pub wake_pending: Arc<AtomicBool>,
    watcher_stopped: Arc<AtomicBool>,
    #[cfg(unix)]
    watcher_activity: Arc<crate::command_notify::Activity>,
    #[cfg(unix)]
    watcher: Option<crate::command_notify::Watcher>,
    finished: Arc<dyn Fn(crate::command_notify::Finished) + Send + Sync>,
}

struct Input {
    bytes: Vec<u8>,
    pause_boundary: Option<Arc<AtomicU64>>,
}

/// A line editor's modes (readline, zle, also in python or psql): no canonical mode or echo,
/// signals on, and CR left unmapped, unlike a program's silent read.
#[cfg(unix)]
fn editor_modes(fd: std::os::fd::RawFd) -> bool {
    let mut attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: fd is a live PTY master; tcgetattr initializes attrs on success.
    if unsafe { libc::tcgetattr(fd, attrs.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: tcgetattr succeeded.
    let attrs = unsafe { attrs.assume_init() };
    attrs.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG) == libc::ISIG
        && attrs.c_iflag & (libc::ICRNL | libc::INLCR) != libc::ICRNL
}

#[cfg(unix)]
fn buffered_output(fd: std::os::fd::RawFd) -> u64 {
    let mut bytes: libc::c_int = 0;
    // SAFETY: fd is a live PTY master and bytes is writable for FIONREAD.
    if unsafe { libc::ioctl(fd, libc::FIONREAD, &mut bytes) } == 0 {
        bytes.max(0) as u64
    } else {
        u64::MAX
    }
}

/// Sleeps until `fd` is ready for `events`, or has hung up.
#[cfg(unix)]
fn wait_until(fd: BorrowedFd, events: libc::c_short) -> std::io::Result<()> {
    let mut poll = libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    };
    loop {
        // SAFETY: poll reads and writes one live pollfd.
        if unsafe { libc::poll(&mut poll, 1, -1) } >= 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(unix)]
fn set_nonblocking(fd: BorrowedFd) -> std::io::Result<()> {
    // SAFETY: fcntl only reads and sets the status flags of a live descriptor.
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Reads a non-blocking PTY master: spins `READ_SPIN` on an empty PTY, then polls.
#[cfg(unix)]
struct SpinReader(std::fs::File);

#[cfg(unix)]
impl Read for SpinReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut empty_since = None;
        loop {
            match self.0.read(buf) {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    let since = *empty_since.get_or_insert_with(Instant::now);
                    if since.elapsed() >= READ_SPIN {
                        wait_until(self.0.as_fd(), libc::POLLIN)?;
                        empty_since = None;
                    } else {
                        std::hint::spin_loop();
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                result => return result,
            }
        }
    }
}

/// `write_all` for a PTY master shared with the non-blocking reader: a full input
/// buffer waits for room instead of failing.
fn write_all_waiting(
    writer: &mut impl Write,
    #[cfg(unix)] fd: Option<BorrowedFd>,
    mut bytes: &[u8],
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        match writer.write(bytes) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                #[cfg(unix)]
                if let Some(fd) = fd {
                    wait_until(fd, libc::POLLOUT)?;
                    continue;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    writer.flush()
}

fn size(cols: usize, rows: usize, cell_w: f32, cell_h: f32) -> PtySize {
    PtySize {
        rows: rows as u16,
        cols: cols as u16,
        pixel_width: (cols as f32 * cell_w) as u16,
        pixel_height: (rows as f32 * cell_h) as u16,
    }
}

/// How a shell's argv asks it to run something other than its prompt.
struct ShellArgs {
    /// The short option that runs a command string (`-c`).
    command: char,
    /// Short options that take a value (`-o vi`).
    short_values: &'static str,
    /// Whether a short option's value may follow it in the same argument (getopt's `-Ccmd`);
    /// otherwise it is always the next argument and the cluster goes on (bash's `-oc vi cmd`).
    attached_values: bool,
    /// Long options (without `--`) that run a command string.
    long_commands: &'static [&'static str],
    /// Long options (without `--`) that take a value, after `=` or as the next argument.
    long_values: &'static [&'static str],
}

const POSIX_SHELL_ARGS: ShellArgs = ShellArgs {
    command: 'c',
    short_values: "o",
    attached_values: false,
    long_commands: &[],
    long_values: &[],
};

/// The argv grammar of the shells whose options are modelled.
fn shell_args(shell: &str) -> Option<ShellArgs> {
    Some(match shell {
        "bash" => ShellArgs {
            short_values: "oO",
            long_values: &["rcfile", "init-file"],
            ..POSIX_SHELL_ARGS
        },
        "zsh" => ShellArgs {
            long_values: &["emulate"],
            ..POSIX_SHELL_ARGS
        },
        "sh" | "ash" | "dash" | "ksh" | "mksh" | "oksh" | "yash" | "osh" | "ysh" => {
            POSIX_SHELL_ARGS
        }
        "fish" => ShellArgs {
            command: 'c',
            short_values: "Cdo",
            attached_values: true,
            long_commands: &["command"],
            long_values: &[
                "init-command",
                "debug",
                "debug-output",
                "features",
                "profile",
                "profile-startup",
            ],
        },
        _ => return None,
    })
}

/// Whether `args` (argv) is a shell started for its prompt rather than for a command string
/// (`-c`, also inside a cluster such as `-lc`, or fish's `--command`) or a script operand
/// (`bash build.sh`, anything after `--`). Options are case-sensitive, and the values of
/// options that take one (`-o vi`, `-O extglob`, `--rcfile f`, fish `-C cmd`) are skipped.
/// Shells whose options are not modelled count as idle only with no options other than
/// `-l`, `-i`, `--login` and `--interactive`.
fn is_idle_shell(args: &[String]) -> bool {
    let Some((program, options)) = args.split_first() else {
        return false;
    };
    let Some(shell) = shell_name(program) else {
        return false;
    };
    let Some(grammar) = shell_args(shell) else {
        return options
            .iter()
            .all(|arg| matches!(arg.as_str(), "-l" | "-i" | "--login" | "--interactive"));
    };
    let mut options = options.iter();
    while let Some(arg) = options.next() {
        if arg == "--" || arg == "-" {
            return options.next().is_none();
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, value) = long
                .split_once('=')
                .map_or((long, None), |(n, v)| (n, Some(v)));
            if grammar.long_commands.contains(&name) {
                return false;
            }
            if grammar.long_values.contains(&name) && value.is_none() && options.next().is_none() {
                return false;
            }
            continue;
        }
        let Some(flags) = arg.strip_prefix(['-', '+']) else {
            return false;
        };
        for (at, flag) in flags.char_indices() {
            if flag == grammar.command {
                return false;
            }
            if grammar.short_values.contains(flag) {
                if grammar.attached_values && at + flag.len_utf8() < flags.len() {
                    break;
                }
                if options.next().is_none() {
                    return false;
                }
            }
        }
    }
    true
}

/// The shell `program` names (`/bin/bash`, `-zsh` for a login shell), if it is one.
fn shell_name(program: &str) -> Option<&str> {
    let name = std::path::Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(program)
        .trim_start_matches('-');
    matches!(
        name,
        "sh" | "ash"
            | "bash"
            | "dash"
            | "zsh"
            | "fish"
            | "ksh"
            | "mksh"
            | "oksh"
            | "yash"
            | "csh"
            | "tcsh"
            | "nu"
            | "elvish"
            | "xonsh"
            | "pwsh"
            | "osh"
            | "ysh"
    )
    .then_some(name)
}

impl Pty {
    /// Starts `command` (the user's login shell when empty). `wake` is called from the
    /// reader thread when output is ready, and with `true` once the shell exits. It starts in
    /// `cwd` when given, else in this process's directory (`$HOME` when that is `/`).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        command: &[String],
        env: &std::collections::BTreeMap<String, String>,
        cwd: Option<&std::path::Path>,
        cols: usize,
        rows: usize,
        cell: (f32, f32),
        wake: impl Fn(bool) + Send + 'static,
        finished: impl Fn(crate::command_notify::Finished) + Send + Sync + 'static,
    ) -> Result<Pty, String> {
        let pair = native_pty_system()
            .openpty(size(cols, rows, cell.0, cell.1))
            .map_err(|e| format!("cannot open pty: {e}"))?;
        let mut cmd = match command.split_first() {
            Some((program, args)) => {
                let mut cmd = CommandBuilder::new(program);
                cmd.args(args);
                cmd
            }
            None => CommandBuilder::new_default_prog(),
        };
        cmd.env("TERM", crate::terminfo::FALLBACK);
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", env!("CARGO_BIN_NAME"));
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        for (key, value) in env {
            cmd.env(key, value);
        }
        if let Some(dir) = cwd {
            cmd.cwd(dir);
        } else if let Some(home) = std::env::var_os("HOME")
            && std::env::current_dir()
                .map(|d| d == std::path::Path::new("/"))
                .unwrap_or(true)
        {
            cmd.cwd(home);
        }
        // portable-pty starts the default shell as a login shell, with argv[0] `-<basename>`.
        let login = cmd.is_default_prog().then(|| {
            let shell = cmd.get_shell();
            format!("-{}", shell.rsplit('/').next().unwrap_or(&shell))
        });
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("cannot start shell: {e}"))?;
        drop(pair.slave);
        let shell_pid = child.process_id();
        #[cfg(unix)]
        let foreground_fd = pair
            .master
            .as_raw_fd()
            .map(|fd| unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned())
            .transpose()
            .map_err(|e| format!("cannot clone pty: {e}"))?;

        #[cfg(unix)]
        let mut reader: Box<dyn Read + Send> = match &foreground_fd {
            Some(fd) => {
                let fd = fd
                    .try_clone()
                    .map_err(|e| format!("cannot read pty: {e}"))?;
                // The flag is on the master's shared open file, so the writer waits for room too.
                set_nonblocking(fd.as_fd()).map_err(|e| format!("cannot read pty: {e}"))?;
                Box::new(SpinReader(fd.into()))
            }
            None => pair
                .master
                .try_clone_reader()
                .map_err(|e| format!("cannot read pty: {e}"))?,
        };
        #[cfg(not(unix))]
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("cannot read pty: {e}"))?;
        let watcher_stopped = Arc::new(AtomicBool::new(false));
        #[cfg(unix)]
        let watcher_activity = Arc::new(crate::command_notify::Activity::default());
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("cannot write pty: {e}"))?;
        let output = Arc::new(Queue {
            probe: Probe {
                #[cfg(unix)]
                fd: foreground_fd,
                shell: shell_pid,
                login,
            },
            ..Default::default()
        });
        let writer_queue = output.clone();
        #[cfg(unix)]
        let writer_fd = output
            .probe
            .fd
            .as_ref()
            .map(|fd| fd.try_clone())
            .transpose()
            .map_err(|e| format!("cannot clone pty: {e}"))?;
        let (input, input_rx) = channel::<Input>();
        std::thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                for input in input_rx {
                    if write_all_waiting(
                        &mut writer,
                        #[cfg(unix)]
                        writer_fd.as_ref().map(|fd| fd.as_fd()),
                        &input.bytes,
                    )
                    .is_err()
                    {
                        break;
                    }
                    if let Some(boundary) = input.pause_boundary {
                        let read_bytes = writer_queue.read_bytes.load(Ordering::SeqCst);
                        #[cfg(unix)]
                        let buffered = writer_fd
                            .as_ref()
                            .map(|fd| buffered_output(fd.as_raw_fd()))
                            .unwrap_or(u64::MAX);
                        #[cfg(not(unix))]
                        let buffered = u64::MAX;
                        boundary.store(
                            read_bytes
                                .saturating_add(buffered)
                                .saturating_add(READ_BUFFER as u64),
                            Ordering::Release,
                        );
                    }
                }
            })
            .map_err(|e| format!("cannot start writer thread: {e}"))?;

        // Bounded, so a shell producing output faster than we parse it is paused by
        // the kernel instead of growing this queue without limit.
        let queue = output.clone();
        let wake_pending = Arc::new(AtomicBool::new(false));
        let pending = wake_pending.clone();
        let reader_stopped = watcher_stopped.clone();
        #[cfg(unix)]
        let reader_activity = watcher_activity.clone();
        std::thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; READ_BUFFER];
                let mut was_editing = false;
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            #[cfg(unix)]
                            reader_activity.kick();
                            // The modes are read after the bytes were written: a read that
                            // finds the editor's modes just set is the end of a command's
                            // output and the prompt, not the editor's own redraw.
                            let modes = queue.probe.editor_modes();
                            let editing = modes && was_editing;
                            was_editing = modes;
                            let sequence = queue.read_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                            let read_bytes =
                                queue.read_bytes.fetch_add(n as u64, Ordering::SeqCst) + n as u64;
                            let mut p = queue.pending.lock().unwrap();
                            let checked = p.check_foreground(Instant::now(), &buf[..n], || {
                                queue.probe.shell()
                            });
                            if p.full() {
                                // The UI may be holding off parsing until its next frame.
                                pending.store(true, Ordering::Release);
                                wake(false);
                            }
                            while p.full() && !p.closed {
                                p = queue.room.wait(p).unwrap();
                            }
                            if p.closed {
                                return;
                            }
                            p.sequence = sequence;
                            p.read_bytes = read_bytes;
                            p.append(&buf[..n], checked, editing);
                            drop(p);
                            if !pending.swap(true, Ordering::AcqRel) {
                                wake(false);
                            }
                        }
                    }
                }
                reader_stopped.store(true, Ordering::Release);
                #[cfg(unix)]
                reader_activity.wake_for_shutdown();
                wake(true);
            })
            .map_err(|e| format!("cannot start reader thread: {e}"))?;

        Ok(Pty {
            flow: Default::default(),
            flow_redraw: false,
            line_sent: false,
            master: pair.master,
            input,
            pause_boundary: None,
            child,
            output,
            wake_pending,
            watcher_stopped,
            #[cfg(unix)]
            watcher_activity,
            #[cfg(unix)]
            watcher: None,
            finished: Arc::new(finished),
        })
    }

    pub fn set_command_notifications(&mut self, enabled: bool) -> Result<(), String> {
        #[cfg(unix)]
        {
            if !enabled {
                self.watcher = None;
            } else if self.watcher.is_none()
                && let (Some(fd), Some(shell)) = (self.master.as_raw_fd(), self.child.process_id())
            {
                let fd = unsafe { BorrowedFd::borrow_raw(fd) }
                    .try_clone_to_owned()
                    .map_err(|e| format!("cannot clone pty: {e}"))?;
                let finished = self.finished.clone();
                self.watcher = Some(
                    crate::command_notify::watch(
                        fd,
                        shell,
                        self.watcher_stopped.clone(),
                        self.watcher_activity.clone(),
                        move |event| finished(event),
                    )
                    .map_err(|e| format!("cannot start command watcher: {e}"))?,
                );
            }
        }
        Ok(())
    }

    /// The process group of a program running in the foreground instead of the shell.
    pub fn running_program(&self) -> Option<i32> {
        self.output.probe.program()
    }

    /// Whether a program holds the foreground, as a check that outlives this borrow, for the
    /// macOS app quit hook.
    #[cfg(target_os = "macos")]
    pub fn running_program_check(&self) -> impl Fn() -> bool + 'static {
        let probe = Probe {
            fd: self
                .output
                .probe
                .fd
                .as_ref()
                .and_then(|fd| fd.try_clone().ok()),
            shell: self.output.probe.shell,
            login: self.output.probe.login.clone(),
        };
        move || probe.program().is_some()
    }

    /// Everything the shell wrote since the last call.
    pub fn take_output(&mut self) -> Output {
        let mut p = self.output.pending.lock().unwrap();
        let mut output = p.take_output(Instant::now(), || self.output.probe.shell());
        output.line_sent = std::mem::take(&mut self.line_sent);
        if !output.bytes.is_empty() {
            if let Some(boundary) = &self.pause_boundary {
                self.flow
                    .protect_output_until(boundary.load(Ordering::Acquire));
            }
            self.flow.output(p.sequence, p.read_bytes);
        }
        drop(p);
        self.output.room.notify_one();
        output
    }

    /// Whether the reader is waiting for the UI to take its output.
    pub fn output_full(&self) -> bool {
        self.output.pending.lock().unwrap().full()
    }

    pub fn write(&mut self, bytes: &[u8]) {
        #[cfg(unix)]
        self.watcher_activity.kick();
        if bytes.is_empty() {
            return;
        }
        self.line_sent |= memchr::memchr2(b'\r', b'\n', bytes).is_some();
        let settings = self.flow_flags().unwrap_or_default();
        self.flow_redraw |= self.flow.input(
            bytes,
            settings,
            self.output.read_sequence.load(Ordering::SeqCst),
        );
        let pause_boundary = (self.flow.paused
            && settings.stop.is_some_and(|stop| bytes.contains(&stop)))
        .then(|| Arc::new(AtomicU64::new(u64::MAX)));
        if pause_boundary.is_some() {
            self.pause_boundary = pause_boundary.clone();
        }
        let _ = self.input.send(Input {
            bytes: bytes.to_vec(),
            pause_boundary,
        });
    }

    /// Whether the shell's line editor (readline, zle) owns the terminal: echo and canonical
    /// input are off while signals stay on, unlike the raw mode of full-screen apps and ssh,
    /// and the foreground is a shell rather than a program, since a cbreak program in the same
    /// mode need not redraw its output on a resize. A shell's own silent `read` (bash
    /// `read -rsn1`, zsh `read -sk1`) sets the same local modes but never redraws; it maps CR
    /// to NL alone, where readline maps neither and zle both.
    pub fn line_editing(&self) -> bool {
        self.editor_modes() && self.output.probe.idle_foreground() != Some(false)
    }

    /// Whether the foreground program reads a password: see `secure_input::password_modes`.
    #[cfg(any(target_os = "macos", test))]
    pub fn password_input(&self) -> bool {
        #[cfg(unix)]
        {
            let Some(fd) = self.master.as_raw_fd() else {
                return false;
            };
            let mut attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
            // SAFETY: fd belongs to the live master; tcgetattr initializes attrs on success.
            if unsafe { libc::tcgetattr(fd, attrs.as_mut_ptr()) } != 0 {
                return false;
            }
            // SAFETY: tcgetattr succeeded.
            crate::secure_input::password_modes(unsafe { attrs.assume_init() }.c_lflag)
        }
        #[cfg(not(unix))]
        false
    }

    fn editor_modes(&self) -> bool {
        #[cfg(unix)]
        {
            self.master.as_raw_fd().is_some_and(editor_modes)
        }
        #[cfg(not(unix))]
        false
    }

    fn flow_flags(&self) -> Option<FlowSettings> {
        #[cfg(unix)]
        {
            let fd = self.master.as_raw_fd()?;
            let mut attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
            // SAFETY: fd belongs to the live master; tcgetattr initializes attrs on success.
            if unsafe { libc::tcgetattr(fd, attrs.as_mut_ptr()) } != 0 {
                return None;
            }
            // SAFETY: tcgetattr succeeded.
            let attrs = unsafe { attrs.assume_init() };
            let key = |index: usize| {
                let value = attrs.c_cc[index];
                (value != libc::_POSIX_VDISABLE).then_some(value)
            };
            Some(FlowSettings {
                ixon: attrs.c_iflag & libc::IXON != 0,
                ixany: attrs.c_iflag & libc::IXANY != 0,
                isig: attrs.c_lflag & libc::ISIG != 0,
                signals: [key(libc::VINTR), key(libc::VQUIT), key(libc::VSUSP)],
                stop: key(libc::VSTOP),
                start: key(libc::VSTART),
            })
        }
        #[cfg(not(unix))]
        None
    }

    pub fn refresh_flow(&mut self) {
        let was_paused = self.flow.paused;
        self.flow.flags(self.flow_flags().unwrap_or_default());
        if let Some(boundary) = &self.pause_boundary {
            self.flow
                .protect_output_until(boundary.load(Ordering::Acquire));
        }
        self.flow.output(
            self.output.read_sequence.load(Ordering::SeqCst),
            self.output.read_bytes.load(Ordering::SeqCst),
        );
        self.flow_redraw |= self.flow.paused != was_paused;
    }

    pub fn resize(&self, cols: usize, rows: usize, cell: (f32, f32)) {
        let _ = self.master.resize(size(cols, rows, cell.0, cell.1));
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.watcher_stopped.store(true, Ordering::Release);
        #[cfg(unix)]
        self.watcher_activity.wake_for_shutdown();
        self.output.pending.lock().unwrap().closed = true;
        self.output.room.notify_one();
        let _ = self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn nonblocking_pipe() -> (std::fs::File, std::fs::File) {
        use std::os::fd::FromRawFd;
        let mut fds = [0; 2];
        // SAFETY: pipe writes two new descriptors into fds.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: both descriptors were just created and are owned here.
        let (r, w) = unsafe {
            (
                std::os::fd::OwnedFd::from_raw_fd(fds[0]),
                std::os::fd::OwnedFd::from_raw_fd(fds[1]),
            )
        };
        set_nonblocking(r.as_fd()).unwrap();
        set_nonblocking(w.as_fd()).unwrap();
        (r.into(), w.into())
    }

    #[cfg(unix)]
    #[test]
    fn the_spin_reader_waits_for_late_output_and_sees_the_end() {
        let (r, mut w) = nonblocking_pipe();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            w.write_all(b"late").unwrap();
        });
        let mut reader = SpinReader(r);
        let mut buf = [0u8; 16];
        let n = reader.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"late");
        writer.join().unwrap();
        assert_eq!(reader.read(&mut buf).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_write_waits_for_room_in_a_full_nonblocking_pty() {
        let (mut r, mut w) = nonblocking_pipe();
        let sent: Vec<u8> = (0..1 << 20).map(|i| (i % 251) as u8).collect();
        let expected = sent.clone();
        let reader = std::thread::spawn(move || {
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => return got,
                    Ok(n) => got.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        });
        let fd = w.try_clone().unwrap();
        write_all_waiting(&mut w, Some(fd.as_fd()), &sent).unwrap();
        drop((w, fd));
        assert!(reader.join().unwrap() == expected);
    }

    #[cfg(unix)]
    #[test]
    fn a_large_paste_and_its_output_cross_the_pty_intact() {
        let dir = std::env::temp_dir().join(format!("terminal-paste-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("paste");
        let line = [b'x'; 63];
        let lines = 4096;
        let mut paste = Vec::new();
        for _ in 0..lines {
            paste.extend_from_slice(&line);
            paste.push(b'\n');
        }
        let script = format!(
            "stty -echo; head -c {} > '{}'; printf done; head -c 3000000 /dev/zero | tr '\\0' x",
            paste.len(),
            file.display()
        );
        let mut pty = Pty::spawn(
            &["sh".into(), "-c".into(), script],
            &Default::default(),
            None,
            80,
            24,
            (8.0, 16.0),
            |_| {},
            |_| {},
        )
        .unwrap();
        pty.write(&paste);
        let mut output = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while output.iter().filter(|&&b| b == b'x').count() < 3_000_000 {
            assert!(Instant::now() < deadline, "got {} bytes", output.len());
            output.extend_from_slice(&pty.take_output().bytes);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(output.windows(4).any(|w| w == b"done"));
        assert_eq!(std::fs::read(&file).unwrap(), paste);
        drop(pty);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn notifications_start_disabled_and_can_be_reloaded() {
        let mut pty = Pty::spawn(
            &["/bin/sh".into()],
            &Default::default(),
            None,
            80,
            24,
            (8.0, 16.0),
            |_| {},
            |_| {},
        )
        .unwrap();
        assert!(pty.watcher.is_none());
        pty.set_command_notifications(false).unwrap();
        assert!(pty.watcher.is_none());
        pty.set_command_notifications(true).unwrap();
        assert!(pty.watcher.is_some());
        pty.set_command_notifications(true).unwrap();
        pty.set_command_notifications(false).unwrap();
        assert!(pty.watcher.is_none());
        pty.set_command_notifications(true).unwrap();
        assert!(pty.watcher.is_some());
    }

    #[cfg(unix)]
    #[test]
    fn line_editing_is_the_editor_not_a_silent_read() {
        let mut shells = vec![(vec!["bash", "--norc", "--noprofile", "-i"], "read -rsn1 k")];
        if std::path::Path::new("/bin/zsh").exists()
            || std::path::Path::new("/usr/bin/zsh").exists()
        {
            shells.push((vec!["zsh", "-f", "-i"], "read -sk1 k"));
        } else {
            eprintln!("zsh not installed; checking bash only");
        }
        for (argv, read) in shells {
            let argv: Vec<String> = argv.into_iter().map(Into::into).collect();
            let mut pty = Pty::spawn(
                &argv,
                &Default::default(),
                None,
                80,
                24,
                (8.0, 16.0),
                |_| {},
                |_| {},
            )
            .unwrap();
            let wait_for = |pty: &mut Pty, editing: bool| {
                let deadline = Instant::now() + Duration::from_secs(10);
                while pty.line_editing() != editing {
                    pty.take_output();
                    assert!(
                        Instant::now() < deadline,
                        "{argv:?}: editing stayed {}",
                        !editing
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
            };
            wait_for(&mut pty, true);
            pty.write(format!("{read}\r").as_bytes());
            wait_for(&mut pty, false);
            pty.write(b"q");
            wait_for(&mut pty, true);
        }
    }

    #[cfg(unix)]
    #[test]
    fn running_program_is_reported_only_away_from_the_shell() {
        let mut pty = Pty::spawn(
            &[
                "bash".into(),
                "--norc".into(),
                "--noprofile".into(),
                "--noediting".into(),
                "-i".into(),
            ],
            &Default::default(),
            None,
            80,
            24,
            (8.0, 16.0),
            |_| {},
            |_| {},
        )
        .unwrap();
        let wait_for = |pty: &mut Pty, running: bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                pty.take_output();
                let program = pty.running_program();
                if program.is_some() == running {
                    return program;
                }
                assert!(Instant::now() < deadline, "running stayed {}", !running);
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait_for(&mut pty, false);
        pty.write(b"sleep 30\r");
        let group = wait_for(&mut pty, true).unwrap();
        assert_eq!(
            crate::close_confirm::process_name(group).as_deref(),
            Some("sleep")
        );
        pty.write(b"\x03");
        wait_for(&mut pty, false);
    }

    #[cfg(unix)]
    #[test]
    fn spawn_starts_in_the_requested_directory() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let mut pty = Pty::spawn(
            &["/bin/sh".into(), "-c".into(), "pwd; sleep 5".into()],
            &Default::default(),
            Some(&dir),
            80,
            24,
            (8.0, 16.0),
            |_| {},
            |_| {},
        )
        .unwrap();
        let expected = format!("{}\r\n", dir.display());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        while !String::from_utf8_lossy(&seen).contains(&expected) {
            assert!(Instant::now() < deadline, "output: {seen:?}");
            seen.extend_from_slice(&pty.take_output().bytes);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Every argv form QA tried on #71 and #60 (as the shell sees it after `env` or a path),
    /// plus the option forms the parser models.
    #[test]
    fn shell_argv_is_idle_only_at_a_prompt() {
        #[rustfmt::skip]
        let cases: &[(bool, &[&str])] = &[
            (true, &["bash"]),
            (true, &["-bash"]),
            (true, &["/opt/homebrew/bin/bash"]),
            (true, &["bash", "--norc"]),
            (true, &["bash", "--norc", "--noprofile", "--noediting", "-i"]),
            (true, &["bash", "-l"]),
            (true, &["bash", "--login"]),
            (true, &["bash", "-i"]),
            (true, &["bash", "-il"]),
            (true, &["bash", "--rcfile", "rc"]),
            (true, &["bash", "--rcfile=rc"]),
            (true, &["bash", "--init-file", "rc", "-i"]),
            (true, &["bash", "-O", "extglob", "-i"]),
            (true, &["bash", "+O", "extglob"]),
            (true, &["bash", "-o", "vi"]),
            (true, &["bash", "-o", "vi", "-i"]),
            (true, &["bash", "+o", "vi"]),
            (true, &["bash", "-io", "vi"]),
            (true, &["bash", "-C"]),
            (true, &["bash", "-Ci"]),
            (true, &["bash", "--"]),
            (true, &["sh"]),
            (true, &["/bin/sh"]),
            (true, &["sh", "-i"]),
            (true, &["dash"]),
            (true, &["dash", "-l"]),
            (true, &["zsh"]),
            (true, &["-zsh"]),
            (true, &["zsh", "-l"]),
            (true, &["/bin/zsh", "-l"]),
            (true, &["zsh", "-C"]),
            (true, &["zsh", "-o", "nonomatch"]),
            (true, &["zsh", "--emulate", "sh"]),
            (true, &["fish"]),
            (true, &["/usr/bin/fish", "-l"]),
            (true, &["fish", "--login"]),
            (true, &["fish", "-i"]),
            (true, &["fish", "-C", "set x 1"]),
            (true, &["fish", "--init-command=set x 1"]),
            (true, &["fish", "--init-command", "set x 1"]),
            (true, &["fish", "--no-config"]),
            (true, &["fish", "-Ctrue"]),
            (true, &["fish", "-iCtrue"]),
            (true, &["fish", "-d3"]),
            (true, &["nu"]),
            (true, &["pwsh", "-l"]),
            (false, &[]),
            (false, &["vim"]),
            (false, &["bashtop"]),
            (false, &["sh", "-c", "sleep 30"]),
            (false, &["/bin/sh", "-c", "sleep 603 | cat"]),
            (false, &["bash", "-c", "sleep 605 && echo done"]),
            (false, &["bash", "-lc", "sleep 607; echo done"]),
            (false, &["bash", "-ic", "make"]),
            (false, &["bash", "-Cc", "make"]),
            (false, &["bash", "-C", "-c", "make"]),
            (false, &["bash", "-i", "-c", "make"]),
            (false, &["bash", "-O", "extglob", "-c", "make"]),
            (false, &["bash", "-o", "vi", "-c", "make"]),
            (false, &["bash", "-o", "pipefail", "-c", "sleep 610 | cat"]),
            (false, &["bash", "--rcfile", "rc", "-c", "make"]),
            (false, &["bash", "--norc", "-c", "make"]),
            (false, &["bash", "s.sh"]),
            (false, &["bash", "/home/u/qa71/h/s.sh"]),
            (false, &["bash", "-o", "vi", "s.sh"]),
            (false, &["bash", "--", "s.sh"]),
            (false, &["bash", "-", "s.sh"]),
            (false, &["bash", "-O"]),
            (false, &["bash", "--rcfile"]),
            (false, &["sh", "s.sh"]),
            (false, &["sh", "./qa71s.sh"]),
            (false, &["dash", "-c", "make"]),
            (false, &["zsh", "-c", "sleep 608; echo done"]),
            (false, &["zsh", "-o", "nonomatch", "-c", "make"]),
            (false, &["zsh", "-C", "-c", "make"]),
            (false, &["zsh", "-lc", "make"]),
            (false, &["zsh", "s.zsh"]),
            (false, &["fish", "-c", "make"]),
            (false, &["fish", "-ic", "make"]),
            (false, &["fish", "--command=make"]),
            (false, &["fish", "--command", "make"]),
            (false, &["fish", "-C", "set x 1", "-c", "make"]),
            (false, &["fish", "s.fish"]),
            (false, &["fish", "-Ctrue", "build.fish"]),
            (false, &["fish", "-d3", "build.fish"]),
            (false, &["fish", "-C", "true", "build.fish"]),
            (false, &["nu", "-c", "ls"]),
            (false, &["pwsh", "-Command", "ls"]),
        ];
        for &(idle, argv) in cases {
            let argv: Vec<String> = argv.iter().map(|&a| a.to_owned()).collect();
            assert_eq!(is_idle_shell(&argv), idle, "{argv:?}");
        }
    }

    #[test]
    fn the_login_shell_stays_idle_under_any_name() {
        let argv = |args: &[&str]| args.iter().map(|&a| a.to_owned()).collect::<Vec<_>>();
        let login = Probe {
            login: Some("-myshell".into()),
            ..Default::default()
        };
        assert!(login.idle(&argv(&["-myshell"])));
        assert!(login.idle(&argv(&["-bash"])));
        assert!(!login.idle(&argv(&["-myshell", "-c", "make"])));
        assert!(!login.idle(&argv(&["vim", "notes.txt"])));
        assert!(!login.idle(&[]));
        let command = Probe::default();
        assert!(!command.idle(&argv(&["-myshell"])));
        assert!(!command.idle(&[]));
    }

    /// A default launch (no `-e`) whose shell `exec`s into a program keeps the shell's pid
    /// and process group, so only the argv check can tell it is no longer at a prompt. The
    /// shell gets a name outside the known shells to check the login argv is idle as well.
    #[cfg(unix)]
    #[test]
    fn a_program_the_default_shell_execs_into_counts_as_running() {
        let dir = std::env::temp_dir().join(format!("terminal-exec-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bash = ["/bin/bash", "/usr/bin/bash", "/opt/homebrew/bin/bash"]
            .into_iter()
            .find(|p| std::path::Path::new(p).exists())
            .unwrap();
        let shell = dir.join("myshell");
        std::os::unix::fs::symlink(bash, &shell).unwrap();
        let env = [
            ("SHELL".to_owned(), shell.to_str().unwrap().to_owned()),
            ("HOME".to_owned(), dir.to_str().unwrap().to_owned()),
        ]
        .into_iter()
        .collect();
        let mut pty = Pty::spawn(&[], &env, None, 80, 24, (8.0, 16.0), |_| {}, |_| {}).unwrap();
        assert_eq!(pty.output.probe.login.as_deref(), Some("-myshell"));
        let wait_for = |pty: &mut Pty, running: bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                pty.take_output();
                let program = pty.running_program();
                let args = program.and_then(crate::close_confirm::process_args);
                let done = match running {
                    true => {
                        args.as_ref().and_then(|a| a.first()).map(String::as_str) == Some("sleep")
                    }
                    false => program.is_none(),
                };
                if done {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "running stayed {}: {args:?}",
                    !running
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait_for(&mut pty, false);
        pty.write(b"exec sleep 30\r");
        wait_for(&mut pty, true);
        drop(pty);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Readline and zle move over and erase emoji sequences one codepoint at a time, so their
    /// echo keeps per-codepoint cells while the same sequence printed by a command joins.
    #[cfg(unix)]
    #[test]
    fn line_editor_echo_keeps_codepoint_cells_and_output_joins() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let bash = ["bash", "--norc", "--noprofile", "-i"];
        line_editor_session(&bash, family);
        line_editor_session(&bash, "\u{1f1ef}\u{1f1f5}");
        if std::process::Command::new("zsh")
            .arg("-fc")
            .arg("true")
            .status()
            .is_ok()
        {
            // zsh draws a ZWJ as `<200d>` unless COMBINING_CHARS is set.
            // No flag: a zsh built with Unicode 9 widths (macOS) counts each regional
            // indicator as two columns where libc counts one, so no single width fits both.
            line_editor_session(&["zsh", "-f", "-i"], "\u{1f44d}\u{1f3fd}");
        } else {
            eprintln!("no zsh on this system; skipping it");
        }
    }

    #[cfg(unix)]
    fn line_editor_session(args: &[&str], sequence: &str) {
        let env = [
            ("LANG".to_owned(), "C.UTF-8".to_owned()),
            ("LC_ALL".to_owned(), "C.UTF-8".to_owned()),
            ("PS1".to_owned(), "$ ".to_owned()),
            // zsh picks the vi keymap from a `vi` editor.
            ("EDITOR".to_owned(), "emacs".to_owned()),
            ("VISUAL".to_owned(), "emacs".to_owned()),
        ]
        .into_iter()
        .collect();
        let args: Vec<String> = args.iter().map(|&a| a.into()).collect();
        let mut pty = Pty::spawn(&args, &env, None, 40, 10, (8.0, 16.0), |_| {}, |_| {}).unwrap();
        let mut term = term_core::Terminal::new(40, 10);
        let row_text = |term: &term_core::Terminal, row: usize| {
            let mut text = String::new();
            for cell in &term.viewport_row(row).cells {
                if !cell.flags.contains(term_core::Flags::WIDE_SPACER) {
                    term.push_cell_text(cell, &mut text);
                }
            }
            text.trim_end().to_owned()
        };
        let pump = |pty: &mut Pty,
                    term: &mut term_core::Terminal,
                    what: &str,
                    done: &dyn Fn(&term_core::Terminal) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let output = pty.take_output();
                term.set_line_editing(output.line_editing);
                if output.line_sent {
                    term.line_sent();
                }
                term.advance(&output.bytes);
                if done(term) && pty.output.pending.lock().unwrap().bytes.is_empty() {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "{args:?} never {what}: {:?} at {:?}",
                    (0..3).map(|row| row_text(term, row)).collect::<Vec<_>>(),
                    term.cursor_position()
                );
                std::thread::sleep(Duration::from_millis(30));
            }
        };
        let cursor = |term: &term_core::Terminal| term.cursor_position().map(|(_, col)| col);
        pump(&mut pty, &mut term, "prompted", &|t| {
            row_text(t, 0) == "$" && cursor(t) == Some(2)
        });
        // As the editor counts them after the 17 columns of `$ printf '%s|\n' `: emoji take
        // two columns, regional indicators one and a ZWJ none.
        let width = |s: &str| {
            let cols = |c: char| match c as u32 {
                0x200d => 0,
                0x1f1e6..=0x1f1ff => 1,
                _ => 2,
            };
            17 + s.chars().map(cols).sum::<usize>()
        };
        let last = sequence.chars().last().unwrap();
        let kept = &sequence[..sequence.len() - last.len_utf8()];
        let end = width(sequence);
        pty.write(format!("printf '%s|\\n' {sequence}").as_bytes());
        pump(&mut pty, &mut term, "echoed", &|t| cursor(t) == Some(end));
        pty.write(b"\x01");
        pump(&mut pty, &mut term, "at the start", &|t| {
            cursor(t) == Some(2)
        });
        pty.write(b"\x05");
        pump(&mut pty, &mut term, "at the end", &|t| {
            cursor(t) == Some(end)
        });
        // Backspace deletes the last codepoint; the screen agrees.
        pty.write(b"\x7f");
        pump(&mut pty, &mut term, "erased", &|t| {
            cursor(t) == Some(width(kept))
        });
        assert_eq!(row_text(&term, 0), format!("$ printf '%s|\\n' {kept}"));
        pty.write(format!("{last}\r").as_bytes());
        pump(&mut pty, &mut term, "printed", &|t| {
            row_text(t, 2) == "$" && cursor(t) == Some(2)
        });
        let row = term.viewport_row(1);
        assert_eq!(term.grapheme(&row.cells[0]), Some(sequence), "{args:?}");
        assert_eq!(term.base_char(&row.cells[2]), '|');
    }

    #[cfg(unix)]
    #[test]
    fn only_the_shell_at_its_prompt_is_line_editing() {
        let mut pty = Pty::spawn(
            &[
                "bash".into(),
                "--norc".into(),
                "--noprofile".into(),
                "-i".into(),
            ],
            &Default::default(),
            None,
            80,
            24,
            (8.0, 16.0),
            |_| {},
            |_| {},
        )
        .unwrap();
        let wait_for = |pty: &mut Pty, check: &dyn Fn(&Pty) -> bool, what: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !check(pty) {
                assert!(Instant::now() < deadline, "never {what}");
                pty.take_output();
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait_for(&mut pty, &Pty::line_editing, "line editing at the prompt");
        // A cbreak program sets the editor's modes but does not redraw on a resize. This is
        // Python 3.12's `tty.setcbreak`, which also stops mapping CR to NL as readline does.
        pty.write(
            b"python3 -c 'import termios as t, time; a = t.tcgetattr(0); a[0] &= ~t.ICRNL; \
              a[3] &= ~(t.ECHO | t.ICANON); t.tcsetattr(0, t.TCSANOW, a); time.sleep(30)'\r",
        );
        wait_for(
            &mut pty,
            &|pty| pty.editor_modes() && pty.running_program().is_some(),
            "in cbreak mode",
        );
        assert!(!pty.line_editing());
        pty.write(b"\x03");
        wait_for(
            &mut pty,
            &Pty::line_editing,
            "line editing after the program",
        );
        // A shell started from the shell edits its own line.
        pty.write(b"bash --norc --noprofile -i\r");
        wait_for(
            &mut pty,
            &|pty| pty.line_editing() && pty.running_program().is_some(),
            "line editing in a nested shell",
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_password_read_is_password_input() {
        let mut pty = Pty::spawn(
            &[
                "bash".into(),
                "--norc".into(),
                "--noprofile".into(),
                "-i".into(),
            ],
            &Default::default(),
            None,
            80,
            24,
            (8.0, 16.0),
            |_| {},
            |_| {},
        )
        .unwrap();
        let wait_for = |pty: &mut Pty, check: &dyn Fn(&Pty) -> bool, what: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !check(pty) {
                assert!(Instant::now() < deadline, "never {what}");
                pty.take_output();
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        wait_for(&mut pty, &Pty::line_editing, "line editing at the prompt");
        assert!(!pty.password_input());
        pty.write(b"python3 -c 'import getpass; getpass.getpass()'\r");
        wait_for(&mut pty, &Pty::password_input, "reading a password");
        pty.write(b"hunter2\r");
        wait_for(
            &mut pty,
            &|pty| pty.line_editing() && !pty.password_input(),
            "back at the prompt",
        );
        // A silent `read -s` is a password read too; a cbreak `read -sn1` is not.
        pty.write(b"read -s secret\r");
        wait_for(&mut pty, &Pty::password_input, "in read -s");
        pty.write(b"x\r");
        wait_for(&mut pty, &Pty::line_editing, "line editing after read -s");
        pty.write(b"read -sn1 key\r");
        let cbreak_read = |pty: &Pty| {
            let fd = pty.master.as_raw_fd().unwrap();
            let mut attrs = std::mem::MaybeUninit::<libc::termios>::uninit();
            // SAFETY: fd belongs to the live master; tcgetattr initializes attrs on success.
            assert_eq!(unsafe { libc::tcgetattr(fd, attrs.as_mut_ptr()) }, 0);
            // SAFETY: tcgetattr succeeded.
            let attrs = unsafe { attrs.assume_init() };
            attrs.c_lflag & (libc::ICANON | libc::ECHO) == 0
                && attrs.c_iflag & (libc::ICRNL | libc::INLCR) == libc::ICRNL
        };
        wait_for(&mut pty, &cbreak_read, "in read -sn1");
        assert!(!pty.password_input());
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_running_a_command_counts_as_running() {
        for shell in ["sh", "bash"] {
            let mut pty = Pty::spawn(
                &[shell.into(), "-c".into(), "sleep 30; true".into()],
                &Default::default(),
                None,
                80,
                24,
                (8.0, 16.0),
                |_| {},
                |_| {},
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            // Before exec the child still carries the test binary's arguments.
            loop {
                pty.take_output();
                let args = pty
                    .running_program()
                    .and_then(crate::close_confirm::process_args);
                if args.as_ref().and_then(|a| a.first()).map(String::as_str) == Some(shell) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "{shell} -c never counted as running: {args:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    #[test]
    fn a_direct_command_counts_as_running() {
        assert_eq!(shell_name("/bin/bash"), Some("bash"));
        assert_eq!(shell_name("-zsh"), Some("zsh"));
        assert!(shell_name("vim").is_none() && shell_name("/usr/bin/ssh").is_none());
        let mut wrapped = Pty::spawn(
            &[
                "env".into(),
                "bash".into(),
                "--norc".into(),
                "--noprofile".into(),
                "-i".into(),
            ],
            &Default::default(),
            None,
            80,
            24,
            (8.0, 16.0),
            |_| {},
            |_| {},
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while wrapped.running_program().is_some() {
            wrapped.take_output();
            assert!(
                Instant::now() < deadline,
                "env bash still counted as running"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut pty = Pty::spawn(
            &["sleep".into(), "30".into()],
            &Default::default(),
            None,
            80,
            24,
            (8.0, 16.0),
            |_| {},
            |_| {},
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        // Before exec the child still carries the test thread's name.
        loop {
            pty.take_output();
            let name = pty
                .running_program()
                .and_then(crate::close_confirm::process_name);
            if name.as_deref() == Some("sleep") {
                break;
            }
            assert!(Instant::now() < deadline, "sleep never counted as running");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn failed_startup_returns_without_a_watcher() {
        assert!(
            Pty::spawn(
                &["/nonexistent/terminal-test-shell".into()],
                &Default::default(),
                None,
                80,
                24,
                (8.0, 16.0),
                |_| {},
                |_| panic!("failed startup"),
            )
            .is_err()
        );
    }

    #[test]
    fn foreground_reset_requires_a_return_from_away() {
        let mut foreground = Foreground::default();
        assert!(!foreground.sample(None));
        assert!(!foreground.sample(Some(true)));
        assert!(!foreground.sample(Some(false)));
        assert!(!foreground.sample(None));
        assert!(!foreground.sample(Some(false)));
        assert!(foreground.sample(Some(true)));
        assert!(!foreground.sample(Some(true)));
    }

    fn read(pending: &mut Pending, at: Instant, bytes: &[u8], shell: Option<bool>) -> bool {
        let checked = pending.check_foreground(at, bytes, || shell);
        pending.append(bytes, checked, false);
        checked
    }

    #[test]
    fn line_editing_starts_after_the_last_read_without_its_modes() {
        let mut pending = Pending::default();
        pending.append(b"edit", true, true);
        pending.append(b"output\n", true, false);
        pending.append(b"prompt", true, true);
        pending.append(b"echo", true, true);
        assert_eq!(
            pending.take_output(Instant::now(), || None).line_editing,
            Some(11)
        );
        pending.append(b"edit", true, true);
        pending.append(b"output", true, false);
        assert_eq!(
            pending.take_output(Instant::now(), || None).line_editing,
            None
        );
    }

    #[test]
    fn queue_offsets_follow_chunks_and_restart_after_drain() {
        let mut pending = Pending::default();
        let start = Instant::now();
        let at = |step: u32| start + FOREGROUND_INTERVAL * step;
        read(&mut pending, at(0), b"away\n", Some(false));
        read(&mut pending, at(1), b"prompt", Some(true));
        read(&mut pending, at(2), b"away", Some(false));
        read(&mut pending, at(3), b"prompt", Some(true));
        let output = pending.take_output(at(3), || panic!("every read was checked"));
        assert_eq!(output.bytes, b"away\npromptawayprompt");
        assert_eq!(output.shell_returns, [5, 15]);
        assert_eq!(pending.lines, 0);
        let empty = pending.take_output(at(3), || panic!("nothing was read"));
        assert!(empty.bytes.is_empty());
        assert!(empty.shell_returns.is_empty());
        read(&mut pending, at(4), b"away", Some(false));
        pending.take_output(at(4), || panic!("every read was checked"));
        read(&mut pending, at(5), b"prompt", Some(true));
        assert_eq!(pending.take_output(at(5), || None).shell_returns, [0]);
    }

    #[test]
    fn queued_command_output_is_checked_once_per_interval() {
        let mut pending = Pending::default();
        let start = Instant::now();
        let soon = FOREGROUND_INTERVAL / 4;
        assert!(read(&mut pending, start, b"cmd", Some(false)));
        assert!(!read(&mut pending, start + soon, b"out", Some(true)));
        assert!(!read(&mut pending, start + soon * 2, b"prompt", Some(true)));
        assert!(read(
            &mut pending,
            start + FOREGROUND_INTERVAL,
            b"$ ",
            Some(true)
        ));
        let output = pending.take_output(start + FOREGROUND_INTERVAL, || {
            panic!("the last read was checked")
        });
        assert_eq!(output.bytes, b"cmdoutprompt$ ");
        assert_eq!(output.shell_returns, [3]);
    }

    #[test]
    fn taking_unchecked_output_marks_a_return_before_it() {
        let mut pending = Pending::default();
        let start = Instant::now();
        read(&mut pending, start, b"cmd", Some(false));
        read(&mut pending, start, b"out", Some(false));
        read(&mut pending, start, b"prompt", Some(true));
        let output = pending.take_output(start, || Some(true));
        assert_eq!(output.bytes, b"cmdoutprompt");
        assert_eq!(output.shell_returns, [3]);
        assert!(read(&mut pending, start, b"$ ", Some(true)));
        assert!(pending.take_output(start, || None).shell_returns.is_empty());
    }

    #[test]
    fn reads_are_checked_whenever_nothing_is_queued() {
        let mut pending = Pending::default();
        let start = Instant::now();
        assert!(read(&mut pending, start, b"$ ", Some(true)));
        assert!(!read(&mut pending, start, b"ls", Some(false)));
        let output = pending.take_output(start, || Some(false));
        assert!(output.shell_returns.is_empty());
        assert!(read(&mut pending, start, b"out", Some(false)));
        assert!(!read(&mut pending, start, b"more", Some(true)));
        assert_eq!(pending.take_output(start, || Some(true)).shell_returns, [3]);
    }

    #[test]
    fn a_command_started_between_checks_still_marks_its_return() {
        let mut pending = Pending::default();
        let start = Instant::now();
        read(&mut pending, start, b"$ ", Some(true));
        read(&mut pending, start, b"vim", Some(false));
        assert!(
            pending
                .take_output(start, || Some(false))
                .shell_returns
                .is_empty()
        );
        read(&mut pending, start, b"screen", Some(false));
        read(&mut pending, start, b"$ ", Some(true));
        assert_eq!(pending.take_output(start, || Some(true)).shell_returns, [6]);
    }

    #[test]
    fn an_unavailable_group_neither_marks_nor_forgets_a_command() {
        let mut pending = Pending::default();
        let start = Instant::now();
        read(&mut pending, start, b"cmd", Some(false));
        read(&mut pending, start, b"out", None);
        assert!(pending.take_output(start, || None).shell_returns.is_empty());
        read(&mut pending, start, b"prompt", Some(true));
        assert_eq!(pending.take_output(start, || Some(true)).shell_returns, [0]);
    }

    const MODES: &[u8] = b"\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b[?2004h";

    fn apply(output: &Output) -> term_core::Terminal {
        let mut term = term_core::Terminal::new(20, 4);
        let mut start = 0;
        for &offset in &output.shell_returns {
            term.advance(&output.bytes[start..offset]);
            term.reset_input_modes();
            start = offset;
        }
        term.advance(&output.bytes[start..]);
        term
    }

    fn input_modes_on(term: &term_core::Terminal) -> bool {
        term.modes.mouse != term_core::MouseMode::Off
            || term.modes.sgr_mouse
            || term.modes.focus_events
            || term.modes.bracketed_paste
    }

    #[test]
    fn modes_a_command_sets_at_the_end_of_a_flood_are_reset() {
        let mut pending = Pending::default();
        let start = Instant::now();
        let soon = FOREGROUND_INTERVAL / 4;
        assert!(read(&mut pending, start, b"flood\n", Some(false)));
        assert!(!read(&mut pending, start + soon, b"flood\n", Some(false)));
        assert!(read(&mut pending, start + soon * 2, MODES, Some(false)));
        assert!(read(&mut pending, start + soon * 3, b"$ ", Some(true)));
        read(&mut pending, start + soon * 3, b"ls", Some(false));
        let output = pending.take_output(start + soon * 3, || Some(false));
        assert_eq!(output.shell_returns, [12 + MODES.len()]);
        assert!(!input_modes_on(&apply(&output)));
    }

    #[test]
    fn a_read_finishing_an_escape_sequence_is_checked() {
        let mut pending = Pending::default();
        let start = Instant::now();
        let (head, rest) = MODES.split_at(MODES.len() - 6);
        let (middle, tail) = rest.split_at(3);
        assert!(read(&mut pending, start, b"flood\n", Some(false)));
        assert!(read(&mut pending, start, head, Some(false)));
        assert!(read(&mut pending, start, middle, Some(false)));
        assert!(read(&mut pending, start, tail, Some(false)));
        assert!(read(&mut pending, start, b"$ ", Some(true)));
        let output = pending.take_output(start, || panic!("the last read was checked"));
        assert_eq!(output.shell_returns, [6 + MODES.len()]);
        assert!(!input_modes_on(&apply(&output)));
    }

    #[test]
    fn plain_output_well_past_an_escape_is_throttled_again() {
        let mut pending = Pending::default();
        let start = Instant::now();
        assert!(read(&mut pending, start, MODES, Some(false)));
        assert!(read(&mut pending, start, &[b'x'; ESCAPE_TAIL], Some(false)));
        assert!(!read(&mut pending, start, b"x", Some(false)));
    }

    #[test]
    fn a_prompt_enabling_paste_after_a_flood_keeps_it() {
        let mut pending = Pending::default();
        let start = Instant::now();
        read(&mut pending, start, MODES, Some(false));
        read(&mut pending, start, b"flood\n", Some(false));
        read(&mut pending, start, b"flood\n", Some(true));
        assert!(read(&mut pending, start, b"\x1b[?2004h$ ", Some(true)));
        let output = pending.take_output(start, || panic!("the last read was checked"));
        assert_eq!(output.shell_returns, [MODES.len() + 6]);
        let term = apply(&output);
        assert!(term.modes.bracketed_paste);
        assert_eq!(term.modes.mouse, term_core::MouseMode::Off);
        assert!(!term.modes.focus_events);
    }
}
