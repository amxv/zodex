#!/usr/bin/env bash
set -euo pipefail

SCRIPT_VERSION="0.1.15"

COMPUTER_MCP_VERSION="${COMPUTER_MCP_VERSION:-latest}"
COMPUTER_MCP_REPO="${COMPUTER_MCP_REPO:-amxv/computer-mcp}"
COMPUTER_MCP_ASSET_URL="${COMPUTER_MCP_ASSET_URL:-}"
COMPUTER_MCP_SOURCE_REF="${COMPUTER_MCP_SOURCE_REF:-main}"
COMPUTER_MCP_BINARY_SOURCE_DIR="${COMPUTER_MCP_BINARY_SOURCE_DIR:-}"
COMPUTER_MCP_INSTALL_DIR="${COMPUTER_MCP_INSTALL_DIR:-/usr/local/bin}"
COMPUTER_MCP_CONFIG_PATH="${COMPUTER_MCP_CONFIG_PATH:-/etc/computer-mcp/config.toml}"
COMPUTER_MCP_STATE_DIR="${COMPUTER_MCP_STATE_DIR:-/var/lib/computer-mcp}"
COMPUTER_MCP_TLS_DIR="${COMPUTER_MCP_TLS_DIR:-${COMPUTER_MCP_STATE_DIR}/tls}"
COMPUTER_MCP_AGENT_USER="${COMPUTER_MCP_AGENT_USER:-computer-mcp-agent}"
COMPUTER_MCP_AGENT_HOME="${COMPUTER_MCP_AGENT_HOME:-/home/${COMPUTER_MCP_AGENT_USER}}"
COMPUTER_MCP_AGENT_SHELL="${COMPUTER_MCP_AGENT_SHELL:-/bin/bash}"
COMPUTER_MCP_DEFAULT_WORKDIR="${COMPUTER_MCP_DEFAULT_WORKDIR:-/workspace}"
COMPUTER_MCP_PUBLISHER_USER="${COMPUTER_MCP_PUBLISHER_USER:-computer-mcp-publisher}"
COMPUTER_MCP_PUBLISHER_HOME="${COMPUTER_MCP_PUBLISHER_HOME:-/nonexistent}"
COMPUTER_MCP_SERVICE_GROUP="${COMPUTER_MCP_SERVICE_GROUP:-computer-mcp}"
COMPUTER_MCP_GIT_USER_NAME_WAS_SET=0
if [[ "${COMPUTER_MCP_GIT_USER_NAME+x}" == "x" ]]; then
  COMPUTER_MCP_GIT_USER_NAME_WAS_SET=1
fi
COMPUTER_MCP_GIT_USER_EMAIL_WAS_SET=0
if [[ "${COMPUTER_MCP_GIT_USER_EMAIL+x}" == "x" ]]; then
  COMPUTER_MCP_GIT_USER_EMAIL_WAS_SET=1
fi
COMPUTER_MCP_GIT_USER_NAME="${COMPUTER_MCP_GIT_USER_NAME:-Computer MCP Agent}"
COMPUTER_MCP_GIT_USER_EMAIL="${COMPUTER_MCP_GIT_USER_EMAIL:-computer-mcp-agent@local.invalid}"
COMPUTER_MCP_READER_KEY_DIR="${COMPUTER_MCP_READER_KEY_DIR:-/etc/computer-mcp/reader}"
COMPUTER_MCP_PUBLISHER_KEY_DIR="${COMPUTER_MCP_PUBLISHER_KEY_DIR:-/etc/computer-mcp/publisher}"
COMPUTER_MCP_HTTP_BIND_PORT="${COMPUTER_MCP_HTTP_BIND_PORT:-}"
COMPUTER_MCP_PUBLIC_HOST="${COMPUTER_MCP_PUBLIC_HOST:-}"
COMPUTER_MCP_ENABLE_CERTBOT="${COMPUTER_MCP_ENABLE_CERTBOT:-0}"

DISTRO_ID="unknown"
DISTRO_LIKE=""
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
    rm -rf "${TMP_DIR}"
  fi
}

need_root() {
  if [[ "${EUID}" -ne 0 ]]; then
    die "run as root (for example: curl ... | sudo bash)"
  fi
}

command_exists() {
  command -v "$1" >/dev/null 2>&1
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
  if [[ -x "${COMPUTER_MCP_AGENT_SHELL}" ]]; then
    printf '%s\n' "${COMPUTER_MCP_AGENT_SHELL}"
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

  if ! getent group "${COMPUTER_MCP_SERVICE_GROUP}" >/dev/null; then
    groupadd --system "${COMPUTER_MCP_SERVICE_GROUP}"
  fi

  if ! id -u "${COMPUTER_MCP_AGENT_USER}" >/dev/null 2>&1; then
    useradd \
      --system \
      --gid "${COMPUTER_MCP_SERVICE_GROUP}" \
      --home-dir "${COMPUTER_MCP_AGENT_HOME}" \
      --create-home \
      --shell "${login_shell}" \
      "${COMPUTER_MCP_AGENT_USER}"
  else
    usermod --home "${COMPUTER_MCP_AGENT_HOME}" "${COMPUTER_MCP_AGENT_USER}" || true
    usermod --shell "${login_shell}" "${COMPUTER_MCP_AGENT_USER}" || true
  fi

  if ! id -u "${COMPUTER_MCP_PUBLISHER_USER}" >/dev/null 2>&1; then
    useradd \
      --system \
      --gid "${COMPUTER_MCP_SERVICE_GROUP}" \
      --home-dir "${COMPUTER_MCP_PUBLISHER_HOME}" \
      --no-create-home \
      --shell "${nologin}" \
      "${COMPUTER_MCP_PUBLISHER_USER}"
  else
    usermod --home "${COMPUTER_MCP_PUBLISHER_HOME}" "${COMPUTER_MCP_PUBLISHER_USER}" || true
    usermod --shell "${nologin}" "${COMPUTER_MCP_PUBLISHER_USER}" || true
  fi
}

detect_platform() {
  [[ "$(uname -s)" == "Linux" ]] || die "Linux only"
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

is_runpod() {
  [[ -n "${RUNPOD_POD_ID:-}" || -n "${RUNPOD_PUBLIC_IP:-}" ]]
}

runpod_proxy_host() {
  local pod_id="${RUNPOD_POD_ID:-<pod-id>}"
  printf '%s-8080.proxy.runpod.net\n' "${pod_id}"
}

resolved_http_bind_port() {
  if [[ -n "${COMPUTER_MCP_HTTP_BIND_PORT}" ]]; then
    printf '%s\n' "${COMPUTER_MCP_HTTP_BIND_PORT}"
    return
  fi

  if is_runpod; then
    printf '8080\n'
  fi
}

should_use_http_proxy_path() {
  [[ -n "$(resolved_http_bind_port)" ]]
}

resolved_public_host() {
  if [[ -n "${COMPUTER_MCP_PUBLIC_HOST}" ]]; then
    printf '%s\n' "${COMPUTER_MCP_PUBLIC_HOST}"
    return
  fi

  if is_runpod; then
    runpod_proxy_host
    return
  fi

  detect_public_ip
}

install_runtime_prerequisites() {
  if command_exists apt-get; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -y
    apt-get install -y --no-install-recommends \
      curl ca-certificates systemd tar gzip git

    if [[ "${COMPUTER_MCP_ENABLE_CERTBOT}" == "1" ]]; then
      apt-get install -y --no-install-recommends certbot || warn "certbot install failed"
    fi
    return
  fi

  if command_exists dnf; then
    dnf install -y curl ca-certificates systemd tar gzip git
    if [[ "${COMPUTER_MCP_ENABLE_CERTBOT}" == "1" ]]; then
      dnf install -y certbot || warn "certbot install failed"
    fi
    return
  fi

  if command_exists yum; then
    yum install -y curl ca-certificates systemd tar gzip git
    if [[ "${COMPUTER_MCP_ENABLE_CERTBOT}" == "1" ]]; then
      yum install -y certbot || warn "certbot install failed"
    fi
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

resolve_release_api_url() {
  if [[ "${COMPUTER_MCP_VERSION}" == "latest" ]]; then
    printf 'https://api.github.com/repos/%s/releases/latest\n' "${COMPUTER_MCP_REPO}"
  else
    printf 'https://api.github.com/repos/%s/releases/tags/%s\n' \
      "${COMPUTER_MCP_REPO}" "${COMPUTER_MCP_VERSION}"
  fi
}

resolve_release_asset_url_by_name() {
  local metadata="$1"
  local archive_name="$2"
  printf '%s' "${metadata}" \
    | tr '\n' ' ' \
    | sed 's/},{/},\n{/g' \
    | grep -Eo "\"browser_download_url\"[[:space:]]*:[[:space:]]*\"[^\"]*/${archive_name}\"" \
    | head -n1 \
      | sed -E 's/"browser_download_url"[[:space:]]*:[[:space:]]*"([^"]+)"/\1/'
}

resolve_release_asset_url_legacy_pattern() {
  local metadata="$1"
  local server_archive_name="$2"
  printf '%s' "${metadata}" \
    | tr '\n' ' ' \
    | sed 's/},{/},\n{/g' \
    | grep -Eo "\"browser_download_url\"[[:space:]]*:[[:space:]]*\"[^\"]*/${server_archive_name}\"" \
    | head -n1 \
    | sed -E 's/"browser_download_url"[[:space:]]*:[[:space:]]*"([^"]+)"/\1/'
}

resolve_release_asset_url() {
  if [[ -n "${COMPUTER_MCP_ASSET_URL}" ]]; then
    printf '%s\n' "${COMPUTER_MCP_ASSET_URL}"
    return
  fi

  local metadata
  metadata="$(curl -fsSL "$(resolve_release_api_url)")" || return 1

  local server_archive_name="computer-mcp-${TARGET_TRIPLE}.tar.gz"
  local primary_server_archive_name="zodex-${TARGET_TRIPLE}.tar.gz"
  local asset_url=""
  asset_url="$(resolve_release_asset_url_by_name "${metadata}" "${primary_server_archive_name}")"
  if [[ -z "${asset_url}" ]]; then
    asset_url="$(resolve_release_asset_url_legacy_pattern "${metadata}" "${server_archive_name}")"
  fi

  [[ -n "${asset_url}" ]] || return 1
  printf '%s\n' "${asset_url}"
}

install_binaries_from_dir() {
  local src_dir="$1"
  local cli_src="${src_dir}/zodex"
  local daemon_src="${src_dir}/zodexd"
  if [[ ! -x "${cli_src}" && -x "${src_dir}/computer-mcp" ]]; then
    cli_src="${src_dir}/computer-mcp"
  fi
  if [[ ! -x "${daemon_src}" && -x "${src_dir}/computer-mcpd" ]]; then
    daemon_src="${src_dir}/computer-mcpd"
  fi

  [[ -x "${cli_src}" ]] || die "missing executable ${src_dir}/zodex or ${src_dir}/computer-mcp"
  [[ -x "${daemon_src}" ]] || die "missing executable ${src_dir}/zodexd or ${src_dir}/computer-mcpd"
  [[ -x "${src_dir}/computer-mcp-prd" ]] || die "missing executable ${src_dir}/computer-mcp-prd"

  install -d -m 0755 "${COMPUTER_MCP_INSTALL_DIR}"
  install -m 0755 "${cli_src}" "${COMPUTER_MCP_INSTALL_DIR}/zodex"
  install -m 0755 "${daemon_src}" "${COMPUTER_MCP_INSTALL_DIR}/zodexd"
  install -m 0755 "${src_dir}/computer-mcp-prd" "${COMPUTER_MCP_INSTALL_DIR}/computer-mcp-prd"
  ln -sf "${COMPUTER_MCP_INSTALL_DIR}/zodex" "${COMPUTER_MCP_INSTALL_DIR}/computer-mcp"
  ln -sf "${COMPUTER_MCP_INSTALL_DIR}/zodexd" "${COMPUTER_MCP_INSTALL_DIR}/computer-mcpd"
}

install_binaries_from_release() {
  local asset_url
  asset_url="$(resolve_release_asset_url)" || return 1
  log "downloading release artifact: ${asset_url}"

  local archive="${TMP_DIR}/release.tar.gz"
  curl -fL "${asset_url}" -o "${archive}"
  tar -xzf "${archive}" -C "${TMP_DIR}"

  local cli_path
  cli_path="$(find "${TMP_DIR}" -type f \( -name zodex -o -name computer-mcp \) -print -quit)"
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
  log "falling back to source build from ${COMPUTER_MCP_REPO}@${COMPUTER_MCP_SOURCE_REF}"
  install_build_prerequisites
  install_rust_toolchain_if_needed

  local src_dir="${TMP_DIR}/source"
  git clone --depth 1 --branch "${COMPUTER_MCP_SOURCE_REF}" \
    "https://github.com/${COMPUTER_MCP_REPO}.git" "${src_dir}"

  (
    cd "${src_dir}"
    if cargo build --release --bin zodex --bin zodexd --bin computer-mcp-prd; then
      :
    else
      cargo build --release --bin computer-mcp --bin computer-mcpd --bin computer-mcp-prd
    fi
  )

  install_binaries_from_dir "${src_dir}/target/release"
}

ensure_dirs_and_config() {
  local config_dir
  config_dir="$(dirname "${COMPUTER_MCP_CONFIG_PATH}")"

  install -d -m 0750 -o root -g "${COMPUTER_MCP_SERVICE_GROUP}" "${config_dir}"
  install -d -m 0750 -o root -g "${COMPUTER_MCP_SERVICE_GROUP}" "${COMPUTER_MCP_STATE_DIR}"
  install -d -m 0750 -o "${COMPUTER_MCP_PUBLISHER_USER}" -g "${COMPUTER_MCP_SERVICE_GROUP}" "${COMPUTER_MCP_STATE_DIR}/publisher"
  install -d -m 0750 -o "${COMPUTER_MCP_PUBLISHER_USER}" -g "${COMPUTER_MCP_SERVICE_GROUP}" "${COMPUTER_MCP_STATE_DIR}/publisher/run"
  install -d -m 0750 -o "${COMPUTER_MCP_PUBLISHER_USER}" -g "${COMPUTER_MCP_SERVICE_GROUP}" "${COMPUTER_MCP_STATE_DIR}/publisher/logs"
  install -d -m 0750 -o root -g "${COMPUTER_MCP_SERVICE_GROUP}" "${COMPUTER_MCP_TLS_DIR}"
  install -d -m 0750 -o root -g "${COMPUTER_MCP_SERVICE_GROUP}" "${COMPUTER_MCP_READER_KEY_DIR}"
  install -d -m 0750 -o root -g "${COMPUTER_MCP_SERVICE_GROUP}" "${COMPUTER_MCP_PUBLISHER_KEY_DIR}"
  install -d -m 0750 -o "${COMPUTER_MCP_AGENT_USER}" -g "${COMPUTER_MCP_SERVICE_GROUP}" "${COMPUTER_MCP_AGENT_HOME}"
  install -d -m 0750 -o "${COMPUTER_MCP_AGENT_USER}" -g "${COMPUTER_MCP_SERVICE_GROUP}" "${COMPUTER_MCP_DEFAULT_WORKDIR}"

  if [[ ! -f "${COMPUTER_MCP_CONFIG_PATH}" ]]; then
    local api_key
    local runpod_http_port_line=""
    if command_exists openssl; then
      api_key="$(openssl rand -hex 24)"
    else
      api_key="$(tr -dc 'A-Za-z0-9' </dev/urandom | head -c 48)"
    fi

    if [[ -n "$(resolved_http_bind_port)" ]]; then
      runpod_http_port_line="http_bind_port = $(resolved_http_bind_port)"
    fi

    umask 077
    cat >"${COMPUTER_MCP_CONFIG_PATH}" <<EOF
api_key = "${api_key}"
${runpod_http_port_line}
agent_user = "${COMPUTER_MCP_AGENT_USER}"
agent_home = "${COMPUTER_MCP_AGENT_HOME}"
default_workdir = "${COMPUTER_MCP_DEFAULT_WORKDIR}"
publisher_user = "${COMPUTER_MCP_PUBLISHER_USER}"

# Most installs can keep the built-in defaults.
# Add only the settings you actually need to override.

# Required GitHub App settings:
# reader_app_id = 123456
# reader_installation_id = 234567890
# publisher_app_id = 345678
#
# [[publisher_targets]]
# id = "owner/repo"
# repo = "owner/repo"
# default_base = "main"
# installation_id = 456789012
EOF
    log "created config at ${COMPUTER_MCP_CONFIG_PATH}"
  fi

  chgrp "${COMPUTER_MCP_SERVICE_GROUP}" "${COMPUTER_MCP_CONFIG_PATH}"
  chmod 0640 "${COMPUTER_MCP_CONFIG_PATH}"
}

run_cli_install() {
  local cli="${COMPUTER_MCP_INSTALL_DIR}/zodex"
  [[ -x "${cli}" ]] || die "zodex not installed at ${cli}"
  "${cli}" --config "${COMPUTER_MCP_CONFIG_PATH}" install
}

run_as_agent_user() {
  if command_exists runuser; then
    runuser -u "${COMPUTER_MCP_AGENT_USER}" -- env HOME="${COMPUTER_MCP_AGENT_HOME}" "$@"
    return
  fi

  if command_exists sudo; then
    sudo -u "${COMPUTER_MCP_AGENT_USER}" env HOME="${COMPUTER_MCP_AGENT_HOME}" "$@"
    return
  fi

  local command_string=""
  local arg
  for arg in "$@"; do
    command_string+=" $(printf '%q' "${arg}")"
  done

  su -s /bin/sh "${COMPUTER_MCP_AGENT_USER}" -c \
    "HOME=$(printf '%q' "${COMPUTER_MCP_AGENT_HOME}")${command_string}"
}

configure_agent_git_reader_helper() {
  local helper_cmd="${COMPUTER_MCP_INSTALL_DIR}/zodex --config ${COMPUTER_MCP_CONFIG_PATH} git-credential-helper"

  run_as_agent_user \
    git config --global --replace-all credential.https://github.com.helper "${helper_cmd}"
  run_as_agent_user \
    git config --global credential.https://github.com.useHttpPath false
}

configure_agent_git_identity() {
  local current_name=""
  local current_email=""

  current_name="$(run_as_agent_user git config --global --get user.name || true)"
  current_email="$(run_as_agent_user git config --global --get user.email || true)"

  if [[ "${COMPUTER_MCP_GIT_USER_NAME_WAS_SET}" == "1" || -z "${current_name}" ]]; then
    run_as_agent_user \
      git config --global user.name "${COMPUTER_MCP_GIT_USER_NAME}"
  fi
  if [[ "${COMPUTER_MCP_GIT_USER_EMAIL_WAS_SET}" == "1" || -z "${current_email}" ]]; then
    run_as_agent_user \
      git config --global user.email "${COMPUTER_MCP_GIT_USER_EMAIL}"
  fi
}

detect_public_ip() {
  local ip=""
  ip="$(curl -fsS --max-time 5 https://api.ipify.org || true)"
  if [[ -z "${ip}" ]]; then
    ip="<public_ip>"
  fi
  printf '%s\n' "${ip}"
}

print_next_steps() {
  local public_host
  public_host="$(resolved_public_host)"

  if should_use_http_proxy_path; then
    local http_port
    http_port="$(resolved_http_bind_port)"
    cat <<EOF

Install complete.

Config file:
  ${COMPUTER_MCP_CONFIG_PATH}

The commands below assume the default config path. If you changed it, add:
  --config "${COMPUTER_MCP_CONFIG_PATH}"

Next steps:
  1. expose HTTP port ${http_port} on your container platform
  2. review "${COMPUTER_MCP_CONFIG_PATH}" and add reader_app_id / reader_installation_id / publisher_app_id / publisher_targets
  3. place the reader GitHub App key at "${COMPUTER_MCP_READER_KEY_DIR}/private-key.pem"
  4. place the publisher GitHub App key at "${COMPUTER_MCP_PUBLISHER_KEY_DIR}/private-key.pem" with owner ${COMPUTER_MCP_PUBLISHER_USER}
  5. zodex start
  6. zodex show-url --host "${public_host}"

Verify:
  - zodex status
  - curl "https://${public_host}/health"
  - MCP URL shape: https://${public_host}/mcp?key=<redacted>

Optional:
  - rotate the installer-generated API key with: zodex set-key "<strong-random-key>"
  - private GitHub HTTPS clones by ${COMPUTER_MCP_AGENT_USER} will use the built-in reader credential helper once reader_app_id, reader_installation_id, and the reader PEM are in place
  - agent commits default to ${COMPUTER_MCP_GIT_USER_NAME} <${COMPUTER_MCP_GIT_USER_EMAIL}> unless you override COMPUTER_MCP_GIT_USER_NAME / COMPUTER_MCP_GIT_USER_EMAIL during install
EOF
    return
  fi

  cat <<EOF

Install complete.

Config file:
  ${COMPUTER_MCP_CONFIG_PATH}

The commands below assume the default config path. If you changed it, add:
  --config "${COMPUTER_MCP_CONFIG_PATH}"

Next steps:
  1. review "${COMPUTER_MCP_CONFIG_PATH}" and add reader_app_id / reader_installation_id / publisher_app_id / publisher_targets
  2. place the reader GitHub App key at "${COMPUTER_MCP_READER_KEY_DIR}/private-key.pem"
  3. place the publisher GitHub App key at "${COMPUTER_MCP_PUBLISHER_KEY_DIR}/private-key.pem" with owner ${COMPUTER_MCP_PUBLISHER_USER}
  4. zodex start
  5. zodex show-url --host "${public_host}"

Verify:
  - zodex status
  - curl -k "https://${public_host}/health"
  - MCP URL shape: https://${public_host}/mcp?key=<redacted>

Optional:
  - rotate the installer-generated API key with: zodex set-key "<strong-random-key>"
  - private GitHub HTTPS clones by ${COMPUTER_MCP_AGENT_USER} will use the built-in reader credential helper once reader_app_id, reader_installation_id, and the reader PEM are in place
  - agent commits default to ${COMPUTER_MCP_GIT_USER_NAME} <${COMPUTER_MCP_GIT_USER_EMAIL}> unless you override COMPUTER_MCP_GIT_USER_NAME / COMPUTER_MCP_GIT_USER_EMAIL during install
EOF
}

main() {
  need_root
  detect_platform
  install_runtime_prerequisites
  ensure_service_accounts

  TMP_DIR="$(mktemp -d)"
  trap cleanup EXIT

  if [[ -n "${COMPUTER_MCP_BINARY_SOURCE_DIR}" ]]; then
    log "installing binaries from COMPUTER_MCP_BINARY_SOURCE_DIR=${COMPUTER_MCP_BINARY_SOURCE_DIR}"
    install_binaries_from_dir "${COMPUTER_MCP_BINARY_SOURCE_DIR}"
  elif ! install_binaries_from_release; then
    warn "release artifact install failed; attempting source build fallback"
    install_binaries_from_source
  fi

  ensure_dirs_and_config
  run_cli_install
  configure_agent_git_identity
  configure_agent_git_reader_helper
  print_next_steps
}

main "$@"
