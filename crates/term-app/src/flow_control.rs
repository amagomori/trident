#[derive(Default)]
pub struct FlowControl {
    pub paused: bool,
    pause_sequence: u64,
}

impl FlowControl {
    pub fn flags(&mut self, ixon: bool) {
        if !ixon {
            self.paused = false;
        }
    }

    pub fn input(&mut self, bytes: &[u8], ixon: bool, ixany: bool, sequence: u64) {
        self.flags(ixon);
        for &byte in bytes {
            if byte == 0x11 || (ixany && byte != 0x13) {
                self.paused = false;
            } else if ixon && byte == 0x13 {
                self.paused = true;
                self.pause_sequence = sequence;
            }
        }
    }

    pub fn output(&mut self, sequence: u64) {
        if sequence > self.pause_sequence {
            self.paused = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_requires_ixon() {
        for ixon in [false, true] {
            let mut flow = FlowControl::default();
            flow.input(b"\x13", ixon, false, 0);
            assert_eq!(flow.paused, ixon);
        }
    }

    #[test]
    fn resume_conditions() {
        let mut flow = FlowControl::default();
        flow.input(b"\x13hello", true, false, 0);
        assert!(flow.paused);
        flow.input(b"", true, false, 0);
        assert!(flow.paused);
        flow.input(b"\x11", true, false, 0);
        assert!(!flow.paused);
        flow.input(b"\x13", true, false, 0);
        flow.output(1);
        assert!(!flow.paused);
        flow.input(b"\x13", true, false, 0);
        flow.flags(false);
        assert!(!flow.paused);
        flow.input(b"\x13x", true, true, 0);
        assert!(!flow.paused);
        flow.input(b"x\x13", true, true, 0);
        assert!(flow.paused);
    }

    #[test]
    fn queued_output_before_stop_does_not_resume() {
        let mut flow = FlowControl::default();
        flow.input(b"\x13", true, false, 7);
        flow.output(6);
        flow.output(7);
        assert!(flow.paused);
        flow.output(8);
        assert!(!flow.paused);
    }
}
