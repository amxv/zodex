#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-1}"
export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"

if [[ "${ZODEX_ALLOW_RUSTC_WRAPPER:-0}" != "1" ]]; then
  unset RUSTC_WRAPPER
fi

echo "==> cargo fmt --check"
cargo fmt --check

echo "==> cargo clippy --all-targets -- -D warnings"
cargo clippy --quiet --all-targets -- -D warnings

echo "==> source file LOC guard"
cargo test --quiet --test source_file_size source_files_stay_under_1000_lines

echo "==> cargo test"
cargo test --quiet
