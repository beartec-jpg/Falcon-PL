#!/usr/bin/env bash
# Falcon PL — wallet + faucet staging (private fleet → v2.3 prep).
#
#   bash crates/fd-pl/scripts/pl-wallet-stage.sh setup
#   bash crates/fd-pl/scripts/pl-wallet-stage.sh drip --to user1 --amount 5000
#   bash crates/fd-pl/scripts/pl-wallet-stage.sh demo --user user1
#   bash crates/fd-pl/scripts/pl-wallet-stage.sh balance --account user1
set -euo pipefail

RS="$(cd "$(dirname "$0")/../../.." && pwd)"
BIN="${PL_BIN:-$RS/target/release}"
ROOT="${PL_ROOT:-$HOME/falcon-pl-private-v22}"
KEYS="${PL_KEYS:-$ROOT/keys}"
ADDR="${PL_ADDR:-127.0.0.1:19101}"
SCHEME="${PL_SCHEME:-hmac}"
CTL=("$BIN/falcon-pl-ctl" --addr "$ADDR")

cmd="${1:-help}"
shift || true

need_keys() {
  if [[ ! -f "$KEYS/pl_keys.json" ]]; then
    echo "missing keys at $KEYS — run private fleet start or:"
    echo "  $BIN/falcon-pl-ctl gen-keys --keys-dir $KEYS --scheme $SCHEME --validators v1,v2,v3"
    exit 1
  fi
}

case "$cmd" in
  setup)
    mkdir -p "$KEYS"
    if [[ ! -f "$KEYS/pl_keys.json" ]]; then
      "$BIN/falcon-pl-ctl" gen-keys \
        --keys-dir "$KEYS" \
        --scheme "$SCHEME" \
        --validators v1,v2,v3 \
        --load-wallets "${PL_LOAD_WALLETS:-32}"
    fi
    # ensure faucet key exists (regen partial via gen-wallet if old keys pre-faucet)
    "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account faucet --scheme "$SCHEME"
    echo "staging keys ready: $KEYS"
    "${CTL[@]}" account --account faucet || true
    ;;

  gen-user)
    need_keys
    user="${1:-}"
    [[ -n "$user" ]] || { echo "usage: $0 gen-user <account>"; exit 1; }
    "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account "$user" --scheme "$SCHEME"
    echo "created wallet $user"
    ;;

  drip|faucet)
    need_keys
    to=""
    amount=1000
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --to) to="$2"; shift 2 ;;
        --amount) amount="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    [[ -n "$to" ]] || { echo "usage: $0 drip --to <acct> [--amount N]"; exit 1; }
    # ensure recipient key exists for later spends
    "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$KEYS" --account "$to" --scheme "$SCHEME" >/dev/null
    "${CTL[@]}" faucet --to "$to" --amount "$amount" --fee 2 --sequence 0 \
      --scheme "$SCHEME" --keys-dir "$KEYS"
    sleep 1
    "${CTL[@]}" account --account "$to"
    ;;

  balance|account)
    need_keys
    acct=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --account) acct="$2"; shift 2 ;;
        *) acct="${acct:-$1}"; shift ;;
      esac
    done
    [[ -n "$acct" ]] || { echo "usage: $0 balance --account <id>"; exit 1; }
    "${CTL[@]}" account --account "$acct"
    ;;

  pay)
    need_keys
    from="" to="" amount=10 fee=2
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --from) from="$2"; shift 2 ;;
        --to) to="$2"; shift 2 ;;
        --amount) amount="$2"; shift 2 ;;
        --fee) fee="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    [[ -n "$from" && -n "$to" ]] || { echo "usage: $0 pay --from a --to b [--amount N]"; exit 1; }
    "${CTL[@]}" pay --from "$from" --to "$to" --amount "$amount" --fee "$fee" \
      --sequence 0 --scheme "$SCHEME" --keys-dir "$KEYS"
    sleep 0.5
    "${CTL[@]}" account --account "$from"
    "${CTL[@]}" account --account "$to"
    ;;

  demo)
    need_keys
    user="user1"
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --user) user="$2"; shift 2 ;;
        *) shift ;;
      esac
    done
    echo "=== wallet staging demo for $user ==="
    "$0" gen-user "$user"
    "$0" drip --to "$user" --amount 5000
    echo "[demo] wait for seal…"
    sleep 2
    "$0" balance --account "$user"
    # user pays alice a little
    "${CTL[@]}" pay --from "$user" --to alice --amount 25 --fee 2 --sequence 0 \
      --scheme "$SCHEME" --keys-dir "$KEYS" || true
    sleep 2
    "$0" balance --account "$user"
    # vault open toward alice
    "${CTL[@]}" vault-open --account "$user" --destination alice || true
    "${CTL[@]}" claim --account "$user" || true
    echo "=== demo done ==="
    "${CTL[@]}" status | python3 -c "
import sys,json
b=json.load(sys.stdin)
body=b.get('body',b)
print('tip', body.get('tip_height'),
      'suggested_min_fee', body.get('suggested_min_fee'),
      'faucet_seq', (body.get('faucet') or {}).get('sequence'),
      'faucet_bal', (body.get('faucet') or {}).get('balance'))
accts=body.get('accounts') or {}
if '$user' in accts:
  print('user', accts['$user'])
"
    ;;

  help|*)
    cat <<EOF
Falcon PL wallet/faucet staging

  setup                         ensure keys + faucet key
  gen-user <id>                 create wallet key
  drip --to <id> [--amount N]   faucet drip (auto seq)
  balance --account <id>        seq/balance/claimable
  pay --from a --to b …         auto-seq payment
  demo [--user id]              end-to-end staging flow

Env: PL_ROOT PL_KEYS PL_ADDR PL_SCHEME PL_BIN
EOF
    [[ "$cmd" == "help" ]] || exit 1
    ;;
esac
