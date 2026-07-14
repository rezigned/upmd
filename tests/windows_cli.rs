//! Windows-cross CLI runner integration tests.
//!
//! These tests exercise execution plan production for `cmd` and `powershell`
//! without spawning a PTY or requiring the language binaries to be installed.
//! They run on every platform (Windows GitHub runner included) because
//! `plan()` is pure and does not validate the binary on PATH.

use upmd_runner::{find, CodeInput, ShellQuoteStyle, StateCaptureContext};

const NO_CAPTURE: StateCaptureContext = StateCaptureContext {
    enabled: false,
    fifos: None,
    code_id: 1,
};

fn make_input<'a>(content: &'a str, lang: &'a upmd_runner::Language) -> CodeInput<'a> {
    CodeInput {
        id: 1,
        content,
        language: lang,
        state_capture: &NO_CAPTURE,
    }
}

/// Asserts the plan's first command is a single argument equal to `expected`.
fn assert_inline_command(plan: &upmd_runner::ExecutionPlan<'_>, expected: &str) {
    assert!(
        !plan.commands.is_empty(),
        "plan must produce at least one command"
    );
    let cmd = &plan.commands[0];
    assert_eq!(cmd.len(), 1, "inline command must be a single argument");
    assert_eq!(cmd[0], expected, "inline command content must match");
}

#[test]
fn cmd_inline_plan_emits_user_code() {
    let runner = find("cmd").expect("cmd runner must be registered");
    let lang = runner.language().clone();
    let input = make_input("echo hello", &lang);
    let plan = runner.plan(&input).expect("plan must succeed");

    assert!(!plan.requires_file, "inline cmd must not require a file");
    assert_eq!(
        plan.quote_style,
        ShellQuoteStyle::Cmd,
        "cmd must use Cmd quoting"
    );
    assert_inline_command(&plan, "echo hello");
}

#[test]
fn cmd_multiline_plan_creates_bat_file() {
    let runner = find("cmd").expect("cmd runner must be registered");
    let lang = runner.language().clone();
    let input = make_input("echo hello\necho world", &lang);
    let plan = runner.plan(&input).expect("plan must succeed");

    assert!(plan.requires_file, "multiline cmd must require a file");
    assert_eq!(
        plan.files.len(),
        1,
        "multiline cmd must create exactly one file"
    );

    let (path, content) = &plan.files[0];
    let path_str = path.to_string_lossy();
    assert!(
        path_str.ends_with(".bat"),
        "cmd file must have .bat extension, got: {path_str}"
    );
    assert!(
        content.contains("echo hello") && content.contains("echo world"),
        "cmd file must preserve both lines, got: {content}"
    );
}

#[test]
fn powershell_inline_plan_emits_user_code() {
    let runner = find("powershell").expect("powershell runner must be registered");
    let lang = runner.language().clone();
    let input = make_input("Write-Output 'hello'", &lang);
    let plan = runner.plan(&input).expect("plan must succeed");

    assert!(
        !plan.requires_file,
        "inline powershell must not require a file"
    );
    assert_eq!(
        plan.quote_style,
        ShellQuoteStyle::PowerShell,
        "powershell must use PowerShell quoting"
    );
    assert_inline_command(&plan, "Write-Output 'hello'");
}

#[test]
fn powershell_multiline_plan_creates_ps1_file() {
    let runner = find("powershell").expect("powershell runner must be registered");
    let lang = runner.language().clone();
    let input = make_input("Write-Output 'a'\nWrite-Output 'b'", &lang);
    let plan = runner.plan(&input).expect("plan must succeed");

    assert!(
        plan.requires_file,
        "multiline powershell must require a file"
    );
    assert_eq!(
        plan.files.len(),
        1,
        "multiline powershell must create exactly one file"
    );

    let (path, content) = &plan.files[0];
    let path_str = path.to_string_lossy();
    assert!(
        path_str.ends_with(".ps1"),
        "powershell file must have .ps1 extension, got: {path_str}"
    );
    assert!(
        content.contains("Write-Output 'a'") && content.contains("Write-Output 'b'"),
        "powershell file must preserve both lines, got: {content}"
    );
}

#[test]
fn cmd_and_powershell_are_in_registry() {
    let cmd = find("cmd");
    let ps = find("powershell");
    assert!(cmd.is_ok(), "cmd runner must be found in registry");
    assert!(ps.is_ok(), "powershell runner must be found in registry");

    assert_eq!(cmd.unwrap().language().name, "Cmd");
    assert_eq!(ps.unwrap().language().name, "PowerShell");
}

#[test]
fn cmd_alias_batch_resolves() {
    let runner = find("batch").expect("batch alias must resolve to Cmd runner");
    assert_eq!(runner.language().name, "Cmd");
}

#[test]
fn powershell_alias_pwsh_resolves() {
    let runner = find("pwsh").expect("pwsh alias must resolve to PowerShell runner");
    assert_eq!(runner.language().name, "PowerShell");
}

#[test]
fn cmd_alias_bat_resolves() {
    let runner = find("bat").expect("bat alias must resolve to Cmd runner");
    assert_eq!(runner.language().name, "Cmd");
}

#[test]
fn powershell_alias_ps1_resolves() {
    let runner = find("ps1").expect("ps1 alias must resolve to PowerShell runner");
    assert_eq!(runner.language().name, "PowerShell");
}
