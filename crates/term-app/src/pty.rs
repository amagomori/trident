//! Spawning the shell on a pseudo-terminal and pumping its output to the UI thread.

use crate::flow_control::FlowSettings;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

/// Output the reader thread queues before it waits for the UI, once it also holds
/// `MIN_LINES` lines. That many lines is well past a screen plus default scrollback, so a
/// flood's batch is long enough to skip building the lines it evicts, even with long lines.
const MIN_QUEUED: usize = 1 << 20;
const MIN_LINES: usize = 32 * 1024;
/// The most the reader queues whatever the line count, which is also the most the UI
/// parses per wake-up, so huge bursts can't starve rendering.
const MAX_QUEUED: usize = 8 << 20;
const READ_BUFFER: usize = 64 * 1024;

/// Shell output read but not yet parsed, as one buffer: small reads (macOS returns about
/// 1 KiB per read) still reach the parser as one batch.
#[derive(Default)]
struct Queue {
    read_sequence: AtomicU64,
    read_bytes: AtomicU64,
    pending: Mutex<Pending>,
    room: Condvar,
}

#[derive(Default)]
struct Pending {
    sequence: u64,
    read_bytes: u64,
    bytes: Vec<u8>,
    shell_returns: Vec<usize>,
    lines: usize,
    closed: bool,
}

impl Pending {
    fn append(&mut self, bytes: &[u8], shell_returned: bool) {
        if shell_returned {
            self.shell_returns.push(self.bytes.len());
        }
        self.bytes.extend_from_slice(bytes);
        self.lines += bytes.iter().filter(|&&b| b == b'\n').count();
    }

    fn take_output(&mut self) -> Output {
        self.lines = 0;
        Output {
            bytes: std::mem::take(&mut self.bytes),
            shell_returns: std::mem::take(&mut self.shell_returns),
        }
    }

    fn full(&self) -> bool {
        self.bytes.len() >= MAX_QUEUED
            || (self.bytes.len() >= MIN_QUEUED && self.lines >= MIN_LINES)
    }
}

pub struct Output {
    pub bytes: Vec<u8>,
    pub shell_returns: Vec<usize>,
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
    watcher: Option<crate::command_notify::Watcher>,
    finished: Arc<dyn Fn(crate::command_notify::Finished) + Send + Sync>,
}

struct Input {
    bytes: Vec<u8>,
    pause_boundary: Option<Arc<AtomicU64>>,
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

fn size(cols: usize, rows: usize, cell_w: f32, cell_h: f32) -> PtySize {
    PtySize {
        rows: rows as u16,
        cols: cols as u16,
        pixel_width: (cols as f32 * cell_w) as u16,
        pixel_height: (rows as f32 * cell_h) as u16,
    }
}

impl Pty {
    /// Starts `command` (the user's login shell when empty). `wake` is called from the
    /// reader thread when output is ready, and with `true` once the shell exits.
    pub fn spawn(
        command: &[String],
        env: &std::collections::BTreeMap<String, String>,
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
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", env!("CARGO_BIN_NAME"));
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        for (key, value) in env {
            cmd.env(key, value);
        }
        if let Some(home) = std::env::var_os("HOME")
            && std::env::current_dir()
                .map(|d| d == std::path::Path::new("/"))
                .unwrap_or(true)
        {
            cmd.cwd(home);
        }
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

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("cannot read pty: {e}"))?;
        let watcher_stopped = Arc::new(AtomicBool::new(false));
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("cannot write pty: {e}"))?;
        let output = Arc::new(Queue::default());
        let writer_queue = output.clone();
        #[cfg(unix)]
        let writer_fd = foreground_fd
            .as_ref()
            .map(|fd| fd.try_clone())
            .transpose()
            .map_err(|e| format!("cannot clone pty: {e}"))?;
        let (input, input_rx) = channel::<Input>();
        std::thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                for input in input_rx {
                    if writer
                        .write_all(&input.bytes)
                        .and_then(|_| writer.flush())
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
        std::thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; READ_BUFFER];
                let mut foreground = Foreground::default();
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let sequence = queue.read_sequence.fetch_add(1, Ordering::SeqCst) + 1;
                            let read_bytes =
                                queue.read_bytes.fetch_add(n as u64, Ordering::SeqCst) + n as u64;
                            #[cfg(unix)]
                            let shell = foreground_fd.as_ref().and_then(|fd| {
                                let group = unsafe { libc::tcgetpgrp(fd.as_raw_fd()) };
                                shell_pid
                                    .filter(|_| group > 0)
                                    .map(|pid| group as u32 == pid)
                            });
                            #[cfg(not(unix))]
                            let shell = {
                                let _ = shell_pid;
                                None
                            };
                            let shell_returned = foreground.sample(shell);
                            let mut p = queue.pending.lock().unwrap();
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
                            p.append(&buf[..n], shell_returned);
                            drop(p);
                            if !pending.swap(true, Ordering::AcqRel) {
                                wake(false);
                            }
                        }
                    }
                }
                reader_stopped.store(true, Ordering::Release);
                wake(true);
            })
            .map_err(|e| format!("cannot start reader thread: {e}"))?;

        Ok(Pty {
            flow: Default::default(),
            flow_redraw: false,
            master: pair.master,
            input,
            pause_boundary: None,
            child,
            output,
            wake_pending,
            watcher_stopped,
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
                        move |event| finished(event),
                    )
                    .map_err(|e| format!("cannot start command watcher: {e}"))?,
                );
            }
        }
        Ok(())
    }

    /// Everything the shell wrote since the last call.
    pub fn take_output(&mut self) -> Output {
        let mut p = self.output.pending.lock().unwrap();
        let output = p.take_output();
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
        if bytes.is_empty() {
            return;
        }
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
        self.output.pending.lock().unwrap().closed = true;
        self.output.room.notify_one();
        let _ = self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn notifications_start_disabled_and_can_be_reloaded() {
        let mut pty = Pty::spawn(
            &["/bin/sh".into()],
            &Default::default(),
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

    #[test]
    fn failed_startup_returns_without_a_watcher() {
        assert!(
            Pty::spawn(
                &["/nonexistent/terminal-test-shell".into()],
                &Default::default(),
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

    #[test]
    fn queue_offsets_follow_chunks_and_restart_after_drain() {
        let mut pending = Pending::default();
        pending.append(b"away\n", false);
        pending.append(b"prompt", true);
        pending.append(b"away", false);
        pending.append(b"prompt", true);
        let output = pending.take_output();
        assert_eq!(output.bytes, b"away\npromptawayprompt");
        assert_eq!(output.shell_returns, [5, 15]);
        assert_eq!(pending.lines, 0);
        let empty = pending.take_output();
        assert!(empty.bytes.is_empty());
        assert!(empty.shell_returns.is_empty());
        pending.append(b"prompt", true);
        assert_eq!(pending.take_output().shell_returns, [0]);
    }
}
