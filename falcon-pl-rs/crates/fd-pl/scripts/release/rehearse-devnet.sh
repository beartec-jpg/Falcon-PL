#!/usr/bin/env bash
# Local 4-seat devnet for installer rehearsals (127.0.0.1 only, network 2200, falcon-512).
#   devnet.sh start|stop|status|load-start|load-stop
set -euo pipefail
D="${DEVNET_ROOT:-$PWD/devnet-net}"
BIN="${DEVNET_BIN:-$PWD/dist}"
NID=2200
VALS=v1,v2,v3,v4
PORTS=(19601 19602 19603 19604)
cmd="${1:-status}"
mkdir -p "$D"/{keys,logs,snap,run}
case "$cmd" in
  start)
    if [[ ! -f "$D/keys/pl_keys.json" ]]; then
      "$BIN/falcon-pl-ctl" gen-keys --keys-dir "$D/keys" --scheme falcon-512 --validators "$VALS" --load-wallets 0 >/dev/null
      for a in faucet alice; do "$BIN/falcon-pl-ctl" gen-wallet --keys-dir "$D/keys" --account "$a" --scheme falcon-512 >/dev/null; done
    fi
    ALL=$(printf '127.0.0.1:%s,' "${PORTS[@]}"); ALL=${ALL%,}
    for i in 0 1 2 3; do
      id="v$((i+1))"; port=${PORTS[$i]}
      [[ -f "$D/run/$id.pid" ]] && kill -0 "$(cat "$D/run/$id.pid")" 2>/dev/null && continue
      extra=(); [[ $id == v1 ]] && extra=(--archive --hub --max-chain-ledgers 0); [[ $id == v2 ]] && extra=(--hub)
      peers=$(tr ',' '\n' <<<"$ALL" | grep -v ":$port\$" | paste -sd,)
      nohup "$BIN/falcon-pl-node" --id "$id" "${extra[@]}" --listen "127.0.0.1:$port" --peers "$peers" \
        --hubs 127.0.0.1:19601,127.0.0.1:19602 --validators "$VALS" --scheme falcon-512 --keys-dir "$D/keys" \
        --load-wallets 0 --bond 1000 --network-id $NID --seal-interval-ms 200 --pack-budget-ms 1000 \
        --progress-timeout-ms 3000 --max-txs 0 --max-mempool-txs 50000 --epoch-ms 600000 --first-claim-epoch 1 \
        --unbond-cooldown-ms 60000 --snapshot-interval-ms 30000 --online-epoch-heights 10 --lock-public-wire \
        --admin-sock "$D/run/$id.admin.sock" --snapshot "$D/snap/$id.json" --snapshot-out "$D/snap/$id.json" \
        >>"$D/logs/$id.log" 2>&1 &
      echo $! >"$D/run/$id.pid"; echo "started $id :$port pid $!"
    done ;;
  stop)
    for f in "$D"/run/*.pid; do [[ -f $f ]] && kill "$(cat "$f")" 2>/dev/null || true; rm -f "$f"; done; echo stopped ;;
  load-start)
    # tiny tx load so the on-demand chain keeps producing blocks: alice pays faucet 1 FPL every 3 s
    nohup bash -c "
      '$BIN/falcon-pl-ctl' --addr 127.0.0.1:19601 faucet --to alice --amount 1000000 --scheme falcon-512 --keys-dir '$D/keys' --network-id $NID >/dev/null 2>&1
      while :; do '$BIN/falcon-pl-ctl' --addr 127.0.0.1:19602 faucet --to alice --amount 1 --scheme falcon-512 --keys-dir '$D/keys' --network-id $NID >/dev/null 2>&1; sleep 3; done" \
      >"$D/logs/load.log" 2>&1 &
    echo $! >"$D/run/load.pid"; echo "load pid $!" ;;
  load-stop) [[ -f $D/run/load.pid ]] && kill "$(cat "$D/run/load.pid")" 2>/dev/null; rm -f "$D/run/load.pid" ;;
  status)
    for p in "${PORTS[@]}"; do
      printf '%s ' "$p"; timeout 10 "$BIN/falcon-pl-ctl" --addr "127.0.0.1:$p" status 2>/dev/null | grep -m2 -oE '"(tip_height|product_version)": *[^,]+' | tr '\n' ' '; echo
    done ;;
esac
