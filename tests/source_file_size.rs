use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};

const MAX_LOC: usize = 1_000;
const SOURCE_EXTENSIONS: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "py", "go"];
const EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".next",
    "coverage",
    "vendor",
    ".venv",
    "__pycache__",
    "generated",
    "tmp",
];

#[derive(Debug, Eq, PartialEq)]
struct SourceFileSize {
    path: PathBuf,
    loc: usize,
}

#[test]
fn source_files_stay_under_1000_lines() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut oversized_files = Vec::new();

    collect_oversized_source_files(repo_root, repo_root, &mut oversized_files);
    oversized_files.sort_by_key(|file| (Reverse(file.loc), file.path.clone()));

    assert!(
        oversized_files.is_empty(),
        "source files over {MAX_LOC} LOC:\n{}",
        format_source_file_sizes(&oversized_files)
    );
}

fn collect_oversized_source_files(
    repo_root: &Path,
    directory: &Path,
    oversized_files: &mut Vec<SourceFileSize>,
) {
    let mut entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| {
            panic!(
                "failed to collect entries in {}: {error}",
                directory.display()
            )
        });
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type().unwrap_or_else(|error| {
            panic!("failed to read file type for {}: {error}", path.display())
        });

        if file_type.is_dir() {
            if should_skip_dir(&path) {
                continue;
            }
            collect_oversized_source_files(repo_root, &path, oversized_files);
            continue;
        }

        if !file_type.is_file() || !is_source_file(&path) {
            continue;
        }

        let contents = fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!("failed to read source file {}: {error}", path.display())
        });
        let loc = contents.lines().count();
        if loc > MAX_LOC {
            oversized_files.push(SourceFileSize {
                path: path.strip_prefix(repo_root).unwrap_or(&path).to_path_buf(),
                loc,
            });
        }
    }
}

fn should_skip_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| EXCLUDED_DIRS.contains(&name))
}

fn is_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| SOURCE_EXTENSIONS.contains(&extension))
}

fn format_source_file_sizes(files: &[SourceFileSize]) -> String {
    files
        .iter()
        .map(|file| format!("{:>5} {}", file.loc, file.path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}
