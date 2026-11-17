//! Software flow-control state for stop/start input and bounded output parsing.

#[derive(Clone, Copy, Default)]
pub struct FlowSettings {
    pub ixon: bool,
    pub ixany: bool,
    pub isig: bool,
    pub signals: [Option<u8>; 3],
    pub stop: Option<u8>,
    pub start: Option<u8>,
}

#[derive(Default)]
pub struct FlowControl {
    pub paused: bool,
    pause_sequence: u64,
    output_budget: u64,
    start: Option<u8>,
}

impl FlowControl {
    pub fn flags(&mut self, settings: FlowSettings) {
        self.start = settings.start;
        if !settings.ixon {
            self.paused = false;
        }
    }

    pub fn input(&mut self, bytes: &[u8], settings: FlowSettings, sequence: u64) -> bool {
        let was_paused = self.paused;
        self.flags(settings);
        for &byte in bytes {
            if Some(byte) == settings.start {
                self.paused = false;
            } else if settings.ixon && Some(byte) == settings.stop {
                self.paused = true;
                self.pause_sequence = sequence;
                self.output_budget = 0;
            } else if settings.ixany || (settings.isig && settings.signals.contains(&Some(byte))) {
                self.paused = false;
            }
        }
        self.paused != was_paused
    }

    pub fn banner(&self) -> Option<String> {
        self.paused.then(|| match self.start {
            Some(0..=31) => format!(
                "Output paused - press Ctrl+{}",
                (self.start.unwrap() + 64) as char
            ),
            Some(127) => "Output paused - press Backspace".into(),
            Some(key) if key.is_ascii_graphic() || key == b' ' => {
                format!("Output paused - press '{}'", key as char)
            }
            Some(key) => format!("Output paused - resume key 0x{key:02X}"),
            None => "Output paused - resume key disabled".into(),
        })
    }

    pub fn protect_output_until(&mut self, bytes: u64) {
        self.output_budget = bytes;
    }

    pub fn output(&mut self, sequence: u64, bytes_read: u64) {
        if sequence > self.pause_sequence && bytes_read > self.output_budget {
            self.paused = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(ixon: bool, ixany: bool) -> FlowSettings {
        FlowSettings {
            ixon,
            ixany,
            stop: Some(0x13),
            start: Some(0x11),
            ..Default::default()
        }
    }

    #[test]
    fn stop_requires_ixon() {
        for ixon in [false, true] {
            let mut flow = FlowControl::default();
            flow.input(b"\x13", settings(ixon, false), 0);
            assert_eq!(flow.paused, ixon);
        }
    }

    #[test]
    fn resume_conditions() {
        let mut flow = FlowControl::default();
        flow.input(b"\x13hello", settings(true, false), 0);
        assert!(flow.paused);
        flow.input(b"", settings(true, false), 0);
        assert!(flow.paused);
        flow.input(b"\x11", settings(true, false), 0);
        assert!(!flow.paused);
        flow.input(b"\x13", settings(true, false), 0);
        flow.output(1, 1);
        assert!(!flow.paused);
        flow.input(b"\x13", settings(true, false), 0);
        flow.flags(settings(false, false));
        assert!(!flow.paused);
        flow.input(b"\x13x", settings(true, true), 0);
        assert!(!flow.paused);
        flow.input(b"x\x13", settings(true, true), 0);
        assert!(flow.paused);
    }

    #[test]
    fn queued_output_before_stop_does_not_resume() {
        let mut flow = FlowControl::default();
        flow.input(b"\x13", settings(true, false), 7);
        flow.output(6, 100);
        flow.output(7, 100);
        assert!(flow.paused);
        flow.output(8, 101);
        assert!(!flow.paused);
    }

    #[test]
    fn input_reports_pause_state_changes() {
        let mut flow = FlowControl::default();
        assert!(flow.input(b"\x13", settings(true, false), 0));
        assert!(!flow.input(b"", settings(true, false), 0));
        assert!(!flow.input(b"x", settings(true, false), 0));
        assert!(flow.input(b"\x1b[M", settings(true, true), 0));
        assert!(!flow.input(b"\x11", settings(true, false), 0));
    }

    #[test]
    fn signal_keys_resume_only_with_isig() {
        for isig in [false, true] {
            for signal in [0x03, 0x1c, 0x1a] {
                let mut flow = FlowControl::default();
                let settings = FlowSettings {
                    isig,
                    signals: [Some(0x03), Some(0x1c), Some(0x1a)],
                    ..settings(true, false)
                };
                flow.input(b"\x13", settings, 0);
                flow.protect_output_until(u64::MAX);
                assert_eq!(flow.input(&[signal], settings, 0), isig);
                assert_eq!(flow.paused, !isig);
            }
        }
    }

    #[test]
    fn flow_keys_take_precedence_over_signals() {
        let mut flow = FlowControl::default();
        let mut settings = FlowSettings {
            isig: true,
            signals: [Some(0x03), None, None],
            stop: Some(0x03),
            ..settings(true, false)
        };
        assert!(flow.input(b"\x03", settings, 7));
        assert!(flow.paused);
        assert!(!flow.input(b"\x03", settings, 7));
        assert!(flow.paused);
        settings.start = Some(0x03);
        settings.stop = Some(0x13);
        assert!(flow.input(b"\x03", settings, 7));
        assert!(!flow.paused);
        settings.stop = settings.start;
        assert!(!flow.input(b"\x03", settings, 7));
        assert!(!flow.paused);
    }

    #[test]
    fn custom_and_disabled_signal_keys() {
        let mut flow = FlowControl::default();
        let settings = FlowSettings {
            isig: true,
            signals: [Some(0x18), None, None],
            ..settings(true, false)
        };
        flow.input(b"\x13\x03\x1c\x1a\0", settings, 0);
        assert!(flow.paused);
        assert!(flow.input(b"\x18", settings, 0));
        assert!(!flow.paused);
    }

    #[test]
    fn buffered_and_in_flight_output_does_not_resume() {
        let mut flow = FlowControl::default();
        flow.input(b"\x13", settings(true, false), 7);
        flow.protect_output_until(100 + 4096 + 65536);
        flow.output(8, 100);
        flow.output(9, 100 + 4096);
        flow.output(10, 100 + 4096 + 65536);
        assert!(flow.paused);
        flow.output(11, 100 + 4096 + 65536 + 1);
        assert!(!flow.paused);
    }

    #[test]
    fn stop_write_pending_and_explicit_resume() {
        let mut flow = FlowControl::default();
        flow.input(b"\x13", settings(true, false), 7);
        flow.protect_output_until(u64::MAX);
        flow.output(8, 1_000_000);
        assert!(flow.paused);
        flow.input(b"\x11", settings(true, false), 8);
        assert!(!flow.paused);
    }

    #[test]
    fn custom_and_disabled_keys() {
        let mut flow = FlowControl::default();
        let mut settings = FlowSettings {
            stop: Some(0x18),
            start: Some(0x19),
            ..settings(true, false)
        };
        flow.input(b"\x13", settings, 0);
        assert!(!flow.paused);
        flow.input(b"\x18", settings, 0);
        assert_eq!(
            flow.banner().as_deref(),
            Some("Output paused - press Ctrl+Y")
        );
        flow.input(b"\x11", settings, 0);
        assert!(flow.paused);
        flow.input(b"\x19", settings, 0);
        assert_eq!(flow.banner(), None);
        settings.stop = None;
        flow.input(b"\x18\0", settings, 0);
        assert!(!flow.paused);
        settings.stop = Some(0x18);
        settings.start = None;
        flow.input(b"\x18\x19\0", settings, 0);
        assert_eq!(
            flow.banner().as_deref(),
            Some("Output paused - resume key disabled")
        );
    }
}
