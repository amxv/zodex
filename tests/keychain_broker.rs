#![cfg(target_os = "macos")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn check_success(output: Output, label: &str) -> Output {
    assert!(
        output.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn broker_build_path() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target =
        root.join("apps/keychain-helper/target/aarch64-apple-darwin/release/zodex-keychain-v1");
    if !target.is_file() {
        check_success(
            Command::new("cargo")
                .args([
                    "build",
                    "--locked",
                    "--release",
                    "--manifest-path",
                    "apps/keychain-helper/Cargo.toml",
                    "--target",
                    "aarch64-apple-darwin",
                ])
                .current_dir(root)
                .output()
                .unwrap(),
            "build immutable Keychain helper",
        );
    }
    target
}

fn build_test_parent(dir: &Path, generation: u32) -> PathBuf {
    let src = dir.join("test-parent.c");
    fs::write(
        &src,
        format!(
            r#"#include <unistd.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <stdlib.h>
static const int test_generation = {generation};
int main(void) {{
    if (test_generation == 0) return 2;
    pid_t child = fork();
    if (child < 0) return 3;
    if (child == 0) {{
        execl("./zodex-keychain-v1", "zodex-keychain-v1", "check-caller", (char*)NULL);
        _exit(4);
    }}
    int status = 0;
    if (waitpid(child, &status, 0) < 0) return 5;
    return WIFEXITED(status) ? WEXITSTATUS(status) : 6;
}}
"#,
        ),
    )
    .unwrap();
    let binary = dir.join("zodex");
    check_success(
        Command::new("clang")
            .args(["-O0", "-o"])
            .arg(&binary)
            .arg(&src)
            .output()
            .unwrap(),
        "compile signed test operator",
    );
    check_success(
        Command::new("codesign")
            .args([
                "--force",
                "--sign",
                "-",
                "--identifier",
                "com.amxv.zodex.local",
            ])
            .arg(&binary)
            .output()
            .unwrap(),
        "ad-hoc sign test operator",
    );
    binary
}

#[test]
fn immutable_broker_denies_direct_access_but_accepts_updated_signed_operator() {
    let dir = tempfile::tempdir().unwrap();
    let broker = dir.path().join("zodex-keychain-v1");
    fs::copy(broker_build_path(), &broker).unwrap();
    check_success(
        Command::new("codesign")
            .args(["--verify", "--strict"])
            .arg(&broker)
            .output()
            .unwrap(),
        "verify packaged broker",
    );
    assert_eq!(
        String::from_utf8(
            check_success(
                Command::new(&broker).arg("protocol").output().unwrap(),
                "broker protocol"
            )
            .stdout
        )
        .unwrap(),
        "1\n"
    );

    // An ordinary CLI invocation from this test process has no credentials
    // and MUST be rejected before the helper even tries to open Keychain.
    let unauthorized = Command::new(&broker).arg("get").output().unwrap();
    assert!(!unauthorized.status.success());
    assert!(unauthorized.stdout.is_empty());
    assert!(String::from_utf8_lossy(&unauthorized.stderr).contains("parent"));

    let first = build_test_parent(dir.path(), 1);
    let first_contents = fs::read(&first).unwrap();
    let stable_broker_bytes = fs::read(&broker).unwrap();
    assert_eq!(
        check_success(
            Command::new(&first)
                .current_dir(dir.path())
                .output()
                .unwrap(),
            "first signed parent"
        )
        .stdout,
        b"ok\n"
    );

    let second = build_test_parent(dir.path(), 2);
    assert_ne!(
        fs::read(&second).unwrap(),
        first_contents,
        "test versions must differ"
    );
    assert_eq!(
        check_success(
            Command::new(&second)
                .current_dir(dir.path())
                .output()
                .unwrap(),
            "updated signed parent"
        )
        .stdout,
        b"ok\n"
    );
    assert_eq!(
        fs::read(&broker).unwrap(),
        stable_broker_bytes,
        "broker may never change across upgrades"
    );
}
