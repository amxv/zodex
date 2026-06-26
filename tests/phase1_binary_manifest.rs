#[test]
fn cargo_manifest_exposes_phase1_binary_names() {
    let manifest = include_str!("../Cargo.toml");

    assert!(manifest.contains("name = \"zodex\""));
    assert!(manifest.contains("name = \"zodexd\""));
    assert!(manifest.contains("name = \"computer-mcp\""));
    assert!(manifest.contains("name = \"computer-mcpd\""));
}
