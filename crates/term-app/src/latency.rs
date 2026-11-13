use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
#[path = "present_macos.rs"]
mod macos;
#[cfg(unix)]
#[path = "latency_signals.rs"]
mod signals;
#[cfg(target_os = "linux")]
#[path = "present_x11.rs"]
mod x11;

pub struct Latency {
    path: PathBuf,
    start: Instant,
    pending: Vec<Instant>,
    echoed: Vec<Instant>,
    lines: Vec<String>,
    pub deadline: Option<Instant>,
    remaining: usize,
    sent: usize,
    random: u32,
    sequence: usize,
    feedback: Arc<Mutex<Vec<String>>>,
    #[cfg(unix)]
    signals: Option<signals::Signals>,
}

impl Latency {
    pub fn from_env(refresh: u32) -> Option<Self> {
        let path = std::env::var_os("TERM_LATENCY_LOG")?;
        let count = std::env::var("TERM_LATENCY_PROBE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let now = Instant::now();
        let duration = std::env::var("TERM_LATENCY_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|s| *s > 0);
        Some(Self {
            path: path.into(),
            start: now,
            pending: Vec::new(),
            echoed: Vec::new(),
            lines: vec![format!("refresh_mhz {refresh}")],
            deadline: if count > 0 {
                Some(now + Duration::from_secs(1))
            } else {
                duration.map(|s| now + Duration::from_secs(s))
            },
            remaining: count,
            sent: 0,
            random: 1,
            sequence: 0,
            feedback: Arc::default(),
            #[cfg(unix)]
            signals: match signals::Signals::install() {
                Ok(signals) => Some(signals),
                Err(error) => {
                    eprintln!("terminal: latency signals unavailable: {error}");
                    None
                }
            },
        })
    }

    pub fn input(&mut self, now: Instant) {
        self.pending.push(now);
        self.lines.push(format!(
            "input {}",
            now.duration_since(self.start).as_micros()
        ));
    }

    pub fn feedback(&mut self, window: Arc<winit::window::Window>) {
        #[cfg(target_os = "linux")]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if !matches!(
                window.window_handle().map(|h| h.as_raw()),
                Ok(RawWindowHandle::Wayland(_))
            ) && let Err(error) = x11::start(&window, self.start, self.feedback.clone())
            {
                eprintln!("terminal: presentation feedback unavailable: {error}");
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = window;
    }

    #[cfg(target_os = "linux")]
    pub fn wayland_log(&self) -> (Instant, Arc<Mutex<Vec<String>>>) {
        (self.start, self.feedback.clone())
    }

    #[cfg(target_os = "macos")]
    pub fn metal_feedback(&self, device: &wgpu::Device) {
        if let Err(error) = macos::install(device, self.start, self.feedback.clone()) {
            eprintln!("terminal: presentation feedback unavailable: {error}");
        }
    }

    pub fn output(&mut self, bytes: usize) {
        if bytes > 0 {
            self.echoed.append(&mut self.pending);
        }
    }

    pub fn frame(&mut self, now: Instant, bytes: usize) {
        self.lines.push(format!(
            "frame {} {} {bytes}",
            self.sequence,
            now.duration_since(self.start).as_micros(),
        ));
        for input in self.echoed.drain(..) {
            self.lines.push(format!(
                "echo {} {}",
                input.duration_since(self.start).as_micros(),
                self.sequence
            ));
        }
        self.sequence += 1;
    }

    pub fn probe(&mut self, now: Instant) -> Option<&'static [u8]> {
        if self.deadline.is_none_or(|deadline| now < deadline) || self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        self.sent += 1;
        self.random = self.random.wrapping_mul(1664525).wrapping_add(1013904223);
        self.deadline = Some(
            now + if self.remaining == 0 {
                Duration::from_secs(1)
            } else {
                Duration::from_millis(40 + u64::from(self.random % 81))
            },
        );
        Some(if self.sent.is_multiple_of(40) {
            b"\r"
        } else {
            b"x"
        })
    }

    pub fn finished(&self, now: Instant) -> bool {
        #[cfg(unix)]
        if self.signals.as_ref().is_some_and(|s| s.terminated()) {
            return true;
        }
        self.remaining == 0 && self.deadline.is_some_and(|d| now >= d)
    }

    pub fn wake_deadline(&self, now: Instant) -> Option<Instant> {
        #[cfg(unix)]
        if self.signals.is_some() {
            let poll = now + Duration::from_millis(25);
            return Some(self.deadline.map_or(poll, |deadline| deadline.min(poll)));
        }
        self.deadline
    }
}

impl Drop for Latency {
    fn drop(&mut self) {
        self.lines.extend(self.feedback.lock().unwrap().drain(..));
        if let Err(error) = std::fs::write(&self.path, self.lines.join("\n")) {
            eprintln!("terminal: latency log: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_nonempty_output_matches_inputs_to_the_next_present() {
        let now = Instant::now();
        let mut log = Latency {
            path: PathBuf::from("/dev/null"),
            start: now,
            pending: vec![],
            echoed: vec![],
            lines: vec![],
            deadline: None,
            remaining: 0,
            sent: 0,
            random: 1,
            sequence: 0,
            feedback: Arc::default(),
            #[cfg(unix)]
            signals: None,
        };
        log.frame(now, 0);
        log.input(now);
        log.output(0);
        log.frame(now, 0);
        assert_eq!(log.pending.len(), 1);
        log.output(1);
        log.input(now);
        log.frame(now + Duration::from_micros(10), 1);
        assert_eq!(log.lines.last().unwrap(), "echo 0 2");
        assert!(log.lines.iter().any(|line| line == "input 0"));
        assert!(log.lines.iter().any(|line| line == "frame 2 10 1"));
        assert_eq!(log.pending.len(), 1);
        assert!(log.echoed.is_empty());
        assert!(log.probe(now).is_none());
        assert!(!log.finished(now));
    }
}
