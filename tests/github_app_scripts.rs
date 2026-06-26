use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path(name: &str) -> PathBuf {
    repo_root().join("scripts").join(name)
}

#[test]
fn github_app_scripts_have_valid_bash_syntax() {
    let scripts = [
        "mint-gh-app-installation-token.sh",
        "protect-main-branch.sh",
    ];

    for script in scripts {
        let output = Command::new("bash")
            .arg("-n")
            .arg(script_path(script))
            .output()
            .expect("run bash -n");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!("bash -n failed for {script}: {stderr}");
        }
    }
}

#[test]
fn github_app_docs_and_scripts_include_expected_permissions_and_flow() {
    let docs =
        std::fs::read_to_string(repo_root().join("docs").join("setup.md")).expect("read docs");
    let mint_script = std::fs::read_to_string(script_path("mint-gh-app-installation-token.sh"))
        .expect("read mint script");
    let protect_script = std::fs::read_to_string(script_path("protect-main-branch.sh"))
        .expect("read protect script");

    assert!(docs.contains("Contents: Read & write"));
    assert!(docs.contains("Pull requests: Read & write"));
    assert!(docs.contains("reader app"));
    assert!(docs.contains("Device Flow"));
    assert!(docs.contains("publisher_client_id"));
    assert!(docs.contains("plain `git clone https://github.com/amxv/zodex.git` works"));
    assert!(docs.contains("zodex github request-push"));
    assert!(docs.contains("zodex github grant-push"));
    assert!(docs.contains("zodex github revoke-push"));
    assert!(docs.contains("--forget-local-auth"));
    assert!(docs.contains("list-grants"));
    assert!(docs.contains("temporary repo-scoped direct push access"));
    assert!(docs.contains("opens the GitHub verification URL automatically"));
    assert!(docs.contains("default active grant TTL is `30m`"));
    assert!(docs.contains("does not persist refresh-token state"));
    assert!(docs.contains("Expired grants stop working in the credential-helper path"));
    assert!(mint_script.contains("GITHUB_APP_INSTALLATION_ID"));
    assert!(
        mint_script.contains(r#"permissions_json='{"contents":"write","pull_requests":"write"}'"#)
    );
    assert!(mint_script.contains("access_tokens"));
    assert!(protect_script.contains("Upgrade to GitHub Pro or make this repository public"));
}
