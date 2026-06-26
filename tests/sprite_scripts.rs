use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn setup_doc_describes_sprite_first_zodex_flow() {
    let setup =
        std::fs::read_to_string(repo_root().join("docs").join("setup.md")).expect("read setup doc");

    assert!(setup.contains("zodex sprite setup"));
    assert!(setup.contains("npx wrangler deploy"));
    assert!(setup.contains("vars.SPRITE_ORIGIN"));
    assert!(setup.contains("zodex github request-push"));
    assert!(setup.contains("zodex github grant-push"));
    assert!(setup.contains("zodex github revoke-push"));
    assert!(setup.contains("--forget-local-auth"));
    assert!(setup.contains("--ttl <duration>"));
    assert!(setup.contains("--no-ttl"));
    assert!(setup.contains("--cache-refresh-token"));
    assert!(setup.contains("read-only GitHub access"));
    assert!(setup.contains("temporary repo-scoped direct push access"));
    assert!(setup.contains("Expired grants stop working in the credential-helper path"));
    assert!(setup.contains("canonical repository slug is `amxv/zodex`"));
    assert!(setup.contains("--repo amxv/zodex"));
    assert!(setup.contains("https://github.com/amxv/zodex.git"));
    let deprecated_deploy_path = ["Run", "pod"].join("");
    let deprecated_vm_path = ['V', 'P', 'S'].iter().collect::<String>();
    assert!(!setup.contains(&deprecated_deploy_path));
    assert!(!setup.contains(&deprecated_vm_path));
}
