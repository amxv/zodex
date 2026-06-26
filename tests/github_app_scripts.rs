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
        "agent-create-pr.sh",
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
    let docs = std::fs::read_to_string(repo_root().join("docs").join("github-app-agent-auth.md"))
        .expect("read docs");
    let mint_script = std::fs::read_to_string(script_path("mint-gh-app-installation-token.sh"))
        .expect("read mint script");
    let protect_script = std::fs::read_to_string(script_path("protect-main-branch.sh"))
        .expect("read protect script");
    let pr_script =
        std::fs::read_to_string(script_path("agent-create-pr.sh")).expect("read pr script");

    assert!(docs.contains("Contents: Read & write"));
    assert!(docs.contains("Pull requests: Read & write"));
    assert!(docs.contains("private GitHub App"));
    assert!(docs.contains("reader app"));
    assert!(docs.contains("reader_installation_id"));
    assert!(docs.contains("publisher daemon holds the GitHub App private key"));
    assert!(docs.contains("plain `git clone https://github.com/<owner>/<repo>.git` works"));
    assert!(docs.contains("short-lived reader credential helper"));
    assert!(docs.contains("git bundle"));
    assert!(docs.contains("zodex start"));
    assert!(docs.contains("GitHub Pro"));
    assert!(mint_script.contains("GITHUB_APP_INSTALLATION_ID"));
    assert!(
        mint_script.contains(r#"permissions_json='{"contents":"write","pull_requests":"write"}'"#)
    );
    assert!(mint_script.contains("access_tokens"));
    assert!(protect_script.contains("Upgrade to GitHub Pro or make this repository public"));
    assert!(pr_script.contains("gh pr create"));
    assert!(pr_script.contains("git push"));
}
