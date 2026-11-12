//! Spawning the shell on a pseudo-terminal and pumping its output to the UI thread.

use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex};

/// Output the reader thread may queue before it waits for the UI, which is also the most
/// the UI parses per wake-up, so huge bursts can't starve rendering.
const MAX_QUEUED: usize = 1 << 20;

/// Shell output read but not yet parsed, as one buffer: small reads (macOS returns about
/// 1 KiB per read) still reach the parser as one batch.
#[derive(Default)]
struct Queue {
    bytes: Mutex<(Vec<u8>, bool)>,
    room: Condvar,
}

pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    /// Input queued for the writer thread, so a full PTY never blocks the UI.
    input: Sender<Vec<u8>>,
    child: Box<dyn Child + Send + Sync>,
    output: Arc<Queue>,
    /// Set while a wake-up is queued, so a burst of output wakes the UI only once.
    pub wake_pending: Arc<AtomicBool>,
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
        cols: usize,
        rows: usize,
        cell: (f32, f32),
        wake: impl Fn(bool) + Send + 'static,
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

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("cannot read pty: {e}"))?;
        let mut writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("cannot write pty: {e}"))?;
        let (input, input_rx) = channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("pty-writer".into())
            .spawn(move || {
                for bytes in input_rx {
                    if writer
                        .write_all(&bytes)
                        .and_then(|_| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|e| format!("cannot start writer thread: {e}"))?;

        // Bounded, so a shell producing output faster than we parse it is paused by
        // the kernel instead of growing this queue without limit.
        let output = Arc::new(Queue::default());
        let queue = output.clone();
        let wake_pending = Arc::new(AtomicBool::new(false));
        let pending = wake_pending.clone();
        std::thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || {
                let mut buf = vec![0u8; 64 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut bytes = queue.bytes.lock().unwrap();
                            while bytes.0.len() >= MAX_QUEUED && !bytes.1 {
                                bytes = queue.room.wait(bytes).unwrap();
                            }
                            if bytes.1 {
                                return;
                            }
                            bytes.0.extend_from_slice(&buf[..n]);
                            drop(bytes);
                            if !pending.swap(true, Ordering::AcqRel) {
                                wake(false);
                            }
                        }
                    }
                }
                wake(true);
            })
            .map_err(|e| format!("cannot start reader thread: {e}"))?;

        Ok(Pty {
            master: pair.master,
            input,
            child,
            output,
            wake_pending,
        })
    }

    /// Everything the shell wrote since the last call.
    pub fn take_output(&self) -> Vec<u8> {
        let bytes = std::mem::take(&mut self.output.bytes.lock().unwrap().0);
        self.output.room.notify_one();
        bytes
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let _ = self.input.send(bytes.to_vec());
    }

    pub fn resize(&self, cols: usize, rows: usize, cell: (f32, f32)) {
        let _ = self.master.resize(size(cols, rows, cell.0, cell.1));
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.output.bytes.lock().unwrap().1 = true;
        self.output.room.notify_one();
        let _ = self.child.kill();
    }
}
