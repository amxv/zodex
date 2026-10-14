use std::{env, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../Cargo.toml");
    println!("cargo:rerun-if-changed={}", root.display());
    let manifest = fs::read_to_string(root).expect("read root Cargo.toml");
    let version = manifest
        .lines()
        .take_while(|line| !line.starts_with("[[bin]]"))
        .find_map(|line| {
            line.strip_prefix("version = \"")
                .and_then(|value| value.strip_suffix('"'))
        })
        .expect("Zodex package version");
    println!("cargo:rustc-env=ZODEX_OPERATOR_VERSION={version}");
}
