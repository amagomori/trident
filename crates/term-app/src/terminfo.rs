//! The `xterm-trident` terminfo entry: TERM selection and installation.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const NAME: &str = "xterm-trident";
/// The entry's pre-rename name, kept as an alias.
const OLD_NAME: &str = "xterm-terminal";
pub const FALLBACK: &str = "xterm-256color";
pub const SOURCE: &str = include_str!("../terminfo/xterm-trident.terminfo");

/// The TERM value for a child with `env` applied: `wanted` (the config's
/// `term`) when the system ncurses resolves it in that environment, else
/// xterm-256color. The default skips the lookup.
pub fn term<'a>(wanted: &'a str, env: &BTreeMap<String, String>) -> &'a str {
    if wanted != FALLBACK && resolves(&tool("infocmp"), wanted, env) {
        wanted
    } else {
        FALLBACK
    }
}

fn resolves(infocmp: &Path, name: &str, env: &BTreeMap<String, String>) -> bool {
    Command::new(infocmp)
        .arg(name)
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// An ncurses program from /usr/bin when it is there, else from PATH. On
/// macOS, Homebrew's ncurses 6.6 writes entries that Apple's ncurses 6.0
/// (used by /usr/bin tools) cannot read; entries the system tic writes are
/// readable by both.
fn tool(name: &str) -> PathBuf {
    let system = Path::new("/usr/bin").join(name);
    if system.is_file() {
        system
    } else {
        PathBuf::from(name)
    }
}

/// Compiles the bundled entry with `tic -x`, which writes to $TERMINFO,
/// the system directory when writable, or ~/.terminfo.
pub fn install() -> i32 {
    match install_with(&BTreeMap::new()) {
        Ok(warnings) => {
            eprint!("{warnings}");
            println!("{}", installed_message());
            0
        }
        Err(e) => {
            eprintln!("trident: {e}");
            1
        }
    }
}

/// Where the ssh and macOS install notes for the entry live.
const USAGE_DOC: &str = "docs/usage.md#terminfo";

fn installed_message() -> String {
    format!(
        "trident: installed the {NAME} terminfo entry; set term = \"{NAME}\" in the \
         config to use it (ssh hosts need the entry too, see {USAGE_DOC})"
    )
}

/// Runs tic with `env` and returns its warnings. tic leaves an alias file
/// older than the run in place ("alias xterm-terminal multiply defined"),
/// so an entry installed under the pre-rename name would keep answering to
/// it; that file is removed and the entry compiled again.
fn install_with(env: &BTreeMap<String, String>) -> Result<String, String> {
    let (_, log) = tic(SOURCE, &["-s".as_ref()], env)?;
    let (dir, warnings) = split_summary(&log);
    let Some(stale) = dir.and_then(|dir| stale_alias(&dir)) else {
        return Ok(warnings);
    };
    std::fs::remove_file(&stale).map_err(|e| format!("cannot replace {}: {e}", stale.display()))?;
    Ok(tic(SOURCE, &[], env)?.1)
}

/// Splits tic's stderr into the directory from its `-s` summary
/// ("1 entries written to DIR") and the other lines.
fn split_summary(log: &str) -> (Option<PathBuf>, String) {
    let mut dir = None;
    let mut rest = String::new();
    for line in log.lines() {
        match line.split_once(" entries written to ") {
            Some((_, d)) => dir = Some(PathBuf::from(d.trim())),
            None => {
                rest.push_str(line);
                rest.push('\n');
            }
        }
    }
    (dir, rest)
}

/// The `OLD_NAME` file next to the freshly written entry in `dir` (leaf
/// directories are `x` or, on macOS, the hex `78`), when it holds a
/// different entry.
fn stale_alias(dir: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    ["x", "78"].iter().find_map(|leaf| {
        let entry = std::fs::metadata(dir.join(leaf).join(NAME)).ok()?;
        let alias = dir.join(leaf).join(OLD_NAME);
        let old = std::fs::metadata(&alias).ok()?;
        ((old.dev(), old.ino()) != (entry.dev(), entry.ino())).then_some(alias)
    })
}

/// Feeds `source` to tic on stdin, so no temporary file is involved, and
/// returns its stdout and stderr.
fn tic(
    source: &str,
    args: &[&std::ffi::OsStr],
    env: &BTreeMap<String, String>,
) -> Result<(String, String), String> {
    let mut child = Command::new(tool("tic"))
        .arg("-x")
        .args(args)
        .arg("/dev/stdin")
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run tic: {e} (install ncurses)"))?;
    let written = child.stdin.take().unwrap().write_all(source.as_bytes());
    let output = child
        .wait_with_output()
        .map_err(|e| format!("tic failed: {e}"))?;
    written.map_err(|e| format!("cannot write to tic: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if output.status.success() {
        Ok((stdout, stderr))
    } else {
        Err(format!("tic failed ({}): {}", output.status, stderr.trim()))
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
        let found = Command::new(tool("tic")).arg("-V").output().is_ok();
        if !found {
            eprintln!("tic not found; skipping");
        }
        found
    }

    /// The install message points at the doc that has the ssh notes.
    #[test]
    fn installed_message_points_at_usage_doc() {
        let message = installed_message();
        assert!(message.contains("see docs/usage.md#terminfo"), "{message}");
        assert!(!message.contains("README"), "{message}");
        let (path, anchor) = USAGE_DOC.split_once('#').unwrap();
        let doc = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(path),
        )
        .unwrap();
        let section = doc
            .split("\n## ")
            .find(|s| s.lines().next().unwrap().to_lowercase() == anchor)
            .unwrap_or_else(|| panic!("no #{anchor} section in {path}"));
        assert!(section.contains("ssh"), "{section}");
        assert!(section.contains(&format!("infocmp -x {NAME}")), "{section}");
    }

    #[test]
    fn falls_back_without_infocmp() {
        let env = BTreeMap::new();
        assert!(!resolves(Path::new("/nonexistent/infocmp"), NAME, &env));
        assert_eq!(term(FALLBACK, &env), FALLBACK);
        assert_eq!(term("no-such-terminal-entry", &env), FALLBACK);
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
        if term(NAME, &only(&empty)) == FALLBACK {
            tic(SOURCE, &["-o".as_ref(), dir.as_os_str()], &BTreeMap::new()).unwrap();
            assert_eq!(term(NAME, &only(&dir)), NAME);
            assert_eq!(term(OLD_NAME, &only(&dir)), OLD_NAME);
            assert_eq!(term(NAME, &only(&empty)), FALLBACK);
            assert_eq!(term(FALLBACK, &only(&dir)), FALLBACK);
        } else {
            eprintln!("{NAME} is installed system-wide; checking the bundled source only");
            tic(SOURCE, &["-o".as_ref(), dir.as_os_str()], &BTreeMap::new()).unwrap();
        }
        let infocmp = Command::new(tool("infocmp"))
            .args(["-x", "-1", "-q", NAME])
            .env("TERMINFO", &dir)
            .output()
            .unwrap();
        assert!(infocmp.status.success());
        // ncurses 6.0 (Apple's) escapes ':' in strings; later versions don't.
        let caps = String::from_utf8(infocmp.stdout)
            .unwrap()
            .replace("\\:", ":");
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
            "civis=\\E[?25l",
            "cnorm=\\E[?25h",
        ] {
            assert!(caps.contains(&cap), "missing {cap}");
        }
        // Inherited from the system's xterm-256color, which varies by version.
        for cap in ["smcup=\\E[?1049h", "kmous=\\E[", "colors#"] {
            assert!(caps.iter().any(|c| c.starts_with(cap)), "missing {cap}");
        }
        for cap in [
            "blink=", "flash=", "initc=", "Ms=", "Cs=", "smglr=", "mc5=", "cvvis=",
        ] {
            assert!(!caps.iter().any(|c| c.starts_with(cap)), "unexpected {cap}");
        }
        assert_cnorm_keeps_blink(&dir);
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&empty).unwrap();
    }

    /// Installing over an entry compiled before the rename, whose primary
    /// name was `xterm-terminal`, makes that name an alias of the new entry.
    #[test]
    fn install_replaces_pre_rename_entry() {
        if !has_ncurses() {
            return;
        }
        let dir = temp_dir("rename");
        let env = BTreeMap::from([
            ("TERMINFO".to_string(), dir.display().to_string()),
            ("TERMINFO_DIRS".to_string(), dir.display().to_string()),
        ]);
        let old = SOURCE.replace(&format!("{NAME}|{OLD_NAME}|"), &format!("{OLD_NAME}|"));
        assert_ne!(old, SOURCE);
        tic(&old, &[], &env).unwrap();
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for leaf in ["x", "78"] {
            if let Ok(file) = std::fs::File::options()
                .write(true)
                .open(dir.join(leaf).join(OLD_NAME))
            {
                file.set_modified(hour_ago).unwrap();
            }
        }
        install_with(&env).unwrap();
        let names = |name: &str| {
            let out = Command::new(tool("infocmp"))
                .args(["-x", "-1", name])
                .envs(&env)
                .output()
                .unwrap();
            assert!(out.status.success());
            let text = String::from_utf8(out.stdout).unwrap();
            text.lines()
                .find(|l| !l.starts_with('#'))
                .unwrap()
                .to_string()
        };
        assert!(names(OLD_NAME).starts_with(&format!("{NAME}|{OLD_NAME}|")));
        assert_eq!(names(OLD_NAME), names(NAME));
        // A second install over the current entry leaves it alone.
        install_with(&env).unwrap();
        assert!(names(OLD_NAME).starts_with(&format!("{NAME}|")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// What tmux sends on every cursor show: a blinking DECSCUSR style, then
    /// the entry's cnorm. The cursor must still blink (KAN-126).
    fn assert_cnorm_keeps_blink(dir: &Path) {
        let cnorm = Command::new(tool("tput"))
            .args(["-T", NAME, "cnorm"])
            .env("TERMINFO", dir)
            .output()
            .unwrap();
        assert!(cnorm.status.success());
        assert_eq!(cnorm.stdout, b"\x1b[?25h");
        let mut term = term_core::Terminal::new(10, 2);
        term.advance(b"\x1b[?25l\x1b[1 q");
        term.advance(&cnorm.stdout);
        assert!(term.cursor_blink);
        assert!(term.modes.show_cursor);
    }
}
