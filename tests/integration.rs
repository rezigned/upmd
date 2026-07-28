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

#[cfg(unix)]
#[test]
fn missing_block_fails_before_either_frontend_starts() {
    let tmp = tempfile::tempdir().expect("create temp dir");
    let markdown = tmp.path().join("case.md");
    std::fs::write(
        &markdown,
        "```shell [name:first]\nprintf 'must-not-run\\n'\n```\n",
    )
    .expect("write markdown");

    for cli in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_upmd"));
        command.arg(&markdown).args(["-y", "-b", "missing"]);
        if cli {
            command.arg("--cli");
        }
        let output = command.output().expect("run upmd");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(
            !output.status.success(),
            "missing block unexpectedly succeeded (cli={cli})\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
        assert!(
            !stdout.contains("must-not-run"),
            "first block ran for unresolved selection (cli={cli})\nstdout:\n{stdout}"
        );
        assert!(
            stderr.contains("code block \"missing\" not found in document"),
            "missing diagnostic (cli={cli})\nstderr:\n{stderr}"
        );
    }
}
