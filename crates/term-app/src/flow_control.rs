#[derive(Default)]
pub struct FlowControl {
    pub paused: bool,
}

impl FlowControl {
    pub fn flags(&mut self, ixon: bool) {
        if !ixon {
            self.paused = false;
        }
    }

    pub fn input(&mut self, bytes: &[u8], ixon: bool, ixany: bool) {
        self.flags(ixon);
        for &byte in bytes {
            if byte == 0x11 || (ixany && byte != 0x13) {
                self.paused = false;
            } else if ixon && byte == 0x13 {
                self.paused = true;
            }
        }
    }

    pub fn output(&mut self) {
        self.paused = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_requires_ixon() {
        for ixon in [false, true] {
            let mut flow = FlowControl::default();
            flow.input(b"\x13", ixon, false);
            assert_eq!(flow.paused, ixon);
        }
    }

    #[test]
    fn resume_conditions() {
        let mut flow = FlowControl::default();
        flow.input(b"\x13hello", true, false);
        assert!(flow.paused);
        flow.input(b"", true, false);
        assert!(flow.paused);
        flow.input(b"\x11", true, false);
        assert!(!flow.paused);
        flow.input(b"\x13", true, false);
        flow.output();
        assert!(!flow.paused);
        flow.input(b"\x13", true, false);
        flow.flags(false);
        assert!(!flow.paused);
        flow.input(b"\x13x", true, true);
        assert!(!flow.paused);
        flow.input(b"x\x13", true, true);
        assert!(flow.paused);
    }
}
