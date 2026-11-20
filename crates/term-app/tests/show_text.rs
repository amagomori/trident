use std::{fs, process::Command};

#[test]
fn multiple_text_files_keep_headers_and_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("empty"), []).unwrap();
    fs::write(dir.path().join("plain"), "界\t🙂\r\n\x1b[31m").unwrap();
    fs::write(dir.path().join("decoded"), b"caf\xe9\x81\r").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_trident"))
        .current_dir(dir.path())
        .args(["show", "empty", "plain", "decoded"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output.stderr);
    assert_eq!(
        output.stdout,
        "empty\nplain\n界\t🙂\n^[[31m\ndecoded\ncafé�\n".as_bytes()
    );
}
