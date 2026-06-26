use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn script_path(name: &str) -> PathBuf {
    repo_root().join("scripts").join(name)
}

#[test]
fn sprite_scripts_have_valid_bash_syntax() {
    let scripts = ["setup-sprite.sh", "sprite-services.sh", "upgrade-sprite.sh"];

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
fn sprite_docs_prefer_zodex_control_plane_commands() {
    let upgrade_script =
        std::fs::read_to_string(script_path("upgrade-sprite.sh")).expect("read upgrade script");
    let setup_script =
        std::fs::read_to_string(script_path("setup-sprite.sh")).expect("read setup script");
    let service_script = std::fs::read_to_string(script_path("sprite-services.sh"))
        .expect("read sprite services script");
    let runbook = std::fs::read_to_string(
        repo_root()
            .join("docs")
            .join("agent-sprites-setup-runbook.md"),
    )
    .expect("read Sprite runbook");
    let deployment_notes =
        std::fs::read_to_string(repo_root().join("docs").join("deployment-notes.md"))
            .expect("read deployment notes");

    assert!(upgrade_script.contains("scripts/sprite-services.sh"));
    assert!(upgrade_script.contains("--force-recreate"));
    assert!(
        upgrade_script.contains("verifying local Sprite health via http://127.0.0.1:8080/health")
    );
    assert!(upgrade_script.contains("computer-mcp-agent unexpectedly gained read access"));
    assert!(upgrade_script.contains("verifying publisher socket permissions"));
    assert!(setup_script.contains("--force-recreate"));
    assert!(setup_script.contains("verify agent still cannot read publisher private key"));
    assert!(
        setup_script
            .contains("verifying publisher socket permissions after Sprite service handoff")
    );
    assert!(service_script.contains("--force-recreate"));
    assert!(runbook.contains("zodex sprite setup"));
    assert!(runbook.contains("zodex sprite upgrade"));
    assert!(runbook.contains("zodex sprite sync"));
    assert!(runbook.contains("zodex github grant-push"));
    assert!(
        runbook.contains("uploads the local `zodex`, `zodexd`, and `computer-mcp-prd` binaries")
    );
    assert!(deployment_notes.contains("zodex sprite upgrade"));
    assert!(deployment_notes.contains("zodex sprite sync"));
    assert!(deployment_notes.contains("run the remote Rust install path"));
}
