#!/usr/bin/env bash
# Dealerless FROST DKG across 2300 seats. Combined secret never exists.
#
# Each seat writes only keys/vault-frost-share-<id> (mode 600).
# Round secrets are wiped after part3. Public packages are exchanged.
#
# Do NOT run this while the published P2WPKH still holds coins — FROST is a
# new P2TR key. Sweep Shamir first, or DKG only after that address is empty.
# Do NOT copy vault-frost-share-* between hosts.
set -euo pipefail

ROOT="${PL_ROOT:-/home/falcon/falcon-pl-public-testnet-2300}"
CTL="${CTL:-$ROOT/bin/falcon-pl-ctl}"
SEATS=(v1 v2 v3 v4 v5 v6)
# t = ceil(n×0.6) = 4 for n=6
T="${T:-4}"
SSH_OPTS=(-o BatchMode=yes -o ConnectTimeout=12 -o StrictHostKeyChecking=accept-new)

# host:keys_dir  (joiners v6/v7 are not lottery signers by default)
declare -A HOST KEYS
HOST[v1]="local"
KEYS[v1]="$ROOT/keys"
HOST[v2]="${HIL_SSH:-root@100.64.0.2}"
KEYS[v2]="${RROOT_TS:-/root/falcon-pl-public-testnet-2300}/keys"
HOST[v3]="${F2_SSH:-host2}"
KEYS[v3]="$ROOT/keys"
HOST[v4]="${FSN_SSH:-root@100.64.0.4}"
KEYS[v4]="${RROOT_TS:-/root/falcon-pl-public-testnet-2300}/keys"
HOST[v5]="${DROP_SSH:-host3}"
KEYS[v5]="$ROOT/keys"
HOST[v6]="${MHR_SSH:-root@100.64.0.6}"
KEYS[v6]="${RROOT_TS:-/root/falcon-pl-public-testnet-2300}/keys"

SEAT_CSV=$(IFS=,; echo "${SEATS[*]}")
STAGING=$(mktemp -d /tmp/fpl-frost-dkg.XXXXXX)
cleanup() { rm -rf "$STAGING"; }
trap cleanup EXIT

run_seat() {
  local seat=$1; shift
  local host=${HOST[$seat]}
  local keys=${KEYS[$seat]}
  if [[ $host == local ]]; then
    "$@" --keys-dir "$keys" --seat "$seat" --seats "$SEAT_CSV" --t "$T"
  else
    ssh "${SSH_OPTS[@]}" "$host" \
      "mkdir -p '$keys' && $*" --keys-dir "$keys" --seat "$seat" --seats "$SEAT_CSV" --t "$T"
  fi
}

copy_from() {
  local seat=$1 rel=$2 dest=$3
  local host=${HOST[$seat]} keys=${KEYS[$seat]}
  if [[ $host == local ]]; then
    cp -a "$keys/$rel" "$dest"
  else
    scp "${SSH_OPTS[@]}" "$host:$keys/$rel" "$dest"
  fi
}

copy_to() {
  local seat=$1 src=$2 rel=$3
  local host=${HOST[$seat]} keys=${KEYS[$seat]}
  if [[ $host == local ]]; then
    cp -a "$src" "$keys/$rel"
    chmod 644 "$keys/$rel" 2>/dev/null || true
  else
    scp "${SSH_OPTS[@]}" "$src" "$host:$keys/$rel"
  fi
}

echo "[frost-dkg] staging $STAGING seats=$SEAT_CSV t=$T"
echo "[frost-dkg] part1"
for s in "${SEATS[@]}"; do
  if [[ ${HOST[$s]} == local ]]; then
    "$CTL" frost-dkg-part1 --keys-dir "${KEYS[$s]}" --seat "$s" --seats "$SEAT_CSV" --t "$T"
  else
    ssh "${SSH_OPTS[@]}" "${HOST[$s]}" "test -x '${KEYS[$s]}/../bin/falcon-pl-ctl' || echo missing-ctl"
    ssh "${SSH_OPTS[@]}" "${HOST[$s]}" \
      "${KEYS[$s]}/../bin/falcon-pl-ctl frost-dkg-part1 --keys-dir '${KEYS[$s]}' --seat '$s' --seats '$SEAT_CSV' --t $T"
  fi
  copy_from "$s" "frost-dkg-r1-$s" "$STAGING/frost-dkg-r1-$s"
done

echo "[frost-dkg] fan r1 packages"
for s in "${SEATS[@]}"; do
  for p in "${SEATS[@]}"; do
    [[ $p == "$s" ]] && continue
    copy_to "$s" "$STAGING/frost-dkg-r1-$p" "frost-dkg-r1-$p"
  done
done

echo "[frost-dkg] part2"
for s in "${SEATS[@]}"; do
  if [[ ${HOST[$s]} == local ]]; then
    "$CTL" frost-dkg-part2 --keys-dir "${KEYS[$s]}" --seat "$s" --seats "$SEAT_CSV"
  else
    ssh "${SSH_OPTS[@]}" "${HOST[$s]}" \
      "${KEYS[$s]}/../bin/falcon-pl-ctl frost-dkg-part2 --keys-dir '${KEYS[$s]}' --seat '$s' --seats '$SEAT_CSV'"
  fi
  for p in "${SEATS[@]}"; do
    [[ $p == "$s" ]] && continue
    copy_from "$s" "frost-dkg-r2-$s-to-$p" "$STAGING/frost-dkg-r2-$s-to-$p"
  done
done

echo "[frost-dkg] route r2 packages"
for s in "${SEATS[@]}"; do
  for p in "${SEATS[@]}"; do
    [[ $p == "$s" ]] && continue
    copy_to "$s" "$STAGING/frost-dkg-r2-$p-to-$s" "frost-dkg-r2-$p-to-$s"
  done
done

echo "[frost-dkg] part3"
ADDR=""
for s in "${SEATS[@]}"; do
  if [[ ${HOST[$s]} == local ]]; then
    out=$("$CTL" frost-dkg-part3 --keys-dir "${KEYS[$s]}" --seat "$s" --seats "$SEAT_CSV")
  else
    out=$(ssh "${SSH_OPTS[@]}" "${HOST[$s]}" \
      "${KEYS[$s]}/../bin/falcon-pl-ctl frost-dkg-part3 --keys-dir '${KEYS[$s]}' --seat '$s' --seats '$SEAT_CSV'")
  fi
  echo "$out"
  a=$(echo "$out" | awk '/^address /{print $2}')
  if [[ -z $ADDR ]]; then ADDR=$a; fi
  if [[ $a != "$ADDR" ]]; then
    echo "address mismatch $s $a vs $ADDR" >&2
    exit 1
  fi
done

echo "[frost-dkg] verify one share per host"
count_glob() {
  local host=$1 keys=$2 pat=$3
  if [[ $host == local ]]; then
    find "$keys" -maxdepth 1 -name "$pat" -type f 2>/dev/null | wc -l
  else
    ssh "${SSH_OPTS[@]}" "$host" "find '$keys' -maxdepth 1 -name '$pat' -type f 2>/dev/null | wc -l"
  fi
}
for s in "${SEATS[@]}"; do
  host=${HOST[$s]} keys=${KEYS[$s]}
  n=$(count_glob "$host" "$keys" "vault-frost-share-*")
  m=$(count_glob "$host" "$keys" "vault-share-*")
  n=${n// /}; m=${m// /}
  echo "  $s frost-shares=$n shamir-shares=$m"
  if [[ "${n:-0}" -ne 1 ]]; then
    echo "expected exactly one frost share on $s" >&2
    exit 1
  fi
done

echo "[frost-dkg] done address=$ADDR"
echo "publish only after Shamir P2WPKH is empty or swept. BTC_RAIL_LIVE stays false."
