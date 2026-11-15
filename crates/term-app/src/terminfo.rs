//! The `xterm-terminal` terminfo entry: TERM selection and installation.

use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

pub const NAME: &str = "xterm-terminal";
pub const FALLBACK: &str = "xterm-256color";
pub const SOURCE: &str = include_str!("../terminfo/xterm-terminal.terminfo");

/// The TERM value for a child with `env` applied: our entry when the
/// system ncurses resolves it in that environment, else xterm-256color.
pub fn term(env: &BTreeMap<String, String>) -> &'static str {
    let found = Command::new("infocmp")
        .arg(NAME)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if found { NAME } else { FALLBACK }
}

/// Compiles the bundled entry with `tic -x`, which writes to $TERMINFO,
/// the system directory when writable, or ~/.terminfo.
pub fn install() -> i32 {
    match tic(&[]) {
        Ok(()) => {
            println!("terminal: installed the {NAME} terminfo entry; new shells get TERM={NAME}");
            0
        }
        Err(e) => {
            eprintln!("terminal: {e}");
            1
        }
    }
}

/// Feeds the source to tic on stdin, so no temporary file is involved.
fn tic(args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let mut child = Command::new("tic")
        .arg("-x")
        .args(args)
        .arg("/dev/stdin")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run tic: {e} (install ncurses)"))?;
    let written = child.stdin.take().unwrap().write_all(SOURCE.as_bytes());
    let status = child.wait().map_err(|e| format!("tic failed: {e}"))?;
    written.map_err(|e| format!("cannot write to tic: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tic failed ({status})"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("terminfo-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn has_ncurses() -> bool {
        let found = Command::new("tic").arg("-V").output().is_ok();
        if !found {
            eprintln!("tic not found; skipping");
        }
        found
    }

    #[test]
    fn falls_back_without_infocmp() {
        let env = BTreeMap::from([("PATH".to_string(), String::new())]);
        assert_eq!(term(&env), FALLBACK);
    }

    /// Compiles the entry with the system tic, checks that TERM follows
    /// the child's TERMINFO, and checks every capability the entry adds or
    /// removes; skipped where ncurses is not installed.
    #[test]
    fn entry_compiles_with_tic() {
        if !has_ncurses() {
            return;
        }
        let dir = temp_dir("tic");
        let empty = temp_dir("empty");
        let only = |d: &std::path::Path| {
            BTreeMap::from([
                ("TERMINFO".to_string(), d.display().to_string()),
                ("TERMINFO_DIRS".to_string(), d.display().to_string()),
                ("HOME".to_string(), empty.display().to_string()),
            ])
        };
        if term(&only(&empty)) == FALLBACK {
            tic(&["-o".as_ref(), dir.as_os_str()]).unwrap();
            assert_eq!(term(&only(&dir)), NAME);
            assert_eq!(term(&only(&empty)), FALLBACK);
        } else {
            eprintln!("{NAME} is installed system-wide; checking the bundled source only");
            tic(&["-o".as_ref(), dir.as_os_str()]).unwrap();
        }
        let infocmp = Command::new("infocmp")
            .args(["-x", "-1", "-q", NAME])
            .env("TERMINFO", &dir)
            .output()
            .unwrap();
        assert!(infocmp.status.success());
        let caps = String::from_utf8(infocmp.stdout).unwrap();
        let caps: Vec<&str> = caps
            .lines()
            .map(|l| l.trim().trim_end_matches(','))
            .collect();
        for cap in [
            "Tc",
            "hs",
            "setrgbf=\\E[38;2;%p1%d;%p2%d;%p3%dm",
            "setrgbb=\\E[48;2;%p1%d;%p2%d;%p3%dm",
            "Smulx=\\E[4:%p1%dm",
            "BE=\\E[?2004h",
            "BD=\\E[?2004l",
            "PS=\\E[200~",
            "PE=\\E[201~",
            "Sync=\\E[?2026%?%p1%{1}%-%tl%eh%;",
            "tsl=\\E]2;",
            "fsl=^G",
            "smcup=\\E[?1049h\\E[22;0;0t",
            "kmous=\\E[<",
        ] {
            assert!(caps.contains(&cap), "missing {cap}");
        }
        for cap in ["blink=", "flash=", "initc=", "Ms=", "Cs=", "smglr=", "mc5="] {
            assert!(!caps.iter().any(|c| c.starts_with(cap)), "unexpected {cap}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&empty).unwrap();
    }
}
