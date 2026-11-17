//! Runs a program on a pseudo-terminal with term-core as the terminal, for conformance
//! suites such as vttest and esctest (see docs/conformance.md).
//!
//! `vtrun [--cols N] [--rows N] [--idle MS] [--key K]... -- program args...`
//!
//! Replies (DA, DSR, DECRQCRA, ...) go back to the program as the app sends them. Each
//! `--key` is typed once output has been idle for `--idle` ms (default 300), after the
//! screen is printed to stdout; `\r`, `\e` and `\xNN` escapes are decoded. The final
//! screen is printed when the program exits or the keys run out and output goes idle.

#[cfg(unix)]
fn main() {
    unix::main();
}

#[cfg(not(unix))]
fn main() {
    eprintln!("vtrun needs a Unix pseudo-terminal");
}

#[cfg(unix)]
mod unix {
    use std::ffi::CString;
    use std::time::{Duration, Instant};

    use term_core::Terminal;

    fn unescape(s: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' && i + 1 < b.len() {
                i += 1;
                match b[i] {
                    b'r' => out.push(b'\r'),
                    b'n' => out.push(b'\n'),
                    b'e' => out.push(0x1b),
                    b'\\' => out.push(b'\\'),
                    b'x' if i + 2 < b.len() => {
                        let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
                        out.push(u8::from_str_radix(hex, 16).unwrap_or(b'?'));
                        i += 2;
                    }
                    c => out.push(c),
                }
            } else {
                out.push(b[i]);
            }
            i += 1;
        }
        out
    }

    fn dump(t: &Terminal, step: usize) {
        println!("=== screen {step} cursor {:?}", t.cursor_position());
        for row in 0..t.rows() {
            let text: String = t.viewport_row(row).cells.iter().map(|c| c.c).collect();
            println!("|{}|", text.trim_end());
        }
    }

    pub fn main() {
        let mut args = std::env::args().skip(1);
        let (mut cols, mut rows, mut idle) = (80usize, 24usize, 300u64);
        let mut keys = Vec::new();
        let mut program = Vec::new();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--cols" => cols = args.next().and_then(|v| v.parse().ok()).unwrap_or(cols),
                "--rows" => rows = args.next().and_then(|v| v.parse().ok()).unwrap_or(rows),
                "--idle" => idle = args.next().and_then(|v| v.parse().ok()).unwrap_or(idle),
                "--key" => keys.push(unescape(&args.next().unwrap_or_default())),
                "--" => {
                    program.extend(args.by_ref());
                    break;
                }
                _ => {
                    program.push(arg);
                    program.extend(args.by_ref());
                    break;
                }
            }
        }
        if program.is_empty() {
            eprintln!(
                "usage: vtrun [--cols N] [--rows N] [--idle MS] [--key K]... -- program args..."
            );
            std::process::exit(2);
        }
        let mut t = Terminal::new(cols, rows);
        t.set_checksum_reports(true);
        let (master, child) = spawn(&program, cols, rows);
        let mut keys = keys.into_iter();
        let mut buf = vec![0u8; 65536];
        let mut last_output = Instant::now();
        let mut step = 0;
        loop {
            let mut fds = [libc::pollfd {
                fd: master,
                events: libc::POLLIN,
                revents: 0,
            }];
            // SAFETY: fds contains one initialized pollfd, writable for this call.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, 20) };
            if ready > 0 {
                // SAFETY: buf is writable for its full length; read retains no pointer.
                let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                t.advance(&buf[..n as usize]);
                let replies = t.take_responses();
                write_all(master, &replies);
                last_output = Instant::now();
                continue;
            }
            if last_output.elapsed() >= Duration::from_millis(idle) {
                dump(&t, step);
                step += 1;
                match keys.next() {
                    Some(key) => {
                        write_all(master, &key);
                        last_output = Instant::now();
                    }
                    None => break,
                }
            }
        }
        dump(&t, step);
        // SAFETY: child is this example's unreaped child PID, and null wait status
        // requests no output. The master descriptor is no longer used after close.
        unsafe {
            libc::kill(child, libc::SIGHUP);
            let deadline = Instant::now() + Duration::from_millis(500);
            while libc::waitpid(child, std::ptr::null_mut(), libc::WNOHANG) == 0 {
                if Instant::now() >= deadline {
                    libc::kill(child, libc::SIGKILL);
                    libc::waitpid(child, std::ptr::null_mut(), 0);
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn write_all(fd: i32, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            // SAFETY: bytes is readable for the supplied length during the call.
            let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
            if n <= 0 {
                return;
            }
            bytes = &bytes[n as usize..];
        }
    }

    fn spawn(program: &[String], cols: usize, rows: usize) -> (i32, i32) {
        let (mut master, mut slave) = (0, 0);
        let mut size = libc::winsize {
            ws_row: rows as u16,
            ws_col: cols as u16,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let argv: Vec<CString> = program
            .iter()
            .map(|a| CString::new(a.as_str()).unwrap())
            .collect();
        let mut ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // SAFETY: openpty receives initialized writable outputs and a winsize; the
        // descriptors stay open until duplicated/closed. argv owns NUL-terminated
        // strings and ptrs has a null sentinel for execvp. The single-threaded child
        // calls libc only after fork and uses _exit if exec fails.
        unsafe {
            if libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut size,
            ) != 0
            {
                panic!("openpty failed");
            }
            let pid = libc::fork();
            if pid == 0 {
                libc::close(master);
                libc::setsid();
                libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
                libc::dup2(slave, 0);
                libc::dup2(slave, 1);
                libc::dup2(slave, 2);
                if slave > 2 {
                    libc::close(slave);
                }
                libc::execvp(ptrs[0], ptrs.as_ptr());
                libc::_exit(127);
            }
            libc::close(slave);
            (master, pid)
        }
    }
}
