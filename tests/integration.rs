// Integration tests for end-to-end behavior.
//
// Add smoke tests here: CLI execution, config merging, runner plan
// production, etc. Parser unit tests belong in the upmd-parser crate.

#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
#[test]
fn cli_block_yes_prints_pty_output_before_exit() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    std::fs::write(tmp.path().join("listed-file.txt"), "").expect("create listed file");

    let markdown = tmp.path().join("case.md");
    std::fs::write(&markdown, "```shell\nls\n```\n").expect("write markdown");

    let output = Command::new(env!("CARGO_BIN_EXE_upmd"))
        .arg(&markdown)
        .args(["--cli", "-y", "-b", "1", "--working-dir"])
        .arg(tmp.path())
        .output()
        .expect("run upmd");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "upmd failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    assert!(
        stdout.contains("listed-file.txt"),
        "CLI output should include PTY stdout\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
}
