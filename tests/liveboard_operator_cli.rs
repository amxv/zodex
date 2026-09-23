use std::process::Command;

#[test]
fn local_watch_rejects_invalid_web_agent_id_before_runtime_lookup() {
    let output = Command::new(env!("CARGO_BIN_EXE_zodex"))
        .args(["local", "watch", "--agent", "K7M2"])
        .output()
        .expect("run zodex local watch --agent K7M2");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Agent ID must be exactly four lowercase ASCII letters/digits"));
}

#[test]
fn local_watch_exposes_url_and_copyurl_subcommands() {
    let output = Command::new(env!("CARGO_BIN_EXE_zodex"))
        .args(["local", "watch", "--help"])
        .output()
        .expect("run zodex local watch --help");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("url"));
    assert!(stdout.contains("copyurl"));
}
