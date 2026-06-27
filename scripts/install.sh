#!/usr/bin/env bash
set -euo pipefail

SCRIPT_VERSION="0.1.15"

ZODEX_VERSION="${ZODEX_VERSION:-latest}"
ZODEX_REPO="${ZODEX_REPO:-amxv/zodex}"
ZODEX_ASSET_URL="${ZODEX_ASSET_URL:-}"
ZODEX_SOURCE_REF="${ZODEX_SOURCE_REF:-main}"
ZODEX_BINARY_SOURCE_DIR="${ZODEX_BINARY_SOURCE_DIR:-}"
ZODEX_INSTALL_DIR="${ZODEX_INSTALL_DIR:-/usr/local/bin}"
ZODEX_CONFIG_PATH="${ZODEX_CONFIG_PATH:-/etc/zodex/config.toml}"
ZODEX_STATE_DIR="${ZODEX_STATE_DIR:-/var/lib/zodex}"
ZODEX_TLS_DIR="${ZODEX_TLS_DIR:-${ZODEX_STATE_DIR}/tls}"
ZODEX_AGENT_USER="${ZODEX_AGENT_USER:-zodex-agent}"
ZODEX_AGENT_HOME="${ZODEX_AGENT_HOME:-/home/${ZODEX_AGENT_USER}}"
ZODEX_AGENT_SHELL="${ZODEX_AGENT_SHELL:-/bin/bash}"
ZODEX_DEFAULT_WORKDIR="${ZODEX_DEFAULT_WORKDIR:-/workspace}"
ZODEX_PUBLISHER_USER="${ZODEX_PUBLISHER_USER:-zodex-publisher}"
ZODEX_PUBLISHER_HOME="${ZODEX_PUBLISHER_HOME:-/nonexistent}"
ZODEX_SERVICE_GROUP="${ZODEX_SERVICE_GROUP:-zodex}"
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
ZODEX_HTTP_BIND_PORT="${ZODEX_HTTP_BIND_PORT:-}"
ZODEX_PUBLIC_HOST="${ZODEX_PUBLIC_HOST:-}"
ZODEX_ENABLE_CERTBOT="${ZODEX_ENABLE_CERTBOT:-0}"

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

resolved_http_bind_port() {
  if [[ -n "${ZODEX_HTTP_BIND_PORT}" ]]; then
    printf '%s\n' "${ZODEX_HTTP_BIND_PORT}"
  fi
}

should_use_http_proxy_path() {
  [[ -n "$(resolved_http_bind_port)" ]]
}

resolved_public_host() {
  if [[ -n "${ZODEX_PUBLIC_HOST}" ]]; then
    printf '%s\n' "${ZODEX_PUBLIC_HOST}"
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

    if [[ "${ZODEX_ENABLE_CERTBOT}" == "1" ]]; then
      apt-get install -y --no-install-recommends certbot || warn "certbot install failed"
    fi
    return
  fi

  if command_exists dnf; then
    dnf install -y curl ca-certificates systemd tar gzip git
    if [[ "${ZODEX_ENABLE_CERTBOT}" == "1" ]]; then
      dnf install -y certbot || warn "certbot install failed"
    fi
    return
  fi

  if command_exists yum; then
    yum install -y curl ca-certificates systemd tar gzip git
    if [[ "${ZODEX_ENABLE_CERTBOT}" == "1" ]]; then
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
  if [[ "${ZODEX_VERSION}" == "latest" ]]; then
    printf 'https://api.github.com/repos/%s/releases/latest\n' "${ZODEX_REPO}"
  else
    printf 'https://api.github.com/repos/%s/releases/tags/%s\n' \
      "${ZODEX_REPO}" "${ZODEX_VERSION}"
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

resolve_release_asset_url() {
  if [[ -n "${ZODEX_ASSET_URL}" ]]; then
    printf '%s\n' "${ZODEX_ASSET_URL}"
    return
  fi

  local metadata
  metadata="$(curl -fsSL "$(resolve_release_api_url)")" || return 1

  local server_archive_name="zodex-${TARGET_TRIPLE}.tar.gz"
  local asset_url=""
  asset_url="$(resolve_release_asset_url_by_name "${metadata}" "${server_archive_name}")"

  [[ -n "${asset_url}" ]] || return 1
  printf '%s\n' "${asset_url}"
}

install_binaries_from_dir() {
  local src_dir="$1"
  local cli_src="${src_dir}/zodex"
  local daemon_src="${src_dir}/zodexd"
  if [[ ! -x "${cli_src}" && -x "${src_dir}/zodex" ]]; then
    cli_src="${src_dir}/zodex"
  fi
  if [[ ! -x "${daemon_src}" && -x "${src_dir}/zodexd" ]]; then
    daemon_src="${src_dir}/zodexd"
  fi

  [[ -x "${cli_src}" ]] || die "missing executable ${src_dir}/zodex or ${src_dir}/zodex"
  [[ -x "${src_dir}/zodex-agent" ]] || die "missing executable ${src_dir}/zodex-agent"
  [[ -x "${daemon_src}" ]] || die "missing executable ${src_dir}/zodexd or ${src_dir}/zodexd"
  [[ -x "${src_dir}/zodex-prd" ]] || die "missing executable ${src_dir}/zodex-prd"

  install -d -m 0755 "${ZODEX_INSTALL_DIR}"
  install -m 0755 "${cli_src}" "${ZODEX_INSTALL_DIR}/zodex"
  install -m 0755 "${src_dir}/zodex-agent" "${ZODEX_INSTALL_DIR}/zodex-agent"
  install -m 0755 "${daemon_src}" "${ZODEX_INSTALL_DIR}/zodexd"
  install -m 0755 "${src_dir}/zodex-prd" "${ZODEX_INSTALL_DIR}/zodex-prd"
}

install_binaries_from_release() {
  local asset_url
  asset_url="$(resolve_release_asset_url)" || return 1
  log "downloading release artifact: ${asset_url}"

  local archive="${TMP_DIR}/release.tar.gz"
  curl -fL "${asset_url}" -o "${archive}"
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
    if cargo build --release --bin zodex --bin zodex-agent --bin zodexd --bin zodex-prd; then
      :
    else
      cargo build --release --bin zodex --bin zodex-agent --bin zodexd --bin zodex-prd
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
  install -d -m 0750 -o root -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_TLS_DIR}"
  install -d -m 0750 -o root -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_READER_KEY_DIR}"
  install -d -m 0750 -o root -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_PUBLISHER_KEY_DIR}"
  install -d -m 0750 -o "${ZODEX_AGENT_USER}" -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_AGENT_HOME}"
  install -d -m 0750 -o "${ZODEX_AGENT_USER}" -g "${ZODEX_SERVICE_GROUP}" "${ZODEX_DEFAULT_WORKDIR}"

  if [[ ! -f "${ZODEX_CONFIG_PATH}" ]]; then
    local api_key
    local bind_port_line=""
    local http_bind_port_line=""
    if command_exists openssl; then
      api_key="$(openssl rand -hex 24)"
    else
      api_key="$(tr -dc 'A-Za-z0-9' </dev/urandom | head -c 48)"
    fi

    if [[ -n "$(resolved_http_bind_port)" ]]; then
      bind_port_line="bind_port = 8443"
      http_bind_port_line="http_bind_port = $(resolved_http_bind_port)"
    fi

    umask 077
    cat >"${ZODEX_CONFIG_PATH}" <<EOF
api_key = "${api_key}"
${bind_port_line}
${http_bind_port_line}
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

  chgrp "${ZODEX_SERVICE_GROUP}" "${ZODEX_CONFIG_PATH}"
  chmod 0640 "${ZODEX_CONFIG_PATH}"
}

run_cli_install() {
  local cli="${ZODEX_INSTALL_DIR}/zodex"
  [[ -x "${cli}" ]] || die "zodex not installed at ${cli}"
  "${cli}" --config "${ZODEX_CONFIG_PATH}" install
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
  ${ZODEX_CONFIG_PATH}

The commands below assume the default config path. If you changed it, add:
  --config "${ZODEX_CONFIG_PATH}"

Next steps:
  1. expose HTTP port ${http_port} on your container platform
  2. review "${ZODEX_CONFIG_PATH}" and add reader_app_id / reader_installation_id / publisher_client_id
  3. enable Device Flow on the push-grant GitHub App
  4. place the reader GitHub App key at "${ZODEX_READER_KEY_DIR}/private-key.pem"
  5. if you want the internal publish daemon, also add publisher_app_id / publisher_targets and place the publisher GitHub App key at "${ZODEX_PUBLISHER_KEY_DIR}/private-key.pem" with owner ${ZODEX_PUBLISHER_USER}
  6. zodex start
  7. zodex-agent show-url --host "${public_host}"

Verify:
  - zodex status
  - curl "https://${public_host}/health"
  - MCP URL shape: https://${public_host}/mcp?key=<redacted>

Optional:
  - rotate the installer-generated API key with: zodex set-key "<strong-random-key>"
  - private GitHub HTTPS clones by ${ZODEX_AGENT_USER} will use the built-in reader credential helper once reader_app_id, reader_installation_id, and the reader PEM are in place
  - agent-facing GitHub auth is restricted to zodex-agent: request push with `zodex-agent github request-push --repo <owner/repo>` and revoke with `zodex-agent github revoke-push --repo <owner/repo>`
  - agent commits default to ${ZODEX_GIT_USER_NAME} <${ZODEX_GIT_USER_EMAIL}> unless you override ZODEX_GIT_USER_NAME / ZODEX_GIT_USER_EMAIL during install
EOF
    return
  fi

  cat <<EOF

Install complete.

Config file:
  ${ZODEX_CONFIG_PATH}

The commands below assume the default config path. If you changed it, add:
  --config "${ZODEX_CONFIG_PATH}"

Next steps:
  1. review "${ZODEX_CONFIG_PATH}" and add reader_app_id / reader_installation_id / publisher_client_id
  2. enable Device Flow on the push-grant GitHub App
  3. place the reader GitHub App key at "${ZODEX_READER_KEY_DIR}/private-key.pem"
  4. if you want the internal publish daemon, also add publisher_app_id / publisher_targets and place the publisher GitHub App key at "${ZODEX_PUBLISHER_KEY_DIR}/private-key.pem" with owner ${ZODEX_PUBLISHER_USER}
  5. zodex start
  6. zodex-agent show-url --host "${public_host}"

Verify:
  - zodex status
  - curl -k "https://${public_host}/health"
  - MCP URL shape: https://${public_host}/mcp?key=<redacted>

Optional:
  - rotate the installer-generated API key with: zodex set-key "<strong-random-key>"
  - private GitHub HTTPS clones by ${ZODEX_AGENT_USER} will use the built-in reader credential helper once reader_app_id, reader_installation_id, and the reader PEM are in place
  - agent-facing GitHub auth is restricted to zodex-agent: request push with `zodex-agent github request-push --repo <owner/repo>` and revoke with `zodex-agent github revoke-push --repo <owner/repo>`
  - agent commits default to ${ZODEX_GIT_USER_NAME} <${ZODEX_GIT_USER_EMAIL}> unless you override ZODEX_GIT_USER_NAME / ZODEX_GIT_USER_EMAIL during install
EOF
}

main() {
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
  run_cli_install
  configure_agent_git_identity
  configure_agent_git_reader_helper
  print_next_steps
}

main "$@"
