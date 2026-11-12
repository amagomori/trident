use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn terminate(_: libc::c_int) {
    TERMINATED.store(true, Ordering::Relaxed);
}

pub struct Signals {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

impl Signals {
    pub fn install() -> io::Result<Self> {
        TERMINATED.store(false, Ordering::Relaxed);
        let mut signals = Self { previous: vec![] };
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            unsafe {
                let mut action: libc::sigaction = std::mem::zeroed();
                let mut previous = std::mem::zeroed();
                action.sa_sigaction = terminate as *const () as usize;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, &mut previous) != 0 {
                    return Err(io::Error::last_os_error());
                }
                signals.previous.push((signal, previous));
            }
        }
        Ok(signals)
    }

    pub fn terminated(&self) -> bool {
        TERMINATED.load(Ordering::Relaxed)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for (signal, previous) in &self.previous {
            unsafe {
                libc::sigaction(*signal, previous, std::ptr::null_mut());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termination_signals_set_state_and_restore_handlers() {
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            let signals = Signals::install().unwrap();
            assert!(!signals.terminated());
            let previous = signals
                .previous
                .iter()
                .find(|(s, _)| *s == signal)
                .unwrap()
                .1;
            unsafe {
                assert_eq!(libc::raise(signal), 0);
            }
            assert!(signals.terminated());
            drop(signals);
            unsafe {
                let mut restored: libc::sigaction = std::mem::zeroed();
                assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut restored), 0);
                assert_eq!(restored.sa_sigaction, previous.sa_sigaction);
            }
        }
    }
}
