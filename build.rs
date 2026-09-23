use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const EMBED_REQUIRED_ENV: &str = "ZODEX_LIVEBOARD_EMBED_REQUIRED";

fn main() {
    println!("cargo:rerun-if-env-changed={EMBED_REQUIRED_ENV}");
    println!("cargo:rerun-if-changed=apps/liveboard/dist");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR"));
    let generated = out_dir.join("liveboard_assets.rs");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let profile = env::var("PROFILE").unwrap_or_default();
    let explicitly_required = matches!(
        env::var(EMBED_REQUIRED_ENV).ok().as_deref(),
        Some("1" | "true" | "yes")
    );
    let supported = matches!(target_os.as_str(), "linux" | "macos" | "windows");
    let required = supported && (profile == "release" || explicitly_required);

    if !supported {
        write_unavailable(
            &generated,
            "Liveboard is only embedded in Linux, macOS, and Windows operator builds",
        );
        return;
    }

    let dist = Path::new("apps/liveboard/dist");
    if !dist.join("index.html").is_file() {
        if required {
            panic!(
                "Liveboard assets are required for this build but apps/liveboard/dist/index.html is missing. Run `cd apps/liveboard && bun install --frozen-lockfile && bun run build` before Cargo."
            );
        }
        write_unavailable(
            &generated,
            "Liveboard assets were not embedded in this development build. Run `cd apps/liveboard && bun run build`, then rebuild Zodex with ZODEX_LIVEBOARD_EMBED_REQUIRED=1.",
        );
        return;
    }

    let mut files = Vec::new();
    collect_files(dist, dist, &mut files);
    files.sort();
    let mut source = String::from(
        "pub(super) const EMBEDDED_AVAILABLE: bool = true;\n\
         pub(super) const EMBEDDED_UNAVAILABLE_REASON: &str = \"\";\n\
         pub(super) static EMBEDDED_ASSETS: &[EmbeddedAsset] = &[\n",
    );
    for relative in files {
        let absolute = dist.join(&relative).canonicalize().unwrap_or_else(|error| {
            panic!(
                "failed to resolve Liveboard asset {}: {error}",
                relative.display()
            )
        });
        source.push_str("    EmbeddedAsset { path: ");
        source.push_str(&format!("{:?}", slash_path(&relative)));
        source.push_str(", bytes: include_bytes!(");
        source.push_str(&format!("{:?}", absolute.display().to_string()));
        source.push_str(") },\n");
        println!("cargo:rerun-if-changed={}", dist.join(&relative).display());
    }
    source.push_str("];\n");
    fs::write(&generated, source).expect("failed to write generated Liveboard asset table");
}

fn collect_files(root: &Path, directory: &Path, files: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| {
            panic!(
                "failed to read Liveboard dist {}: {error}",
                directory.display()
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("failed to enumerate Liveboard dist: {error}"));
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().unwrap_or_else(|error| {
            panic!(
                "failed to inspect Liveboard asset {}: {error}",
                path.display()
            )
        });
        if file_type.is_dir() {
            collect_files(root, &path, files);
        } else if file_type.is_file() {
            files.push(
                path.strip_prefix(root)
                    .expect("Liveboard asset escaped dist root")
                    .to_path_buf(),
            );
        }
    }
}

fn write_unavailable(path: &Path, reason: &str) {
    let source = format!(
        "pub(super) const EMBEDDED_AVAILABLE: bool = false;\n\
         pub(super) const EMBEDDED_UNAVAILABLE_REASON: &str = {reason:?};\n\
         pub(super) static EMBEDDED_ASSETS: &[EmbeddedAsset] = &[];\n"
    );
    fs::write(path, source).expect("failed to write Liveboard development asset stub");
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}
