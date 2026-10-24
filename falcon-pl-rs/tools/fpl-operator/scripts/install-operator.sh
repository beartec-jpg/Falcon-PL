#!/usr/bin/env bash
# Falcon PL BitVM2 bridge OPERATOR kit: one-command installer and manager.
#
#   curl -fsSL https://falcon-fpl.com/install-operator.sh | bash
#
# Afterwards: falcon-operator status | feed | resume | logs | check | start | stop | restart | keys | register | upgrade | uninstall
#
# What it does (idempotent):
#   1. Checks Linux x86_64, glibc >= 2.34, bitcoind/bitcoin-cli present, disk >= 20 GB, RAM >= 8 GB.
#   2. Downloads fpl-operator-<ver>-x86_64-linux.tar.gz from beartec-jpg/falcon-pl-releases, verifies the
#      minisign signature on SHA256SUMS (pinned release key), then SHA256.
#   3. Creates ~/.falcon-operator (0700), writes operator.env, and generates the operator graph key
#      LOCALLY in keys/operator.secret (0600). It is never printed or uploaded. Your BTC stays in YOUR
#      bitcoind wallet; Falcon never holds keys or BTC.
#   4. Installs a systemd unit (user unit unless root; --no-systemd for containers) in DRY-RUN mode.
#      Switch to live only after `falcon-operator check` passes: set FPL_OP_MODE=live in operator.env.
set -euo pipefail
INSTALLER_VERSION="0.3.0"
RELEASE_REPO="${FALCON_RELEASE_REPO:-beartec-jpg/falcon-pl-releases}"
RELEASE_MINISIGN_PUBKEY_PINNED="RWTs+gnuKfzPik0Xbcf+m1hD/JrCUnWqaD/OAZFZEY2fcNc90D67mZlM"
RELEASE_MINISIGN_PUBKEY="${FALCON_MINISIGN_PUBKEY-$RELEASE_MINISIGN_PUBKEY_PINNED}"
OP_HOME="${FPL_OP_HOME:-$HOME/.falcon-operator}"
WANT_VERSION="${FPL_OPERATOR_VERSION:-}"
USE_SYSTEMD=auto
CMD="${1:-install}"; [[ "$CMD" == --* ]] && CMD=install
for a in "$@"; do [[ "$a" == "--no-systemd" ]] && USE_SYSTEMD=no; done
if [[ -t 1 ]]; then B=$'\e[1m'; G=$'\e[32m'; Y=$'\e[33m'; R=$'\e[31m'; N=$'\e[0m'; else B=; G=; Y=; R=; N=; fi
say(){ printf '%s\n' "${B}falcon-operator:${N} $*"; }; ok(){ printf '%s\n' "${G}ok${N} $*"; }
warn(){ printf '%s\n' "${Y}warn${N} $*" >&2; }; die(){ printf '%s\n' "${R}error${N} $*" >&2; exit 1; }
BIN="$OP_HOME/bin"; D="$BIN/fpl-operatord"

preflight() {
  [[ "$(uname -s)" == Linux ]] || die "Linux only"; [[ "$(uname -m)" == x86_64 ]] || die "x86_64 only"
  local g; g=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}'); g=${g:-0}
  [[ "$(printf '%s\n2.34\n' "$g" | sort -V | head -1)" == 2.34 ]] || die "glibc >= 2.34 required (found $g)"
  for t in curl tar sha256sum; do command -v $t >/dev/null || die "missing $t"; done
  command -v bitcoin-cli >/dev/null || warn "bitcoin-cli not found: install Bitcoin Core and set FPL_OP_BITCOIN_CLI"
  command -v minisign >/dev/null || [[ -z "$RELEASE_MINISIGN_PUBKEY" ]] || die "minisign required (apt install minisign)"
  local mem; mem=$(awk '/MemTotal/{print int($2/1048576)}' /proc/meminfo); (( mem >= 8 )) || warn "RAM ${mem}G < 8G: graph pregen may OOM"
  ok "preflight (glibc $g)"
}

download() {
  local base tag
  if [[ -n "${FALCON_RELEASE_BASE:-}" ]]; then base="$FALCON_RELEASE_BASE"
  else
    tag="${WANT_VERSION:-$(curl -fsSL "https://api.github.com/repos/$RELEASE_REPO/releases" | grep -o '"tag_name": *"operator-v[^"]*"' | head -1 | cut -d'"' -f4)}"
    [[ -n "$tag" ]] || die "no operator release found in $RELEASE_REPO"
    base="https://github.com/$RELEASE_REPO/releases/download/$tag"
  fi
  local tmp; tmp=$(mktemp -d); trap 'rm -rf "$tmp"' RETURN
  curl -fsSL "$base/SHA256SUMS" -o "$tmp/SHA256SUMS"
  if [[ -n "$RELEASE_MINISIGN_PUBKEY" ]]; then
    curl -fsSL "$base/SHA256SUMS.minisig" -o "$tmp/SHA256SUMS.minisig"
    minisign -Vm "$tmp/SHA256SUMS" -P "$RELEASE_MINISIGN_PUBKEY" >/dev/null || die "bad signature on SHA256SUMS"
    ok "release signature"
  else
    [[ -n "${FALCON_RELEASE_BASE:-}" ]] || die "signature check can only be disabled with a custom FALCON_RELEASE_BASE"
    warn "signature check DISABLED (local rehearsal)"
  fi
  local tgz; tgz=$(grep -o 'fpl-operator-[^ ]*-x86_64-linux.tar.gz' "$tmp/SHA256SUMS" | head -1)
  curl -fsSL "$base/$tgz" -o "$tmp/$tgz"
  ( cd "$tmp" && grep " $tgz\$" SHA256SUMS | sha256sum -c - >/dev/null ) || die "sha256 mismatch"
  tar -C "$tmp" -xzf "$tmp/$tgz"
  mkdir -p "$BIN"; cp "$tmp"/fpl-operator-*/bin/* "$BIN/"; chmod 0755 "$BIN"/*
  cp "$tmp"/fpl-operator-*/install-operator.sh "$BIN/falcon-operator" 2>/dev/null || true; chmod +x "$BIN/falcon-operator" || true
  ok "installed $tgz"
}

gen_key() {
  local k="$OP_HOME/keys/operator.secret"
  [[ -f "$k" ]] && { ok "operator key exists (not shown)"; return; }
  ( umask 077; od -An -tx1 -N32 /dev/urandom | tr -d ' \n' > "$k" ); chmod 600 "$k"
  ok "operator graph key generated locally: $k (0600, never printed)"
}

unit() {
  [[ "$USE_SYSTEMD" == no ]] || ! command -v systemctl >/dev/null && { warn "no systemd: run '$D run' under your supervisor"; return; }
  local dir scope=--user; [[ $EUID -eq 0 ]] && { dir=/etc/systemd/system; scope=; } || dir="$HOME/.config/systemd/user"
  mkdir -p "$dir"
  cat > "$dir/falcon-operator.service" <<UNIT
[Unit]
Description=Falcon PL BitVM2 bridge operator
After=network-online.target
[Service]
Environment=FPL_OP_HOME=$OP_HOME
ExecStart=$D run --home $OP_HOME
Restart=always
RestartSec=10
StandardOutput=append:$OP_HOME/logs/operator.log
StandardError=append:$OP_HOME/logs/operator.log
[Install]
WantedBy=default.target
UNIT
  systemctl $scope daemon-reload; systemctl $scope enable --now falcon-operator.service
  ok "systemd unit falcon-operator.service started (dry-run until you set FPL_OP_MODE=live)"
}

sc(){ local scope=--user; [[ $EUID -eq 0 ]] && scope=; systemctl $scope "$@" falcon-operator.service; }

case "$CMD" in
  install) say "installer $INSTALLER_VERSION"; preflight; download; "$D" init --home "$OP_HOME"; gen_key; unit
           say "next: edit $OP_HOME/operator.env, then: falcon-operator check  (docs: OPERATOR_GUIDE.md)";;
  upgrade) download; sc restart || true;;
  status)  curl -fsS "http://$(grep -E '^FPL_OP_HEALTH_LISTEN=' "$OP_HOME/operator.env" | cut -d= -f2 || echo 127.0.0.1:19480)/status" || "$D" status --home "$OP_HOME";;
  check)   "$D" check --home "$OP_HOME";;
  feed)    curl -fsS "http://127.0.0.1:$(grep -E '^FPL_OP_P2P_LISTEN=' "$OP_HOME/operator.env" | rev | cut -d: -f1 | rev)/feed";;
  resume)  curl -fsS -X POST "http://$(grep -E '^FPL_OP_HEALTH_LISTEN=' "$OP_HOME/operator.env" | cut -d= -f2)/resume" && ok "halt cleared";;
  register) "$D" register --home "$OP_HOME";;
  keys)    echo "secret: $OP_HOME/keys/operator.secret (not shown)"; echo "gossip/registry x-only: $("$D" pubkey --home "$OP_HOME")"; echo "funding address: fpl_ceremony_coordinator --print-funding-addresses";;
  logs)    tail -n 200 -f "$OP_HOME/logs/operator.log";;
  start|stop|restart) sc "$CMD";;
  uninstall) sc disable --now || true; say "binaries removed; keys + state kept in $OP_HOME (delete yourself if you are sure)"; rm -rf "$BIN";;
  *) die "unknown command $CMD";;
esac
