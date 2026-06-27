use std::process::Command;

#[test]
fn zodex_agent_help_only_exposes_restricted_surface() {
    let output = Command::new(env!("CARGO_BIN_EXE_zodex-agent"))
        .arg("--help")
        .output()
        .expect("run zodex-agent --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("Restricted Zodex agent CLI"));
    assert!(stdout.contains("git-credential-helper"));
    assert!(stdout.contains("show-url"));
    assert!(stdout.contains("github"));
    assert!(!stdout.contains("install"));
    assert!(!stdout.contains("sprite"));
    assert!(!stdout.contains("proxy"));
    assert!(!stdout.contains("set-key"));
}

#[test]
fn zodex_agent_github_help_exposes_only_local_auth_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_zodex-agent"))
        .args(["github", "--help"])
        .output()
        .expect("run zodex-agent github --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("request-push"));
    assert!(stdout.contains("revoke-push"));
    assert!(stdout.contains("list-grants"));
    assert!(stdout.contains("create-pr"));
    assert!(!stdout.contains("grant-push"));
}

#[test]
fn zodex_agent_github_create_pr_help_exposes_expected_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_zodex-agent"))
        .args(["github", "create-pr", "--help"])
        .output()
        .expect("run zodex-agent github create-pr --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("--repo"));
    assert!(stdout.contains("--head"));
    assert!(stdout.contains("--title"));
    assert!(stdout.contains("--base"));
    assert!(stdout.contains("--body"));
    assert!(stdout.contains("--draft"));
}
