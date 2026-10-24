#!/usr/bin/env bash
# Distributed FROST sign: t seats keep their shares. Coordinator holds no share.
# Usage:
#   pl-btc-frost-coord.sh sign-message HEX
#   (round1 on v1-v4, make package, round2, aggregate → prints 64-byte sig hex)
set -euo pipefail
ROOT="${PL_PUBLIC_ROOT:-$HOME/falcon-pl-public-testnet-2300}"
CTL="${FPL_CTL:-$ROOT/bin/falcon-pl-ctl}"
SEATS=(v1 v2 v3 v4)
FULL="v1,v2,v3,v4,v5,v6"
WORK="${PL_FROST_WORK:-$ROOT/run/frost-sign}"
mkdir -p "$WORK"

# host:keys_dir:ssh
seat_info() {
  case "$1" in
    v1) echo "local|$ROOT/keys|" ;;
    v2) echo "remote|/root/falcon-pl-public-testnet-2300/keys|root@100.64.0.2" ;;
    v3) echo "remote|/home/falcon/falcon-pl-public-testnet-2300/keys|root@100.64.0.3" ;;
    v4) echo "remote|/root/falcon-pl-public-testnet-2300/keys|root@100.64.0.4" ;;
    v5) echo "remote|/home/falcon/falcon-pl-public-testnet-2300/keys|falcon@100.64.0.5" ;;
    v6) echo "remote|/root/falcon-pl-public-testnet-2300/keys|root@100.64.0.6" ;;
    *) echo "bad seat $1" >&2; return 1 ;;
  esac
}

remote_ctl() {
  local host="$1" keys="$2"
  echo "$host" | grep -q '^local$' && echo "$CTL --keys-dir $keys" && return
  echo "ssh -o ConnectTimeout=12 $host $keys/../bin/falcon-pl-ctl --keys-dir $keys"
}

run_seat() {
  local seat="$1"; shift
  IFS='|' read -r kind keys ssh <<<"$(seat_info "$seat")"
  if [ "$kind" = local ]; then
    "$CTL" "$@" --keys-dir "$keys"
  else
    local rctl
    rctl=$(ssh -o ConnectTimeout=12 "$ssh" "ls $keys/../bin/falcon-pl-ctl 2>/dev/null || ls /root/falcon-pl-public-testnet-2300/bin/falcon-pl-ctl")
    ssh -o ConnectTimeout=12 "$ssh" "$rctl $* --keys-dir $keys"
  fi
}

copy_from_seat() {
  local seat="$1" src="$2" dest="$3"
  IFS='|' read -r kind keys ssh <<<"$(seat_info "$seat")"
  if [ "$kind" = local ]; then
    cp -a "$src" "$dest"
  else
    scp -o ConnectTimeout=12 "$ssh:$src" "$dest"
  fi
}

copy_to_seat() {
  local seat="$1" src="$2" dest="$3"
  IFS='|' read -r kind keys ssh <<<"$(seat_info "$seat")"
  if [ "$kind" = local ]; then
    cp -a "$src" "$dest"
  else
    scp -o ConnectTimeout=12 "$src" "$ssh:$dest"
  fi
}

cmd="${1:-}"
msg="${2:-}"
if [ "$cmd" != sign-message ] || [ ${#msg} -lt 64 ]; then
  echo "usage: $0 sign-message <32-byte hex sighash>" >&2
  exit 2
fi
msg="${msg#0x}"

echo "round1 ${SEATS[*]}" >&2
for s in "${SEATS[@]}"; do
  run_seat "$s" frost-sign-round1 --seat "$s"
  IFS='|' read -r kind keys ssh <<<"$(seat_info "$s")"
  copy_from_seat "$s" "$keys/frost-sign-commit-$s" "$WORK/frost-sign-commit-$s"
done
# commits also needed in v1 keys for make-package
for s in "${SEATS[@]}"; do
  cp -a "$WORK/frost-sign-commit-$s" "$ROOT/keys/frost-sign-commit-$s"
done
"$CTL" frost-make-package --keys-dir "$ROOT/keys" --signers "v1,v2,v3,v4" --seats "$FULL" \
  --message-hex "$msg" --out "$WORK/signing-package.json"
echo "round2" >&2
for s in "${SEATS[@]}"; do
  IFS='|' read -r kind keys ssh <<<"$(seat_info "$s")"
  copy_to_seat "$s" "$WORK/signing-package.json" "$keys/signing-package.json"
  run_seat "$s" frost-sign-round2 --seat "$s" --seats "$FULL" --signing-package "$keys/signing-package.json"
  copy_from_seat "$s" "$keys/frost-sign-share-$s" "$WORK/frost-sign-share-$s"
  cp -a "$WORK/frost-sign-share-$s" "$ROOT/keys/frost-sign-share-$s"
done
"$CTL" frost-aggregate --keys-dir "$ROOT/keys" --signers "v1,v2,v3,v4" --seats "$FULL" \
  --signing-package "$WORK/signing-package.json" --message-hex "$msg"
