//! The `xterm-terminal` terminfo entry: TERM selection and installation.

use std::path::{Path, PathBuf};

pub const NAME: &str = "xterm-terminal";
pub const FALLBACK: &str = "xterm-256color";
pub const SOURCE: &str = include_str!("../terminfo/xterm-terminal.terminfo");

/// The TERM value for child processes: our entry when installed, else xterm-256color.
pub fn term() -> &'static str {
    if installed_in(&search_dirs(
        std::env::var_os("TERMINFO"),
        std::env::var_os("TERMINFO_DIRS"),
        std::env::var_os("HOME"),
    ))
    .is_some()
    {
        NAME
    } else {
        FALLBACK
    }
}

/// Directories ncurses searches, in order: $TERMINFO, ~/.terminfo,
/// $TERMINFO_DIRS (an empty entry means the system directories), then the
/// system directories.
fn search_dirs(
    terminfo: Option<std::ffi::OsString>,
    terminfo_dirs: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Vec<PathBuf> {
    const SYSTEM: &[&str] = &[
        "/etc/terminfo",
        "/lib/terminfo",
        "/usr/share/terminfo",
        "/usr/lib/terminfo",
        "/usr/local/share/terminfo",
        "/opt/homebrew/share/terminfo",
        "/opt/local/share/terminfo",
    ];
    let mut dirs: Vec<PathBuf> = Vec::new();
    dirs.extend(terminfo.filter(|t| !t.is_empty()).map(PathBuf::from));
    dirs.extend(home.map(|h| Path::new(&h).join(".terminfo")));
    if let Some(list) = terminfo_dirs {
        for dir in std::env::split_paths(&list) {
            if dir.as_os_str().is_empty() {
                dirs.extend(SYSTEM.iter().map(PathBuf::from));
            } else {
                dirs.push(dir);
            }
        }
    }
    dirs.extend(SYSTEM.iter().map(PathBuf::from));
    dirs
}

/// The directory holding the compiled entry, under either the letter
/// (Linux) or hex (macOS) subdirectory layout.
fn installed_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .find(|dir| {
            ["x", "78"]
                .iter()
                .any(|sub| dir.join(sub).join(NAME).is_file())
        })
        .cloned()
}

/// Compiles the bundled entry with `tic -x`, which writes to $TERMINFO,
/// the system directory when writable, or ~/.terminfo.
pub fn install() -> i32 {
    let dir = std::env::temp_dir().join(format!("{NAME}-{}.terminfo", std::process::id()));
    if let Err(e) = std::fs::write(&dir, SOURCE) {
        eprintln!("terminal: cannot write {}: {e}", dir.display());
        return 1;
    }
    let status = std::process::Command::new("tic")
        .arg("-x")
        .arg(&dir)
        .status();
    let _ = std::fs::remove_file(&dir);
    match status {
        Ok(s) if s.success() => {
            println!("terminal: installed the {NAME} terminfo entry; new shells get TERM={NAME}");
            0
        }
        Ok(s) => {
            eprintln!("terminal: tic failed ({s})");
            1
        }
        Err(e) => {
            eprintln!("terminal: cannot run tic: {e} (install ncurses)");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_order_matches_ncurses() {
        let dirs = search_dirs(
            Some("/ti".into()),
            Some("/a::/b".into()),
            Some("/home/u".into()),
        );
        assert_eq!(dirs[0], Path::new("/ti"));
        assert_eq!(dirs[1], Path::new("/home/u/.terminfo"));
        assert_eq!(dirs[2], Path::new("/a"));
        assert_eq!(dirs[3], Path::new("/etc/terminfo"));
        assert!(dirs.contains(&PathBuf::from("/b")));
        assert!(dirs.ends_with(&[PathBuf::from("/opt/local/share/terminfo")]));
        let dirs = search_dirs(Some("".into()), None, None);
        assert_eq!(dirs[0], Path::new("/etc/terminfo"));
    }

    #[test]
    fn falls_back_when_not_installed() {
        let root = std::env::temp_dir().join(format!("terminfo-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(installed_in(std::slice::from_ref(&root)), None);
        for sub in ["x", "78"] {
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join(sub)).unwrap();
            assert_eq!(installed_in(std::slice::from_ref(&root)), None);
            std::fs::write(root.join(sub).join(NAME), b"").unwrap();
            assert_eq!(
                installed_in(std::slice::from_ref(&root)),
                Some(root.clone())
            );
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// Compiles the entry with the system tic and checks every capability
    /// it adds or removes; skipped where ncurses is not installed.
    #[test]
    fn entry_compiles_with_tic() {
        let Ok(out) = std::process::Command::new("tic").arg("-V").output() else {
            eprintln!("tic not found; skipping");
            return;
        };
        assert!(out.status.success());
        let dir = std::env::temp_dir().join(format!("terminfo-tic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("entry.terminfo");
        std::fs::write(&src, SOURCE).unwrap();
        let tic = std::process::Command::new("tic")
            .args(["-x", "-o"])
            .arg(&dir)
            .arg(&src)
            .output()
            .unwrap();
        assert!(
            tic.status.success(),
            "{}",
            String::from_utf8_lossy(&tic.stderr)
        );
        assert!(installed_in(std::slice::from_ref(&dir)).is_some());
        let infocmp = std::process::Command::new("infocmp")
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
    }
}
