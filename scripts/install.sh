#!/usr/bin/env bash
set -euo pipefail

SCRIPT_VERSION="0.2.1"

ZODEX_VERSION="${ZODEX_VERSION:-latest}"
ZODEX_INSTALL_MODE="${ZODEX_INSTALL_MODE:-auto}"
ZODEX_REPO="${ZODEX_REPO:-amxv/zodex}"
ZODEX_ASSET_URL="${ZODEX_ASSET_URL:-}"
ZODEX_SOURCE_REF="${ZODEX_SOURCE_REF:-main}"
ZODEX_BINARY_SOURCE_DIR="${ZODEX_BINARY_SOURCE_DIR:-}"
ZODEX_INSTALL_DIR="${ZODEX_INSTALL_DIR:-/usr/local/bin}"
ZODEX_CODESIGN_IDENTITY="${ZODEX_CODESIGN_IDENTITY:-}"
ZODEX_CONFIG_PATH="${ZODEX_CONFIG_PATH:-/etc/zodex/config.toml}"
ZODEX_STATE_DIR="${ZODEX_STATE_DIR:-/var/lib/zodex}"
ZODEX_SERVICE_PORT="${ZODEX_SERVICE_PORT:-8080}"
ZODEX_AGENT_USER="${ZODEX_AGENT_USER:-zodex-agent}"
ZODEX_AGENT_HOME="${ZODEX_AGENT_HOME:-/home/${ZODEX_AGENT_USER}}"
ZODEX_AGENT_SHELL="${ZODEX_AGENT_SHELL:-/bin/bash}"
ZODEX_DEFAULT_WORKDIR="${ZODEX_DEFAULT_WORKDIR:-/workspace}"
ZODEX_PUBLISHER_USER="${ZODEX_PUBLISHER_USER:-zodex-publisher}"
ZODEX_PUBLISHER_HOME="${ZODEX_PUBLISHER_HOME:-/nonexistent}"
ZODEX_SERVICE_GROUP="${ZODEX_SERVICE_GROUP:-zodex}"
ZODEX_DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES=134217728
ZODEX_GIT_USER_NAME_WAS_SET=0
if [[ "${ZODEX_GIT_USER_NAME+x}" == "x" ]]; then
  ZODEX_GIT_USER_NAME_WAS_SET=1
fi
ZODEX_GIT_USER_EMAIL_WAS_SET=0
if [[ "${ZODEX_GIT_USER_EMAIL+x}" == "x" ]]; then
  ZODEX_GIT_USER_EMAIL_WAS_SET=1
fi
ZODEX_GIT_USER_NAME="${ZODEX_GIT_USER_NAME:-Zodex Agent}"
ZODEX_GIT_USER_EMAIL="${ZODEX_GIT_USER_EMAIL:-zodex-agent@local.invalid}"
ZODEX_READER_KEY_DIR="${ZODEX_READER_KEY_DIR:-/etc/zodex/reader}"
ZODEX_PUBLISHER_KEY_DIR="${ZODEX_PUBLISHER_KEY_DIR:-/etc/zodex/publisher}"

DISTRO_ID="unknown"
DISTRO_LIKE=""
OS="unknown"
ARCH="unknown"
TARGET_TRIPLE="unknown"
TMP_DIR=""

log() {
  printf '[zodex install] %s\n' "$*"
}

warn() {
  printf '[zodex install] WARNING: %s\n' "$*" >&2
}

die() {
  printf '[zodex install] ERROR: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  if [[ -n "${TMP_DIR}" && -d "${TMP_DIR}" ]]; then
    /bin/rm -rf "${TMP_DIR}"
  fi
}


is_root() {
  [[ "${EUID}" -eq 0 ]]
}

resolved_install_mode() {
  case "${ZODEX_INSTALL_MODE}" in
    auto)
      printf 'operator\n'
      ;;
    operator|runtime)
      printf '%s\n' "${ZODEX_INSTALL_MODE}"
      ;;
    *)
      die "unsupported ZODEX_INSTALL_MODE=${ZODEX_INSTALL_MODE}; expected auto, operator, or runtime"
      ;;
  esac
}

need_root() {
  if ! is_root; then
    die "run as root (for example: curl ... | sudo bash)"
  fi
}

command_exists() {
  command -v "$1" >/dev/null 2>&1
}

normalize_release_version() {
  local version="$1"
  case "${version}" in
    latest|v*)
      printf '%s\n' "${version}"
      ;;
    [0-9]*)
      printf 'v%s\n' "${version}"
      ;;
    *)
      printf '%s\n' "${version}"
      ;;
  esac
}

download_file() {
  local url="$1"
  local destination="$2"
  local attempt=1
  local max_attempts=3

  while true; do
    /bin/rm -f "${destination}"
    if curl \
      --fail \
      --location \
      --silent \
      --show-error \
      --connect-timeout 15 \
      --speed-limit 1024 \
      --speed-time 20 \
      --max-time 600 \
      "${url}" \
      -o "${destination}"
    then
      return 0
    fi

    if (( attempt >= max_attempts )); then
      return 1
    fi
    warn "download failed (attempt ${attempt}/${max_attempts}); retrying ${url}"
    sleep $((attempt * 2))
    attempt=$((attempt + 1))
  done
}

resolve_nologin_shell() {
  if [[ -x /usr/sbin/nologin ]]; then
    printf '/usr/sbin/nologin\n'
    return
  fi
  if [[ -x /sbin/nologin ]]; then
    printf '/sbin/nologin\n'
    return
  fi
  printf '/bin/false\n'
}

resolve_login_shell() {
  if [[ -x "${ZODEX_AGENT_SHELL}" ]]; then
    printf '%s\n' "${ZODEX_AGENT_SHELL}"
    return
  fi
  if [[ -x /bin/bash ]]; then
    printf '/bin/bash\n'
    return
  fi
  printf '/bin/sh\n'
}

ensure_service_accounts() {
  local nologin
  local login_shell
  nologin="$(resolve_nologin_shell)"
  login_shell="$(resolve_login_shell)"

  if ! getent group "${ZODEX_SERVICE_GROUP}" >/dev/null; then
    groupadd --system "${ZODEX_SERVICE_GROUP}"
  fi

  if ! id -u "${ZODEX_AGENT_USER}" >/dev/null 2>&1; then
    useradd \
      --system \
      --gid "${ZODEX_SERVICE_GROUP}" \
      --home-dir "${ZODEX_AGENT_HOME}" \
      --create-home \
      --shell "${login_shell}" \
      "${ZODEX_AGENT_USER}"
  else
    usermod --home "${ZODEX_AGENT_HOME}" "${ZODEX_AGENT_USER}" || true
    usermod --shell "${login_shell}" "${ZODEX_AGENT_USER}" || true
  fi

  if ! id -u "${ZODEX_PUBLISHER_USER}" >/dev/null 2>&1; then
    useradd \
      --system \
      --gid "${ZODEX_SERVICE_GROUP}" \
      --home-dir "${ZODEX_PUBLISHER_HOME}" \
      --no-create-home \
      --shell "${nologin}" \
      "${ZODEX_PUBLISHER_USER}"
  else
    usermod --home "${ZODEX_PUBLISHER_HOME}" "${ZODEX_PUBLISHER_USER}" || true
    usermod --shell "${nologin}" "${ZODEX_PUBLISHER_USER}" || true
  fi
}


detect_operator_platform() {
  OS="$(uname -s)"
  case "${OS}" in
    Linux)
      case "$(uname -m)" in
        x86_64|amd64)
          ARCH="x86_64"
          TARGET_TRIPLE="x86_64-unknown-linux-gnu"
          ;;
        aarch64|arm64)
          ARCH="aarch64"
          TARGET_TRIPLE="aarch64-unknown-linux-gnu"
          ;;
        *)
          die "unsupported architecture: $(uname -m)"
          ;;
      esac
      ;;
    Darwin)
      case "$(uname -m)" in
        x86_64|amd64)
          die "Zodex operator releases support Apple Silicon macOS only; Intel macOS is unsupported"
          ;;
        aarch64|arm64)
          ARCH="aarch64"
          TARGET_TRIPLE="aarch64-apple-darwin"
          ;;
        *)
          die "unsupported architecture: $(uname -m)"
          ;;
      esac
      ;;
    *)
      die "unsupported operating system for operator install: ${OS}"
      ;;
  esac

  log "detected os=${OS} arch=${ARCH} target=${TARGET_TRIPLE}"
}

sha256_verify() {
  local expected_file="$1"
  local archive="$2"
  local expected
  local actual=""

  expected="$(awk '{print $1}' "${expected_file}")"
  [[ -n "${expected}" ]] || die "checksum file is empty: ${expected_file}"

  if command_exists sha256sum; then
    actual="$(sha256sum "${archive}" | awk '{print $1}')"
  elif command_exists shasum; then
    actual="$(shasum -a 256 "${archive}" | awk '{print $1}')"
  else
    die "missing sha256sum or shasum for checksum verification"
  fi

  [[ "${expected}" == "${actual}" ]] || die "checksum mismatch for ${archive}"
}

resolve_release_checksum_url() {
  local asset_url="$1"
  printf '%s.sha256\n' "${asset_url}"
}

operator_local_runtime_dir() {
  local state_home="${XDG_STATE_HOME:-${HOME}/.local/state}"
  printf '%s/zodex/local/runtime\n' "${state_home}"
}

ensure_local_stopped_before_operator_replace() {
  local installed_zodex="$1"

  # Fresh installs cannot be replacing the executable that owns an active
  # Local runtime.
  [[ -e "${installed_zodex}" ]] || return 0
  case "$(uname -s)" in
    Darwin|Linux) ;;
    *) return 0 ;;
  esac

  local runtime_dir
  runtime_dir="$(operator_local_runtime_dir)"
  local marker
  for marker in state.json discovery.json bootstrap.json; do
    if [[ -e "${runtime_dir}/${marker}" ]]; then
      die "Zodex Local runtime state is present at ${runtime_dir}; run 'zodex local stop' before upgrading or replacing ${installed_zodex}"
    fi
  done
}

resolve_operator_codesign_identity() {
  [[ "$(uname -s)" == "Darwin" ]] || return 0
  [[ "${ZODEX_CODESIGN_IDENTITY}" != "-" ]] || return 0

  local identity="${ZODEX_CODESIGN_IDENTITY:-Zodex Local Development}"
  if security find-identity -v -p codesigning 2>/dev/null \
      | grep -Fq "\"${identity}\""; then
    printf '%s\n' "${identity}"
  elif [[ -n "${ZODEX_CODESIGN_IDENTITY}" ]]; then
    die "requested code-signing identity '${identity}' is not available"
  fi
}

sign_local_operator_if_configured() {
  local binary="$1"
  [[ "$(uname -s)" == "Darwin" ]] || return 0
  file -b "${binary}" | grep -Fq 'Mach-O' || return 0

  local signature
  signature="$(codesign -dv --verbose=4 "${binary}" 2>&1 || true)"
  [[ "${signature}" == *"Signature=adhoc"* || "${signature}" == *"adhoc,"* ]] || return 0

  local identity
  identity="$(resolve_operator_codesign_identity)"
  [[ -n "${identity}" ]] || return 0
  codesign --force --sign "${identity}" --identifier com.amxv.zodex.local "${binary}" \
    || die "failed to sign the Local operator with '${identity}'"
  codesign --verify --strict "${binary}" \
    || die "the signed Local operator failed verification"
  log "signed Local operator with ${identity}"
}

install_operator_binary_atomically() {
  local source="$1"
  local destination="$2"
  local destination_dir
  destination_dir="$(dirname "${destination}")"
  local temporary
  temporary="$(mktemp "${destination_dir}/.zodex-install.XXXXXX")"

  if ! install -m 0755 "${source}" "${temporary}"; then
    /bin/rm -f "${temporary}"
    return 1
  fi
  sign_local_operator_if_configured "${temporary}"
  if ! mv -f "${temporary}" "${destination}"; then
    /bin/rm -f "${temporary}"
    return 1
  fi
}

running_zodex_menubar_pids() {
  local app_executable="$1"
  local lock_file
  lock_file="/tmp/zodex-menubar-$(/usr/bin/id -u).lock"

  /usr/bin/pgrep -f -x "${app_executable}" 2>/dev/null || true
  # The lock stays open for the lifetime of the app and remains reliable even
  # if an upgrade has renamed or removed the bundle underneath the process.
  if [[ -e "${lock_file}" && -x /usr/sbin/lsof ]]; then
    /usr/sbin/lsof -t "${lock_file}" 2>/dev/null || true
  fi
}

stop_zodex_menubar_before_replace() {
  local app_executable="$1"
  local pids
  pids="$(running_zodex_menubar_pids "${app_executable}")"
  [[ -n "${pids}" ]] || return 1

  local pid
  for pid in ${pids}; do
    kill -TERM "${pid}" >/dev/null 2>&1 || true
  done
  for _ in {1..40}; do
    pids="$(running_zodex_menubar_pids "${app_executable}")"
    [[ -n "${pids}" ]] || return 0
    sleep 0.05
  done

  for pid in ${pids}; do
    kill -KILL "${pid}" >/dev/null 2>&1 || true
  done
  for _ in {1..40}; do
    pids="$(running_zodex_menubar_pids "${app_executable}")"
    [[ -n "${pids}" ]] || return 0
    sleep 0.05
  done

  die "failed to stop the running Zodex menu bar app before replacing ${app_executable}"
}

install_operator_binaries_from_dir() {
  local src_dir="$1"
  local install_dir="${ZODEX_INSTALL_DIR}"

  if [[ "${install_dir}" == "/usr/local/bin" ]] && ! is_root; then
    install_dir="${HOME}/.local/bin"
    log "no root privileges; installing operator CLI to ${install_dir}"
  fi

  [[ -x "${src_dir}/zodex" ]] || die "missing executable ${src_dir}/zodex"
  install -d -m 0755 "${install_dir}"
  ensure_local_stopped_before_operator_replace "${install_dir}/zodex"

  local app_destination=""
  local app_executable=""
  local app_temporary=""
  local app_backup=""
  local app_was_running=0
  if [[ "$(uname -s)" == "Darwin" && -d "${src_dir}/Zodex.app" ]]; then
    app_destination="${install_dir}/Zodex.app"
    app_executable="${app_destination}/Contents/MacOS/zodex-menubar"
    app_temporary="${install_dir}/.Zodex.app.install.$$"
    app_backup="${install_dir}/.Zodex.app.backup.$$"

    /bin/rm -rf "${app_temporary}" "${app_backup}"
    /bin/cp -R "${src_dir}/Zodex.app" "${app_temporary}" \
      || die "failed to stage Zodex menu bar app"
  fi

  install_operator_binary_atomically "${src_dir}/zodex" "${install_dir}/zodex"

  if [[ -n "${app_destination}" ]]; then
    if [[ -x "${app_executable}" ]] \
        && stop_zodex_menubar_before_replace "${app_executable}"; then
      app_was_running=1
    fi

    if [[ -e "${app_destination}" ]]; then
      /bin/mv "${app_destination}" "${app_backup}" \
        || {
          [[ "${app_was_running}" -eq 0 ]] || /usr/bin/open -g "${app_destination}" || true
          die "failed to stage existing Zodex menu bar app for replacement"
        }
    fi
    if ! /bin/mv "${app_temporary}" "${app_destination}"; then
      [[ ! -e "${app_backup}" ]] || /bin/mv "${app_backup}" "${app_destination}" || true
      [[ "${app_was_running}" -eq 0 ]] || /usr/bin/open -g "${app_destination}" || true
      die "failed to install Zodex menu bar app"
    fi
    /bin/rm -rf "${app_backup}"

    if [[ "${app_was_running}" -eq 1 ]]; then
      /usr/bin/open -g "${app_destination}" \
        || warn "Zodex menu bar app was upgraded but could not be relaunched automatically"
    fi
  fi

  if [[ "${ZODEX_UPGRADE_MODE:-0}" == "1" ]]; then
    return 0
  fi

  cat <<EOF

zodex operator CLI installed.

Installed:
  ${install_dir}/zodex
EOF

  if [[ -d "${install_dir}/Zodex.app" ]]; then
    printf '  %s\n' "${install_dir}/Zodex.app"
  fi

  cat <<EOF

Verify:
  ${install_dir}/zodex --version
EOF

  case ":${PATH}:" in
    *":${install_dir}:"*)
      ;;
    *)
      cat <<EOF

${install_dir} is not currently on PATH.

For this shell:
  export PATH="${install_dir}:\$PATH"

Add the same line to your shell profile to keep it available in new terminals.
EOF
      ;;
  esac
}

install_operator_from_release() {
  local asset_url
  asset_url="$(resolve_release_asset_url)" || die "failed to resolve zodex release artifact for ${TARGET_TRIPLE}"
  log "downloading release artifact: ${asset_url}"

  local archive="${TMP_DIR}/release.tar.gz"
  local checksum="${TMP_DIR}/release.tar.gz.sha256"
  download_file "${asset_url}" "${archive}" \
    || die "failed to download release artifact after retries: ${asset_url}"
  download_file "$(resolve_release_checksum_url "${asset_url}")" "${checksum}" \
    || die "failed to download release checksum after retries"
  sha256_verify "${checksum}" "${archive}"
  tar -xzf "${archive}" -C "${TMP_DIR}"

  local cli_path
  cli_path="$(find "${TMP_DIR}" -type f -name zodex -print -quit)"
  [[ -n "${cli_path}" ]] || die "release artifact did not contain zodex"
  install_operator_binaries_from_dir "$(dirname "${cli_path}")"
}

run_operator_install() {
  detect_operator_platform
  TMP_DIR="$(mktemp -d)"
  trap cleanup EXIT

  if [[ -n "${ZODEX_BINARY_SOURCE_DIR}" ]]; then
    install_operator_binaries_from_dir "${ZODEX_BINARY_SOURCE_DIR}"
  else
    install_operator_from_release
  fi
}

detect_platform() {
  OS="$(uname -s)"
  [[ "${OS}" == "Linux" ]] || die "Linux only"
  [[ -f /etc/os-release ]] || die "/etc/os-release not found"

  # shellcheck disable=SC1091
  source /etc/os-release
  DISTRO_ID="${ID:-unknown}"
  DISTRO_LIKE="${ID_LIKE:-}"

  case "$(uname -m)" in
    x86_64|amd64)
      ARCH="x86_64"
      TARGET_TRIPLE="x86_64-unknown-linux-gnu"
      ;;
    aarch64|arm64)
      ARCH="aarch64"
      TARGET_TRIPLE="aarch64-unknown-linux-gnu"
      ;;
    *)
      die "unsupported architecture: $(uname -m)"
      ;;
  esac

  if [[ "${DISTRO_ID}" != "ubuntu" && "${DISTRO_ID}" != "debian" ]]; then
    warn "distro ${DISTRO_ID} is not first-class tested for v1; continuing with best effort"
  fi

  log "detected distro=${DISTRO_ID} arch=${ARCH} target=${TARGET_TRIPLE}"
}

install_runtime_prerequisites() {
  if command_exists apt-get; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -y
    apt-get install -y --no-install-recommends \
      curl ca-certificates systemd tar gzip git ccache ninja-build

    return
  fi

  if command_exists dnf; then
    dnf install -y curl ca-certificates systemd tar gzip git ccache ninja-build
    return
  fi

  if command_exists yum; then
    yum install -y curl ca-certificates systemd tar gzip git ccache ninja-build
    return
  fi

  die "unsupported package manager (expected apt-get, dnf, or yum)"
}

install_build_prerequisites() {
  if command_exists apt-get; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -y
    apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev git
    return
  fi

  if command_exists dnf; then
    dnf install -y gcc gcc-c++ make pkgconf-pkg-config openssl-devel git
    return
  fi

  if command_exists yum; then
    yum install -y gcc gcc-c++ make pkgconfig openssl-devel git
    return
  fi

  die "unsupported package manager for source builds (expected apt-get, dnf, or yum)"
}

resolve_release_asset_url() {
  if [[ -n "${ZODEX_ASSET_URL}" ]]; then
    printf '%s\n' "${ZODEX_ASSET_URL}"
    return
  fi

  local archive_name="zodex-${TARGET_TRIPLE}.tar.gz"
  local version
  version="$(normalize_release_version "${ZODEX_VERSION}")"
  if [[ "${version}" == "latest" ]]; then
    printf 'https://github.com/%s/releases/latest/download/%s\n' \
      "${ZODEX_REPO}" "${archive_name}"
  else
    printf 'https://github.com/%s/releases/download/%s/%s\n' \
      "${ZODEX_REPO}" "${version}" "${archive_name}"
  fi
}

install_binaries_from_dir() {
  local src_dir="$1"
  local daemon_src="${src_dir}/zodexd"
  if [[ ! -x "${daemon_src}" && -x "${src_dir}/zodexd" ]]; then
    daemon_src="${src_dir}/zodexd"
  fi

  [[ -x "${src_dir}/zodex-agent" ]] || die "missing executable ${src_dir}/zodex-agent"
  [[ -x "${src_dir}/git-remote-zodex" ]] || die "missing executable ${src_dir}/git-remote-zodex"
  [[ -x "${daemon_src}" ]] || die "missing executable ${src_dir}/zodexd or ${src_dir}/zodexd"
  [[ -x "${src_dir}/zodex-prd" ]] || die "missing executable ${src_dir}/zodex-prd"

  install -d -m 0755 "${ZODEX_INSTALL_DIR}"
  /bin/rm -f "${ZODEX_INSTALL_DIR}/zodex"
  install -m 0755 "${src_dir}/zodex-agent" "${ZODEX_INSTALL_DIR}/zodex-agent"
  install -m 0755 "${src_dir}/git-remote-zodex" "${ZODEX_INSTALL_DIR}/git-remote-zodex"
  install -m 0755 "${daemon_src}" "${ZODEX_INSTALL_DIR}/zodexd"
  install -m 0755 "${src_dir}/zodex-prd" "${ZODEX_INSTALL_DIR}/zodex-prd"
}

install_binaries_from_release() {
  local asset_url
  asset_url="$(resolve_release_asset_url)" || return 1
  log "downloading release artifact: ${asset_url}"

  local archive="${TMP_DIR}/release.tar.gz"
  local checksum="${TMP_DIR}/release.tar.gz.sha256"
  download_file "${asset_url}" "${archive}" || return 1
  download_file "$(resolve_release_checksum_url "${asset_url}")" "${checksum}" || return 1
  sha256_verify "${checksum}" "${archive}"
  tar -xzf "${archive}" -C "${TMP_DIR}"

  local cli_path
  cli_path="$(find "${TMP_DIR}" -type f \( -name zodex -o -name zodex \) -print -quit)"
  [[ -n "${cli_path}" ]] || return 1

  local extracted_dir
  extracted_dir="$(dirname "${cli_path}")"
  install_binaries_from_dir "${extracted_dir}"
}

install_rust_toolchain_if_needed() {
  if command_exists cargo && command_exists rustc; then
    return
  fi

  log "rust toolchain missing, installing via rustup"
  curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal
  # shellcheck disable=SC1090
  source "${HOME}/.cargo/env"
}

install_binaries_from_source() {
  log "falling back to source build from ${ZODEX_REPO}@${ZODEX_SOURCE_REF}"
  install_build_prerequisites
  install_rust_toolchain_if_needed

  local src_dir="${TMP_DIR}/source"
  git clone --depth 1 --branch "${ZODEX_SOURCE_REF}" \
    "https://github.com/${ZODEX_REPO}.git" "${src_dir}"

  (
    cd "${src_dir}"
    local cargo_args=(build --release --bin zodex-agent --bin git-remote-zodex --bin zodexd --bin zodex-prd)
    if cargo "${cargo_args[@]}"; then
      :
    else
      cargo "${cargo_args[@]}"
    fi
  )

  install_binaries_from_dir "${src_dir}/target/release"
}

ensure_dirs_and_config() {
  local config_dir
  config_dir="$(dirname "${ZODEX_CONFIG_PATH}")"

  install -d -m 0750 -o root -g "${ZODEX_SERVICE_GROUP}" "${config_dir}"
  install -d -m 0750 -o root -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_STATE_DIR}"
  install -d -m 0750 -o "${ZODEX_PUBLISHER_USER}" -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_STATE_DIR}/publisher"
  install -d -m 0750 -o "${ZODEX_PUBLISHER_USER}" -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_STATE_DIR}/publisher/run"
  install -d -m 0750 -o "${ZODEX_PUBLISHER_USER}" -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_STATE_DIR}/publisher/logs"
  install -d -m 0750 -o "${ZODEX_AGENT_USER}" -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_STATE_DIR}/push-grants"
  install -d -m 0750 -o root -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_READER_KEY_DIR}"
  install -d -m 0750 -o root -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_PUBLISHER_KEY_DIR}"
  install -d -m 0750 -o "${ZODEX_AGENT_USER}" -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_AGENT_HOME}"
  install -d -m 0750 -o "${ZODEX_AGENT_USER}" -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_DEFAULT_WORKDIR}"

  if [[ ! -f "${ZODEX_CONFIG_PATH}" ]]; then
    local api_key
    if command_exists openssl; then
      api_key="$(openssl rand -hex 24)"
    else
      api_key="$(tr -dc 'A-Za-z0-9' </dev/urandom | head -c 48)"
    fi

    umask 077
    cat >"${ZODEX_CONFIG_PATH}" <<EOF
api_key = "${api_key}"
service_port = ${ZODEX_SERVICE_PORT}
agent_user = "${ZODEX_AGENT_USER}"
agent_home = "${ZODEX_AGENT_HOME}"
default_workdir = "${ZODEX_DEFAULT_WORKDIR}"
publisher_user = "${ZODEX_PUBLISHER_USER}"

# Most installs can keep the built-in defaults.
# Add only the settings you actually need to override.

# Required GitHub App settings:
# reader_app_id = 123456
# reader_installation_id = 234567890
# publisher_client_id = "Iv1.abc123example"
# publisher_app_id = 345678
#
# [[publisher_targets]]
# id = "amxv/zodex"
# repo = "amxv/zodex"
# default_base = "main"
# installation_id = 456789012
EOF
    log "created config at ${ZODEX_CONFIG_PATH}"
  fi

  migrate_runtime_config

  chgrp "${ZODEX_SERVICE_GROUP}" "${ZODEX_CONFIG_PATH}"
  chmod 0640 "${ZODEX_CONFIG_PATH}"
}

migrate_runtime_config() {
  [[ -f "${ZODEX_CONFIG_PATH}" ]] || return 0

  local legacy_runtime_config=0
  local legacy_managed_github_config=0
  local legacy_service_port=""
  local config_tmp=""

  if grep -Eq '^[[:space:]]*(bind_port|http_bind_port|tls_[A-Za-z0-9_]+)[[:space:]]*=' "${ZODEX_CONFIG_PATH}"; then
    legacy_runtime_config=1
  fi
  if grep -q '^# BEGIN ZODEX_GH_APPS_MANAGED$' "${ZODEX_CONFIG_PATH}" \
    && ! grep -Eq '^[[:space:]]*publisher_client_id[[:space:]]*=' "${ZODEX_CONFIG_PATH}"; then
    legacy_managed_github_config=1
  fi
  legacy_service_port="$(awk -F= '
    /^[[:space:]]*http_bind_port[[:space:]]*=/ {
      value=$2
      gsub(/[[:space:]]/, "", value)
      print value
      exit
    }
  ' "${ZODEX_CONFIG_PATH}")"

  config_tmp="$(mktemp "${ZODEX_CONFIG_PATH}.XXXXXX")"
  if ! awk \
    -v legacy_runtime_config="${legacy_runtime_config}" \
    -v legacy_managed_github_config="${legacy_managed_github_config}" \
    -v legacy_service_port="${legacy_service_port}" \
    -v default_service_port="${ZODEX_SERVICE_PORT}" \
    -v default_bundle_bytes="${ZODEX_DEFAULT_PUBLISHER_MAX_BUNDLE_BYTES}" '
      BEGIN { seen_service_port=0 }
      /^[[:space:]]*service_port[[:space:]]*=/ {
        seen_service_port=1
        print
        next
      }
      /^[[:space:]]*(bind_port|http_bind_port|tls_[A-Za-z0-9_]+)[[:space:]]*=/ {
        next
      }
      /^[[:space:]]*publisher_max_bundle_bytes[[:space:]]*=[[:space:]]*33554432[[:space:]]*$/ {
        if (legacy_runtime_config == 1 || legacy_managed_github_config == 1) {
          print "publisher_max_bundle_bytes = " default_bundle_bytes
          next
        }
      }
      { print }
      END {
        if (!seen_service_port) {
          if (legacy_service_port != "") {
            print "service_port = " legacy_service_port
          } else {
            print "service_port = " default_service_port
          }
        }
      }
    ' "${ZODEX_CONFIG_PATH}" >"${config_tmp}"; then
    /bin/rm -f "${config_tmp}"
    die "failed to migrate runtime config at ${ZODEX_CONFIG_PATH}"
  fi

  if cmp -s "${ZODEX_CONFIG_PATH}" "${config_tmp}"; then
    /bin/rm -f "${config_tmp}"
    return 0
  fi

  mv "${config_tmp}" "${ZODEX_CONFIG_PATH}"
  log "migrated runtime config at ${ZODEX_CONFIG_PATH}"
}

run_as_agent_user() {
  if command_exists runuser; then
    runuser -u "${ZODEX_AGENT_USER}" -- env HOME="${ZODEX_AGENT_HOME}" "$@"
    return
  fi

  if command_exists sudo; then
    sudo -u "${ZODEX_AGENT_USER}" env HOME="${ZODEX_AGENT_HOME}" "$@"
    return
  fi

  local command_string=""
  local arg
  for arg in "$@"; do
    command_string+=" $(printf '%q' "${arg}")"
  done

  su -s /bin/sh "${ZODEX_AGENT_USER}" -c \
    "HOME=$(printf '%q' "${ZODEX_AGENT_HOME}")${command_string}"
}

configure_agent_git_reader_helper() {
  local helper_cmd="${ZODEX_INSTALL_DIR}/zodex-agent --config ${ZODEX_CONFIG_PATH} git-credential-helper"

  run_as_agent_user \
    git config --global --replace-all credential.https://github.com.helper "${helper_cmd}"
  run_as_agent_user \
    git config --global credential.https://github.com.useHttpPath true
}

configure_agent_git_identity() {
  local current_name=""
  local current_email=""

  current_name="$(run_as_agent_user git config --global --get user.name || true)"
  current_email="$(run_as_agent_user git config --global --get user.email || true)"

  if [[ "${ZODEX_GIT_USER_NAME_WAS_SET}" == "1" || -z "${current_name}" ]]; then
    run_as_agent_user \
      git config --global user.name "${ZODEX_GIT_USER_NAME}"
  fi
  if [[ "${ZODEX_GIT_USER_EMAIL_WAS_SET}" == "1" || -z "${current_email}" ]]; then
    run_as_agent_user \
      git config --global user.email "${ZODEX_GIT_USER_EMAIL}"
  fi
}

configure_agent_build_environment() {
  local cache_root="${ZODEX_DEFAULT_WORKDIR}/.cache/zodex-agent"
  local tmp_root="${ZODEX_DEFAULT_WORKDIR}/.tmp"
  local profile_path="/etc/profile.d/zodex-agent-build-env.sh"

  install -d -m 0700 -o "${ZODEX_AGENT_USER}" -g "${ZODEX_SERVICE_GROUP}" \
    "${tmp_root}" \
    "${cache_root}" \
    "${cache_root}/go-build" \
    "${cache_root}/go-mod" \
    "${cache_root}/npm" \
    "${cache_root}/bun" \
    "${cache_root}/corepack" \
    "${cache_root}/pnpm" \
    "${cache_root}/ccache" \
    "${cache_root}/pip" \
    "${cache_root}/uv"

  cat >"${profile_path}" <<EOF
# Managed by the Zodex runtime installer. Applied only to the agent account.
# Upgrades replace this file; keep site-specific toolchain policy in a separate profile fragment.
if [ "\$(id -un 2>/dev/null)" = "${ZODEX_AGENT_USER}" ]; then
  export TMPDIR="${tmp_root}"
  export GOCACHE="${cache_root}/go-build"
  export GOMODCACHE="${cache_root}/go-mod"
  export npm_config_cache="${cache_root}/npm"
  export npm_config_prefer_offline="true"
  export npm_config_audit="false"
  export npm_config_fund="false"
  export npm_config_progress="false"
  export npm_config_update_notifier="false"
  export npm_config_store_dir="${cache_root}/pnpm"
  export BUN_INSTALL_CACHE_DIR="${cache_root}/bun"
  export COREPACK_HOME="${cache_root}/corepack"
  export CCACHE_DIR="${cache_root}/ccache"
  export CCACHE_MAXSIZE="2G"
  export PIP_CACHE_DIR="${cache_root}/pip"
  export UV_CACHE_DIR="${cache_root}/uv"
  if [ -d /.sprite/bin ]; then
    export PATH="/.sprite/bin:\${PATH}"
  fi
  if [ -d /usr/lib/ccache ]; then
    export PATH="/usr/lib/ccache:\${PATH}"
  fi
fi
EOF
  chmod 0644 "${profile_path}"
}

print_runtime_summary() {
  cat <<EOF

Zodex Sprite runtime installed.

Config file:
  ${ZODEX_CONFIG_PATH}

Installed runtime binaries:
  ${ZODEX_INSTALL_DIR}/zodex-agent
  ${ZODEX_INSTALL_DIR}/git-remote-zodex
  ${ZODEX_INSTALL_DIR}/zodexd
  ${ZODEX_INSTALL_DIR}/zodex-prd

Runtime lifecycle is managed from the operator machine with zodex sprite commands.
EOF
}

run_runtime_install() {
  need_root
  detect_platform
  install_runtime_prerequisites
  ensure_service_accounts

  TMP_DIR="$(mktemp -d)"
  trap cleanup EXIT

  if [[ -n "${ZODEX_BINARY_SOURCE_DIR}" ]]; then
    log "installing binaries from ZODEX_BINARY_SOURCE_DIR=${ZODEX_BINARY_SOURCE_DIR}"
    install_binaries_from_dir "${ZODEX_BINARY_SOURCE_DIR}"
  elif ! install_binaries_from_release; then
    warn "release artifact install failed; attempting source build fallback"
    install_binaries_from_source
  fi

  ensure_dirs_and_config
  configure_agent_git_identity
  configure_agent_git_reader_helper
  configure_agent_build_environment
  print_runtime_summary
}

main() {
  case "$(resolved_install_mode)" in
    operator)
      run_operator_install
      ;;
    runtime)
      run_runtime_install
      ;;
  esac
}

main "$@"
