#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  GITHUB_APP_ID=123 \
  GITHUB_APP_INSTALLATION_ID=456 \
  GITHUB_APP_PRIVATE_KEY_PATH=/path/to/app.private-key.pem \
  ./scripts/mint-gh-app-installation-token.sh [--json]

Environment:
  GITHUB_APP_ID
  GITHUB_APP_INSTALLATION_ID
  GITHUB_APP_PRIVATE_KEY_PATH
  GITHUB_APP_PERMISSIONS_JSON      Optional. Default: {"contents":"write","pull_requests":"write","workflows":"write"}
  GITHUB_API_URL                   Optional. Default: https://api.github.com

Output:
  By default, prints only the installation token.
  Use --json to print the full GitHub API response.
EOF
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

die() {
  echo "$*" >&2
  exit 1
}

b64url() {
  openssl base64 -A | tr '+/' '-_' | tr -d '='
}

output_mode="token"
if [[ "${1:-}" == "--json" ]]; then
  output_mode="json"
elif [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
elif [[ $# -gt 0 ]]; then
  usage >&2
  exit 1
fi

require_cmd curl
require_cmd jq
require_cmd openssl

: "${GITHUB_APP_ID:?GITHUB_APP_ID is required}"
: "${GITHUB_APP_INSTALLATION_ID:?GITHUB_APP_INSTALLATION_ID is required}"
: "${GITHUB_APP_PRIVATE_KEY_PATH:?GITHUB_APP_PRIVATE_KEY_PATH is required}"

if [[ ! -f "${GITHUB_APP_PRIVATE_KEY_PATH}" ]]; then
  echo "private key file not found: ${GITHUB_APP_PRIVATE_KEY_PATH}" >&2
  exit 1
fi

github_api_url="${GITHUB_API_URL:-https://api.github.com}"
permissions_json="${GITHUB_APP_PERMISSIONS_JSON:-}"
if [[ -z "${permissions_json}" ]]; then
  permissions_json='{"contents":"write","pull_requests":"write","workflows":"write"}'
fi

printf '%s' "${permissions_json}" | jq -e . >/dev/null \
  || die "GITHUB_APP_PERMISSIONS_JSON must be valid JSON"

now="$(date +%s)"
issued_at="$((now - 60))"
expires_at="$((now + 540))"

header='{"alg":"RS256","typ":"JWT"}'
payload="$(jq -cn \
  --arg iss "${GITHUB_APP_ID}" \
  --argjson iat "${issued_at}" \
  --argjson exp "${expires_at}" \
  '{iat: $iat, exp: $exp, iss: $iss}')"

unsigned_token="$(printf '%s' "${header}" | b64url).$(printf '%s' "${payload}" | b64url)"
signature="$(printf '%s' "${unsigned_token}" \
  | openssl dgst -binary -sha256 -sign "${GITHUB_APP_PRIVATE_KEY_PATH}" \
  | b64url)"
jwt="${unsigned_token}.${signature}"

request_body="$(jq -cn --argjson permissions "${permissions_json}" '{permissions: $permissions}')"
response="$(
  curl -fsSL \
    -X POST \
    -H "Accept: application/vnd.github+json" \
    -H "Authorization: Bearer ${jwt}" \
    -H "X-GitHub-Api-Version: 2022-11-28" \
    "${github_api_url}/app/installations/${GITHUB_APP_INSTALLATION_ID}/access_tokens" \
    -d "${request_body}"
)"

token="$(printf '%s' "${response}" | jq -r '.token')"
if [[ -z "${token}" || "${token}" == "null" ]]; then
  echo "failed to mint installation token" >&2
  printf '%s\n' "${response}" >&2
  exit 1
fi

if [[ "${output_mode}" == "json" ]]; then
  printf '%s\n' "${response}"
else
  printf '%s\n' "${token}"
fi
