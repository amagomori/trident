use std::path::PathBuf;
use std::time::{Duration, Instant};

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
    inputs: usize,
}

impl Latency {
    pub fn from_env(refresh: u32) -> Option<Self> {
        let path = std::env::var_os("TERM_LATENCY_LOG")?;
        let count = std::env::var("TERM_LATENCY_PROBE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let now = Instant::now();
        Some(Self {
            path: path.into(),
            start: now,
            pending: Vec::new(),
            echoed: Vec::new(),
            lines: vec![format!("refresh_mhz {refresh}")],
            deadline: (count > 0).then_some(now + Duration::from_secs(1)),
            remaining: count,
            sent: 0,
            random: 1,
            inputs: 0,
        })
    }

    pub fn input(&mut self, now: Instant) {
        self.pending.push(now);
        self.inputs += 1;
    }

    pub fn output(&mut self, bytes: usize) {
        if bytes > 0 {
            self.echoed.append(&mut self.pending);
        }
    }

    pub fn frame(&mut self, now: Instant, bytes: usize) {
        self.lines.push(format!(
            "frame {} {bytes} {}",
            now.duration_since(self.start).as_micros(),
            self.inputs
        ));
        self.inputs = 0;
        for input in self.echoed.drain(..) {
            self.lines
                .push(format!("latency {}", now.duration_since(input).as_micros()));
        }
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
        self.remaining == 0 && self.deadline.is_some_and(|d| now >= d)
    }
}

impl Drop for Latency {
    fn drop(&mut self) {
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
            inputs: 0,
        };
        log.frame(now, 0);
        log.input(now);
        log.output(0);
        log.frame(now, 0);
        assert_eq!(log.pending.len(), 1);
        log.output(1);
        log.input(now);
        log.frame(now + Duration::from_micros(10), 1);
        assert_eq!(log.lines.last().unwrap(), "latency 10");
        assert_eq!(log.pending.len(), 1);
        assert!(log.echoed.is_empty());
        assert!(log.probe(now).is_none());
        assert!(!log.finished(now));
    }
}
