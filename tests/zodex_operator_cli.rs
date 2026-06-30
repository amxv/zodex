use std::process::Command;

#[test]
fn zodex_github_help_exposes_mode_commands() {
    let output = Command::new(env!("CARGO_BIN_EXE_zodex"))
        .args(["github", "--help"])
        .output()
        .expect("run zodex github --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("request-push"));
    assert!(stdout.contains("grant-push"));
    assert!(stdout.contains("revoke-push"));
    assert!(stdout.contains("list-grants"));
    assert!(stdout.contains("mode"));
}

#[test]
fn zodex_github_mode_help_exposes_yolo_default_and_status() {
    let output = Command::new(env!("CARGO_BIN_EXE_zodex"))
        .args(["github", "mode", "--help"])
        .output()
        .expect("run zodex github mode --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("yolo"));
    assert!(stdout.contains("default"));
    assert!(stdout.contains("status"));
}

#[test]
fn zodex_github_mode_yolo_help_exposes_expected_flags() {
    let output = Command::new(env!("CARGO_BIN_EXE_zodex"))
        .args(["github", "mode", "yolo", "--help"])
        .output()
        .expect("run zodex github mode yolo --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("--sprite"));
    assert!(stdout.contains("--repo"));
    assert!(stdout.contains("--ttl"));
    assert!(stdout.contains("--no-ttl"));
    assert!(stdout.contains("[default: 2h]"));
}
