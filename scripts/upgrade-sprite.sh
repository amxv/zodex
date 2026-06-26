#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

usage() {
  cat <<'EOF'
Usage:
  scripts/upgrade-sprite.sh \
    --sprite <name> \
    [--org <name>] \
    [--version <latest|tag>] \
    [--config <path>] \
    [--repo <owner/repo>] \
    [--url-auth <sprite|public>]

What this script does:
  1. Verifies the Sprite already has a compatible runtime config.
  2. Installs the requested zodex-compatible build inside the Sprite.
  3. Force-recreates Sprite Services from the control plane.
  4. Verifies local health, agent commit identity, reader Git access, socket permissions,
     and publisher-key isolation.
  5. If the Sprite URL is public, verifies external health as well.

Examples:
  scripts/upgrade-sprite.sh --sprite computer --org amxv
  scripts/upgrade-sprite.sh --sprite computer --org amxv --version v0.1.30
EOF
}

log() {
  printf '[upgrade-sprite] %s\n' "$*"
}

die() {
  printf '[upgrade-sprite] ERROR: %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

SPRITE_NAME=""
ORG_NAME=""
VERSION="latest"
CONFIG_PATH="/etc/computer-mcp/config.toml"
TARGET_REPO=""
URL_AUTH=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --sprite)
      SPRITE_NAME="$2"
      shift 2
      ;;
    --org)
      ORG_NAME="$2"
      shift 2
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    --config)
      CONFIG_PATH="$2"
      shift 2
      ;;
    --repo)
      TARGET_REPO="$2"
      shift 2
      ;;
    --url-auth)
      URL_AUTH="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument: $1"
      ;;
  esac
done

[[ -n "${SPRITE_NAME}" ]] || die "--sprite is required"
[[ "${URL_AUTH}" == "" || "${URL_AUTH}" == "sprite" || "${URL_AUTH}" == "public" ]] \
  || die "--url-auth must be sprite or public"

require_cmd sprite
require_cmd curl
require_cmd bash
require_cmd jq

[[ -x "${REPO_ROOT}/scripts/sprite-services.sh" ]] \
  || die "missing executable ${REPO_ROOT}/scripts/sprite-services.sh"

SPRITE_SCOPE_ARGS=("-s" "${SPRITE_NAME}")
if [[ -n "${ORG_NAME}" ]]; then
  SPRITE_SCOPE_ARGS=("-o" "${ORG_NAME}" "-s" "${SPRITE_NAME}")
fi

SPRITE_SERVICE_ARGS=()
if [[ -n "${ORG_NAME}" ]]; then
  SPRITE_SERVICE_ARGS+=(--org "${ORG_NAME}")
fi

installer_ref() {
  if [[ "${VERSION}" == "latest" ]]; then
    printf 'main\n'
  else
    printf '%s\n' "${VERSION}"
  fi
}

run_sprite_exec() {
  sprite "${SPRITE_SCOPE_ARGS[@]}" exec "$@"
}

derive_target_repo_from_remote_config() {
  run_sprite_exec -- sudo awk -F'"' '
    /^\[\[publisher_targets\]\]/ { in_targets=1; next }
    in_targets && /^repo = "/ { print $2; exit }
  ' "${CONFIG_PATH}" 2>/dev/null || true
}

verify_remote_config_exists() {
  log "verifying existing config at ${CONFIG_PATH}"
  run_sprite_exec -- sudo test -f "${CONFIG_PATH}" \
    || die "missing ${CONFIG_PATH} inside the Sprite; use scripts/setup-sprite.sh first"
}

install_requested_version() {
  local tmp_remote_script
  local install_ref
  tmp_remote_script="$(mktemp)"
  trap "/bin/rm -f -- '${tmp_remote_script}'" RETURN
  install_ref="$(installer_ref)"

  cat > "${tmp_remote_script}" <<EOF
#!/usr/bin/env bash
set -euo pipefail

CFG="${CONFIG_PATH}"
VERSION="${VERSION}"
INSTALLER_REF="${install_ref}"

if [[ ! -f "\${CFG}" ]]; then
  echo "[remote] ERROR: missing \${CFG}; use scripts/setup-sprite.sh first" >&2
  exit 1
fi

echo "[remote] install computer-mcp \${VERSION}"
if [[ "\${VERSION}" == "latest" ]]; then
  curl -fsSL "https://raw.githubusercontent.com/amxv/computer-mcp/\${INSTALLER_REF}/scripts/install.sh" | \
    sudo env COMPUTER_MCP_CONFIG_PATH="\${CFG}" bash
else
  curl -fsSL "https://raw.githubusercontent.com/amxv/computer-mcp/\${INSTALLER_REF}/scripts/install.sh" | \
    sudo env \
      COMPUTER_MCP_VERSION="\${VERSION}" \
      COMPUTER_MCP_SOURCE_REF="\${VERSION}" \
      COMPUTER_MCP_CONFIG_PATH="\${CFG}" \
      bash
fi

echo "[remote] installed version"
sudo computer-mcp --version
EOF

  log "installing ${VERSION} inside the Sprite"
  run_sprite_exec --file "${tmp_remote_script}:/tmp/upgrade-computer-mcp-sprite.sh" \
    bash /tmp/upgrade-computer-mcp-sprite.sh
  /bin/rm -f -- "${tmp_remote_script}"
  trap - RETURN
}

sync_sprite_services() {
  log "force-recreating Sprite Services from the control plane"
  if [[ -n "${ORG_NAME}" ]]; then
    "${REPO_ROOT}/scripts/sprite-services.sh" \
      sync \
      --sprite "${SPRITE_NAME}" \
      "${SPRITE_SERVICE_ARGS[@]}" \
      --config "${CONFIG_PATH}" \
      --force-recreate
  else
    "${REPO_ROOT}/scripts/sprite-services.sh" \
      sync \
      --sprite "${SPRITE_NAME}" \
      --config "${CONFIG_PATH}" \
      --force-recreate
  fi
}

verify_local_health() {
  log "verifying local Sprite health via http://127.0.0.1:8080/health"
  run_sprite_exec -- sudo bash -lc \
    "curl -fsS http://127.0.0.1:8080/health | grep -F '\"status\":\"ok\"' >/dev/null"
}

verify_agent_git_identity() {
  log "verifying agent commit identity with a throwaway repo"
  run_sprite_exec -- sudo -u computer-mcp-agent env HOME=/home/computer-mcp-agent bash -lc '
    set -euo pipefail
    smoke_dir=/workspace/.git-identity-upgrade-smoke
    rm -rf "$smoke_dir"
    git init -q "$smoke_dir"
    cd "$smoke_dir"
    printf "sprite git identity smoke\n" > smoke.txt
    git add smoke.txt
    git commit -q -m "Smoke: verify default agent git identity"
    git log -1 --format="%an <%ae>"
    cd /workspace
    rm -rf "$smoke_dir"
  '
}

verify_reader_git_access() {
  local repo="$1"
  if [[ -z "${repo}" ]]; then
    log "warning: no publisher target repo found in ${CONFIG_PATH}; skipping reader Git smoke"
    return
  fi

  log "verifying private GitHub HTTPS access through reader helper for ${repo}"
  run_sprite_exec -- sudo -u computer-mcp-agent env HOME=/home/computer-mcp-agent \
    git ls-remote "https://github.com/${repo}.git" HEAD >/dev/null
}

verify_publisher_socket_permissions() {
  log "verifying publisher socket permissions"
  run_sprite_exec -- sudo bash -lc '
    set -euo pipefail
    dir_path=/var/lib/computer-mcp/publisher/run
    sock_path=/var/lib/computer-mcp/publisher/run/computer-mcp-prd.sock
    [[ "$(stat -c %a "$dir_path")" == "750" ]]
    [[ "$(stat -c %U "$dir_path")" == "computer-mcp-publisher" ]]
    [[ "$(stat -c %G "$dir_path")" == "computer-mcp" ]]
    [[ "$(stat -c %a "$sock_path")" == "660" ]]
    [[ "$(stat -c %U "$sock_path")" == "computer-mcp-publisher" ]]
    [[ "$(stat -c %G "$sock_path")" == "computer-mcp" ]]
  '
}

verify_publisher_key_isolation() {
  log "verifying the agent user still cannot read the publisher private key"
  if run_sprite_exec -- sudo -u computer-mcp-agent env HOME=/home/computer-mcp-agent \
    bash -lc 'cat /etc/computer-mcp/publisher/private-key.pem >/dev/null 2>&1'; then
    die "computer-mcp-agent unexpectedly gained read access to /etc/computer-mcp/publisher/private-key.pem"
  fi
}

verify_service_logs() {
  log "verifying Sprite Service logs are readable"
  if [[ -n "${ORG_NAME}" ]]; then
    "${REPO_ROOT}/scripts/sprite-services.sh" \
      logs \
      --sprite "${SPRITE_NAME}" \
      "${SPRITE_SERVICE_ARGS[@]}" \
      --service computer-mcp-prd \
      --lines 20 >/dev/null
    "${REPO_ROOT}/scripts/sprite-services.sh" \
      logs \
      --sprite "${SPRITE_NAME}" \
      "${SPRITE_SERVICE_ARGS[@]}" \
      --service computer-mcpd \
      --lines 20 >/dev/null
  else
    "${REPO_ROOT}/scripts/sprite-services.sh" \
      logs \
      --sprite "${SPRITE_NAME}" \
      --service computer-mcp-prd \
      --lines 20 >/dev/null
    "${REPO_ROOT}/scripts/sprite-services.sh" \
      logs \
      --sprite "${SPRITE_NAME}" \
      --service computer-mcpd \
      --lines 20 >/dev/null
  fi
}

verify_public_health_if_available() {
  local sprite_url=""
  local sprite_auth=""

  if [[ -n "${URL_AUTH}" ]]; then
    log "setting sprite URL auth: ${URL_AUTH}"
    sprite "${SPRITE_SCOPE_ARGS[@]}" url update --auth "${URL_AUTH}" >/dev/null
  fi

  sprite_url="$(sprite "${SPRITE_SCOPE_ARGS[@]}" url | awk '/^URL:/ {print $2; exit}')"
  sprite_auth="$(sprite "${SPRITE_SCOPE_ARGS[@]}" url | awk '/^Auth:/ {print $2; exit}')"

  if [[ -z "${sprite_url}" ]]; then
    log "warning: could not parse sprite URL; skipping external health verification"
    return
  fi

  if [[ "${sprite_auth}" != "public" ]]; then
    log "sprite URL auth is ${sprite_auth}; skipping external health verification"
    return
  fi

  log "verifying external health via ${sprite_url%/}/health"
  curl -fsS --retry 3 --retry-all-errors --retry-delay 2 "${sprite_url%/}/health" >/dev/null
}

main() {
  local repo="${TARGET_REPO}"

  verify_remote_config_exists
  if [[ -z "${repo}" ]]; then
    repo="$(derive_target_repo_from_remote_config)"
  fi

  install_requested_version
  sync_sprite_services
  verify_service_logs
  verify_local_health
  verify_agent_git_identity
  verify_reader_git_access "${repo}"
  verify_publisher_socket_permissions
  verify_publisher_key_isolation
  verify_public_health_if_available

  log "completed successfully"
}

main
