#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

usage() {
  cat <<'EOF'
Usage:
  scripts/setup-sprite.sh \
    --sprite <name> \
    --repo <owner/repo> \
    --reader-app-id <id> \
    --reader-pem <abs-path> \
    --publisher-app-id <id> \
    --publisher-pem <abs-path> \
    [--org <name>] \
    [--default-base <branch>] \
    [--url-auth <sprite|public>]

What this script does:
  1. Derives GitHub App installation IDs for the target repo.
  2. Validates reader/publisher app token minting locally.
  3. Installs the latest zodex-compatible runtime on the target Sprite.
  4. Installs PEM keys + writes managed config block.
  5. Adapts ports for Sprite service-mode runtime:
     - bind_port = 8443 (TLS)
     - http_bind_port = 8080 (Sprite URL routing)
  6. Verifies the agent can commit with the installer-provided Git identity.
  7. Verifies reader-backed GitHub HTTPS access for the agent user.
  8. Registers Sprite Services and validates Sprite-native lifecycle.
EOF
}

log() {
  printf '[setup-sprite] %s\n' "$*"
}

die() {
  printf '[setup-sprite] ERROR: %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

b64url() {
  openssl base64 -A | tr '+/' '-_' | tr -d '='
}

derive_installation_id() {
  local app_id="$1"
  local key_path="$2"
  local repo="$3"

  local now iat exp header payload unsigned sig jwt
  now="$(date +%s)"
  iat="$((now - 60))"
  exp="$((now + 540))"
  header='{"alg":"RS256","typ":"JWT"}'
  payload="$(printf '{"iat":%s,"exp":%s,"iss":"%s"}' "$iat" "$exp" "$app_id")"
  unsigned="$(printf '%s' "$header" | b64url).$(printf '%s' "$payload" | b64url)"
  sig="$(printf '%s' "$unsigned" | openssl dgst -binary -sha256 -sign "$key_path" | b64url)"
  jwt="${unsigned}.${sig}"

  curl -fsSL \
    -H "Accept: application/vnd.github+json" \
    -H "Authorization: Bearer ${jwt}" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    "https://api.github.com/repos/${repo}/installation" | jq -r '.id'
}

validate_local_prereqs() {
  require_cmd sprite
  require_cmd curl
  require_cmd jq
  require_cmd openssl
  require_cmd bash

  [[ -x "${REPO_ROOT}/scripts/mint-gh-app-installation-token.sh" ]] \
    || die "missing executable ${REPO_ROOT}/scripts/mint-gh-app-installation-token.sh"
  [[ -x "${REPO_ROOT}/scripts/sprite-services.sh" ]] \
    || die "missing executable ${REPO_ROOT}/scripts/sprite-services.sh"
}

SPRITE_NAME=""
ORG_NAME=""
TARGET_REPO=""
READER_APP_ID=""
READER_PEM=""
PUBLISHER_APP_ID=""
PUBLISHER_PEM=""
DEFAULT_BASE="main"
URL_AUTH="sprite"

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
    --repo)
      TARGET_REPO="$2"
      shift 2
      ;;
    --reader-app-id)
      READER_APP_ID="$2"
      shift 2
      ;;
    --reader-pem)
      READER_PEM="$2"
      shift 2
      ;;
    --publisher-app-id)
      PUBLISHER_APP_ID="$2"
      shift 2
      ;;
    --publisher-pem)
      PUBLISHER_PEM="$2"
      shift 2
      ;;
    --default-base)
      DEFAULT_BASE="$2"
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
[[ -n "${TARGET_REPO}" ]] || die "--repo is required"
[[ -n "${READER_APP_ID}" ]] || die "--reader-app-id is required"
[[ -n "${READER_PEM}" ]] || die "--reader-pem is required"
[[ -n "${PUBLISHER_APP_ID}" ]] || die "--publisher-app-id is required"
[[ -n "${PUBLISHER_PEM}" ]] || die "--publisher-pem is required"
[[ -f "${READER_PEM}" ]] || die "reader pem not found: ${READER_PEM}"
[[ -f "${PUBLISHER_PEM}" ]] || die "publisher pem not found: ${PUBLISHER_PEM}"
[[ "${URL_AUTH}" == "sprite" || "${URL_AUTH}" == "public" ]] \
  || die "--url-auth must be sprite or public"

validate_local_prereqs

SPRITE_SCOPE_ARGS=("-s" "${SPRITE_NAME}")
if [[ -n "${ORG_NAME}" ]]; then
  SPRITE_SCOPE_ARGS=("-o" "${ORG_NAME}" "-s" "${SPRITE_NAME}")
fi

SPRITE_SERVICE_ARGS=()
if [[ -n "${ORG_NAME}" ]]; then
  SPRITE_SERVICE_ARGS+=(--org "${ORG_NAME}")
fi

log "deriving installation IDs for ${TARGET_REPO}"
READER_INSTALLATION_ID="$(derive_installation_id "${READER_APP_ID}" "${READER_PEM}" "${TARGET_REPO}")"
PUBLISHER_INSTALLATION_ID="$(derive_installation_id "${PUBLISHER_APP_ID}" "${PUBLISHER_PEM}" "${TARGET_REPO}")"
[[ -n "${READER_INSTALLATION_ID}" ]] || die "empty reader installation id"
[[ -n "${PUBLISHER_INSTALLATION_ID}" ]] || die "empty publisher installation id"
log "reader installation id: ${READER_INSTALLATION_ID}"
log "publisher installation id: ${PUBLISHER_INSTALLATION_ID}"

log "validating reader app token mint"
(
  cd "${REPO_ROOT}"
  GITHUB_APP_ID="${READER_APP_ID}" \
  GITHUB_APP_INSTALLATION_ID="${READER_INSTALLATION_ID}" \
  GITHUB_APP_PRIVATE_KEY_PATH="${READER_PEM}" \
  GITHUB_APP_PERMISSIONS_JSON='{"contents":"read"}' \
  ./scripts/mint-gh-app-installation-token.sh >/dev/null
)

log "validating publisher app token mint"
(
  cd "${REPO_ROOT}"
  GITHUB_APP_ID="${PUBLISHER_APP_ID}" \
  GITHUB_APP_INSTALLATION_ID="${PUBLISHER_INSTALLATION_ID}" \
  GITHUB_APP_PRIVATE_KEY_PATH="${PUBLISHER_PEM}" \
  GITHUB_APP_PERMISSIONS_JSON='{"contents":"write","pull_requests":"write"}' \
  ./scripts/mint-gh-app-installation-token.sh >/dev/null
)

TMP_REMOTE_SCRIPT="$(mktemp)"
trap '/bin/rm -f "${TMP_REMOTE_SCRIPT}"' EXIT

cat > "${TMP_REMOTE_SCRIPT}" <<EOF
#!/usr/bin/env bash
set -euo pipefail

READER_APP_ID="${READER_APP_ID}"
READER_INSTALLATION_ID="${READER_INSTALLATION_ID}"
PUBLISHER_APP_ID="${PUBLISHER_APP_ID}"
PUBLISHER_INSTALLATION_ID="${PUBLISHER_INSTALLATION_ID}"
TARGET_REPO="${TARGET_REPO}"
DEFAULT_BASE="${DEFAULT_BASE}"
CFG="/etc/computer-mcp/config.toml"

echo "[remote] install latest computer-mcp"
curl -fsSL https://raw.githubusercontent.com/amxv/computer-mcp/main/scripts/install.sh | \
  sudo env \
    COMPUTER_MCP_HTTP_BIND_PORT=8080 \
    COMPUTER_MCP_AGENT_HOME=/home/computer-mcp-agent \
    COMPUTER_MCP_DEFAULT_WORKDIR=/workspace \
    bash

echo "[remote] install key files"
sudo install -d -m 0750 -o root -g computer-mcp /etc/computer-mcp/reader /etc/computer-mcp/publisher
sudo install -m 0640 -o root -g computer-mcp /tmp/reader.pem /etc/computer-mcp/reader/private-key.pem
sudo install -m 0600 -o computer-mcp-publisher -g computer-mcp /tmp/publisher.pem /etc/computer-mcp/publisher/private-key.pem

echo "[remote] enforce sprite-safe ports"
sudo awk '
  BEGIN {seen_bind=0; inserted_http=0}
  /^bind_port = / {
    print "bind_port = 8443"
    if (!inserted_http) {
      print "http_bind_port = 8080"
      inserted_http=1
    }
    seen_bind=1
    next
  }
  /^http_bind_port = / {next}
  {print}
  END {
    if (!seen_bind) {
      print "bind_port = 8443"
      if (!inserted_http) {
        print "http_bind_port = 8080"
      }
    }
  }
' "\$CFG" | sudo tee "\$CFG" >/dev/null

echo "[remote] enforce agent workspace defaults"
sudo awk '
  BEGIN {
    seen_agent_home=0
    seen_default_workdir=0
  }
  /^agent_home = / {
    print "agent_home = \"/home/computer-mcp-agent\""
    seen_agent_home=1
    next
  }
  /^default_workdir = / {
    print "default_workdir = \"/workspace\""
    seen_default_workdir=1
    next
  }
  {print}
  END {
    if (!seen_agent_home) {
      print "agent_home = \"/home/computer-mcp-agent\""
    }
    if (!seen_default_workdir) {
      print "default_workdir = \"/workspace\""
    }
  }
' "\$CFG" | sudo tee "\$CFG" >/dev/null

echo "[remote] replace managed GitHub apps config block"
TMP_CFG="\$(mktemp)"
TMP_BLOCK="\$(mktemp)"
sudo awk '
  BEGIN {skip=0}
  /^# BEGIN COMPUTER_MCP_GH_APPS_MANAGED\$/ {skip=1; next}
  /^# END COMPUTER_MCP_GH_APPS_MANAGED\$/ {skip=0; next}
  skip==0 {print}
' "\$CFG" > "\$TMP_CFG"

cat > "\$TMP_BLOCK" <<CFG_BLOCK
# BEGIN COMPUTER_MCP_GH_APPS_MANAGED
reader_app_id = \${READER_APP_ID}
reader_installation_id = \${READER_INSTALLATION_ID}
publisher_app_id = \${PUBLISHER_APP_ID}

[[publisher_targets]]
id = "\${TARGET_REPO}"
repo = "\${TARGET_REPO}"
default_base = "\${DEFAULT_BASE}"
installation_id = \${PUBLISHER_INSTALLATION_ID}
# END COMPUTER_MCP_GH_APPS_MANAGED
CFG_BLOCK

sudo bash -lc "cat '\$TMP_CFG' '\$TMP_BLOCK' > '\$CFG'"
rm -f "\$TMP_CFG" "\$TMP_BLOCK"
sudo chgrp computer-mcp "\$CFG"
sudo chmod 0640 "\$CFG"
rm -f /tmp/reader.pem /tmp/publisher.pem /tmp/setup-computer-mcp-sprite.sh

echo "[remote] verify agent workspace defaults"
sudo -u computer-mcp-agent env HOME=/home/computer-mcp-agent bash -lc '
  cd /workspace
  test -w /workspace
  pwd
  touch .computer-mcp-write-check
  rm -f .computer-mcp-write-check
'
echo

echo "[remote] verify agent git commit identity"
sudo -u computer-mcp-agent env HOME=/home/computer-mcp-agent bash -lc '
  smoke_dir=/workspace/.git-identity-smoke
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
echo

echo "[remote] verify private GitHub HTTPS access through reader helper"
sudo -u computer-mcp-agent env HOME=/home/computer-mcp-agent \
  git -C /workspace ls-remote "https://github.com/${TARGET_REPO}.git" HEAD >/dev/null

echo "[remote] verify agent still cannot read publisher private key"
if sudo -u computer-mcp-agent env HOME=/home/computer-mcp-agent \
  bash -lc 'cat /etc/computer-mcp/publisher/private-key.pem >/dev/null 2>&1'; then
  echo "[remote] ERROR: computer-mcp-agent unexpectedly gained publisher key access" >&2
  exit 1
fi

echo "[remote] stop detached process-mode stack before Sprite service handoff"
sudo computer-mcp stop || true

echo "[remote] done"
EOF

log "running remote sprite setup"
sprite "${SPRITE_SCOPE_ARGS[@]}" exec \
  --file "${TMP_REMOTE_SCRIPT}:/tmp/setup-computer-mcp-sprite.sh" \
  --file "${READER_PEM}:/tmp/reader.pem" \
  --file "${PUBLISHER_PEM}:/tmp/publisher.pem" \
  bash /tmp/setup-computer-mcp-sprite.sh

log "syncing Sprite Services"
if [[ -n "${ORG_NAME}" ]]; then
  "${REPO_ROOT}/scripts/sprite-services.sh" \
    sync \
    --sprite "${SPRITE_NAME}" \
    "${SPRITE_SERVICE_ARGS[@]}" \
    --config /etc/computer-mcp/config.toml \
    --force-recreate
else
  "${REPO_ROOT}/scripts/sprite-services.sh" \
    sync \
    --sprite "${SPRITE_NAME}" \
    --config /etc/computer-mcp/config.toml \
    --force-recreate
fi

log "verifying publisher socket permissions after Sprite service handoff"
sprite "${SPRITE_SCOPE_ARGS[@]}" exec -- sudo bash -lc '
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

log "setting sprite URL auth: ${URL_AUTH}"
sprite "${SPRITE_SCOPE_ARGS[@]}" url update --auth "${URL_AUTH}" >/dev/null

SPRITE_URL="$(sprite "${SPRITE_SCOPE_ARGS[@]}" url | awk '/^URL:/ {print $2; exit}')"
if [[ -n "${SPRITE_URL}" ]]; then
  SPRITE_HOST="${SPRITE_URL#https://}"
  SPRITE_HOST="${SPRITE_HOST%/}"
  log "verifying Sprite health via public URL"
  curl -fsS --retry 3 --retry-all-errors --retry-delay 2 "${SPRITE_URL%/}/health" >/dev/null
  log "Sprite health OK: ${SPRITE_URL%/}/health"
  log "MCP URL hint:"
  sprite "${SPRITE_SCOPE_ARGS[@]}" exec -- sudo computer-mcp show-url --host "${SPRITE_HOST}" || true
else
  log "could not parse sprite URL; run: sprite ${SPRITE_SCOPE_ARGS[*]} url"
fi

log "completed successfully"
