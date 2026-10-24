#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
COMPOSE="$ROOT/deploy/testnet-9003/docker-compose.yml"
PIDFILE="$ROOT/deploy/testnet-9003/bare.pids"

if [[ -f "$PIDFILE" ]]; then
  while read -r p; do
    kill "$p" 2>/dev/null || true
  done <"$PIDFILE"
  rm -f "$PIDFILE"
  echo "stopped bare testnet-9003 processes"
fi

if command -v docker >/dev/null 2>&1; then
  docker compose -f "$COMPOSE" down 2>/dev/null || true
  echo "docker testnet-9003 down (if was running)"
fi
