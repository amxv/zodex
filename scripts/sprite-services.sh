#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/sprite-services.sh <command> [options]

Commands:
  sync     Create or update the Sprite Services for zodex.
  status   List Sprite Services and pretty-print their current state.
  logs     Read Sprite Service logs for a specific service.

Common options:
  --sprite <name>         Required. Sprite name.
  --org <name>            Optional. Sprite organization.

Sync options:
  --config <path>         Service config path inside the Sprite.
                          Default: /etc/computer-mcp/config.toml
  --skip-stop-detached    Skip the pre-sync `computer-mcp stop` attempt.
  --force-recreate        Delete both services first, then recreate them.
                          Useful when Sprite reports stale running state.

Logs options:
  --service <name>        Required for logs. Service name.
  --lines <count>         Optional log line count.
  --duration <duration>   Optional follow duration, for example 5s.

Examples:
  scripts/sprite-services.sh sync --sprite computer --org amxv
  scripts/sprite-services.sh sync --sprite computer --org amxv --force-recreate
  scripts/sprite-services.sh status --sprite computer --org amxv
  scripts/sprite-services.sh logs --sprite computer --org amxv --service computer-mcpd --lines 100
EOF
}

log() {
  printf '[sprite-services] %s\n' "$*"
}

die() {
  printf '[sprite-services] ERROR: %s\n' "$*" >&2
  exit 1
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

COMMAND="${1:-}"
if [[ -z "${COMMAND}" ]]; then
  usage
  exit 1
fi
shift || true

SPRITE_NAME=""
ORG_NAME=""
CONFIG_PATH="/etc/computer-mcp/config.toml"
SERVICE_NAME=""
LINES=""
DURATION=""
STOP_DETACHED=1
FORCE_RECREATE=0

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
    --config)
      CONFIG_PATH="$2"
      shift 2
      ;;
    --service)
      SERVICE_NAME="$2"
      shift 2
      ;;
    --lines)
      LINES="$2"
      shift 2
      ;;
    --duration)
      DURATION="$2"
      shift 2
      ;;
    --skip-stop-detached)
      STOP_DETACHED=0
      shift
      ;;
    --force-recreate)
      FORCE_RECREATE=1
      shift
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

case "${COMMAND}" in
  sync|status|logs)
    ;;
  *)
    die "unknown command: ${COMMAND}"
    ;;
esac

if [[ "${COMMAND}" == "logs" && -z "${SERVICE_NAME}" ]]; then
  die "--service is required for logs"
fi

require_cmd sprite
require_cmd jq

SPRITE_SCOPE_ARGS=("-s" "${SPRITE_NAME}")
if [[ -n "${ORG_NAME}" ]]; then
  SPRITE_SCOPE_ARGS=("-o" "${ORG_NAME}" "-s" "${SPRITE_NAME}")
fi

sprite_api_json() {
  local path="$1"
  shift

  local raw
  raw="$(sprite api "${SPRITE_SCOPE_ARGS[@]}" "${path}" -- -sS "$@")"
  if [[ "${raw}" == Calling\ API:* ]]; then
    printf '%s\n' "${raw}" | sed '1,2d'
  else
    printf '%s\n' "${raw}"
  fi
}

sprite_api_status_code() {
  local path="$1"
  shift

  local raw
  raw="$(sprite api "${SPRITE_SCOPE_ARGS[@]}" "${path}" -- -sS -o /dev/null -w "%{http_code}\n" "$@")"
  printf '%s\n' "${raw}" | tail -n 1
}

stop_detached_process_mode() {
  log "stopping detached process-mode daemons if present"
  if ! sprite exec "${SPRITE_SCOPE_ARGS[@]}" -- sudo computer-mcp stop; then
    log "warning: failed to stop detached daemons cleanly; continuing with Sprite Service sync"
  fi
}

delete_service_if_present() {
  local service_name="$1"
  local status_code

  status_code="$(sprite_api_status_code "/services/${service_name}" -X DELETE)"
  case "${status_code}" in
    204)
      log "deleted existing service ${service_name}"
      ;;
    404)
      log "service ${service_name} was already absent"
      ;;
    *)
      die "failed to delete service ${service_name} (HTTP ${status_code})"
      ;;
  esac
}

publisher_service_payload() {
  jq -nc --arg config "${CONFIG_PATH}" '{
    cmd: "sudo",
    args: [
      "-n",
      "-u",
      "computer-mcp-publisher",
      "/usr/local/bin/computer-mcp-prd",
      "--config",
      $config
    ],
    needs: [],
    http_port: null
  }'
}

main_service_payload() {
  jq -nc --arg config "${CONFIG_PATH}" '{
    cmd: "sudo",
    args: [
      "-n",
      "-u",
      "computer-mcp-agent",
      "/usr/local/bin/computer-mcpd",
      "--config",
      $config
    ],
    needs: ["computer-mcp-prd"],
    http_port: 8080
  }'
}

print_status() {
  sprite_api_json "/services" | jq .
}

print_logs() {
  local query=""
  local sep="?"

  if [[ -n "${LINES}" ]]; then
    query="${query}${sep}lines=${LINES}"
    sep="&"
  fi
  if [[ -n "${DURATION}" ]]; then
    query="${query}${sep}duration=${DURATION}"
  fi

  sprite_api_json "/services/${SERVICE_NAME}/logs${query}" | jq .
}

sync_services() {
  if [[ "${STOP_DETACHED}" == "1" ]]; then
    stop_detached_process_mode
  fi

  if [[ "${FORCE_RECREATE}" == "1" ]]; then
    log "force-recreate requested; deleting Sprite Services before upsert"
    delete_service_if_present "computer-mcpd"
    delete_service_if_present "computer-mcp-prd"
  fi

  log "upserting service computer-mcp-prd"
  sprite_api_json \
    "/services/computer-mcp-prd" \
    -X PUT \
    -H "Content-Type: application/json" \
    -d "$(publisher_service_payload)" >/dev/null

  log "upserting service computer-mcpd"
  sprite_api_json \
    "/services/computer-mcpd" \
    -X PUT \
    -H "Content-Type: application/json" \
    -d "$(main_service_payload)" >/dev/null

  log "current Sprite Service inventory"
  print_status
}

case "${COMMAND}" in
  sync)
    sync_services
    ;;
  status)
    print_status
    ;;
  logs)
    print_logs
    ;;
esac
