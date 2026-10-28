#!/usr/bin/env bash
set -euo pipefail

component_root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "${component_root}/../.." && pwd)"
output="${1:-${repo_root}/target/debug/Zodex.app}"
binary="${output}/Contents/MacOS/zodex-menubar"
broker="${output}/Contents/Resources/zodex-keychain-v1"
version="$(sed -n '/^\[package\]/,/^\[/ s/^version = "\([^"]*\)"/\1/p' "${repo_root}/Cargo.toml" | head -n 1)"
bundle_version="${version%%[-+]*}"

[[ -n "${version}" ]] || {
  echo "failed to resolve package version from Cargo.toml" >&2
  exit 1
}

rm -rf "${output}"
mkdir -p "$(dirname "${binary}")"
mkdir -p "$(dirname "${broker}")"
cp "${component_root}/Info.plist" "${output}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString ${bundle_version}" "${output}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion ${bundle_version}" "${output}/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :ZodexVersion ${version}" "${output}/Contents/Info.plist"

swiftc -O \
  -target arm64-apple-macos13.0 \
  "${component_root}/Sources/AppDelegate.swift" \
  "${component_root}/Sources/LiveboardCopyHUD.swift" \
  "${component_root}/Sources/SystemIntegration.swift" \
  "${component_root}/Sources/main.swift" \
  -o "${binary}"

# The versioned broker is immutable *after installation*. Bundle it with each
# menu release so upgrades from old CLIs (which embed an old install.sh) can
# bootstrap it on their first Keychain call without downloading more files.
helper_binary="${ZODEX_KEYCHAIN_HELPER_BINARY:-${repo_root}/apps/keychain-helper/target/aarch64-apple-darwin/release/zodex-keychain-v1}"
if [[ ! -x "${helper_binary}" ]]; then
  cargo build --locked --release --manifest-path "${repo_root}/apps/keychain-helper/Cargo.toml" --target aarch64-apple-darwin
fi
install -m 0700 "${helper_binary}" "${broker}"
codesign --verify --strict "${broker}"
codesign --force --sign - "${output}" >/dev/null
