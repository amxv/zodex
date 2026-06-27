#[test]
fn cargo_manifest_exposes_zodex_binary_names() {
    let manifest = include_str!("../Cargo.toml");

    assert!(manifest.contains("name = \"zodex\""));
    assert!(manifest.contains("name = \"zodex-agent\""));
    assert!(manifest.contains("name = \"zodex-client\""));
    assert!(manifest.contains("name = \"zodexd\""));
    assert!(manifest.contains("name = \"zodex-prd\""));
}
