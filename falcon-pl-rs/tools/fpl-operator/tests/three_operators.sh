#!/usr/bin/env bash
# Local 3-operator network test: gossip auth, replay, cross-check agree, disagreement halt,
# watcher challenge, public feed. Fake falcon-pl-ctl / bitcoin-cli; no real BTC, no real keys.
set -euo pipefail
BIN=${BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/release/fpl-operatord}
T=$(mktemp -d); trap 'kill $(jobs -p) 2>/dev/null || true; rm -rf "$T"' EXIT
cat > "$T/ctl" <<'X'
#!/bin/sh
cat "$LEDGER"
X
cat > "$T/btc" <<'X'
#!/bin/sh
prev=""
for a in "$@"; do
  if [ "$prev" = getrawtransaction ]; then if [ -f "$TXDIR/$a.json" ]; then cat "$TXDIR/$a.json"; exit 0; else echo "No such mempool or blockchain transaction" >&2; exit 5; fi; fi
  prev=$a
  case "$a" in getbalances) echo "{\"mine\":{\"trusted\":${BAL:-1.0}}}"; exit 0;; gettxout) echo '{"value":0.0001}'; exit 0;; esac
done
echo '{}'
X
chmod +x "$T/ctl" "$T/btc"
ledger() { echo "{\"height\":100,\"rails\":[{\"asset\":\"BTC\",\"bitvm2_claims\":[{\"note_id\":\"w1\",\"dest\":\"$1\",\"amount\":10000,\"open_height\":90,\"status\":\"AwaitingPegout\"}]}]}"; }
HONEST=tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx; ATTACKER=mrCDrCybB6J1vRfbwM5hemdJz73FwDBC8r
ledger $HONEST > "$T/ledger_ok.json"; ledger $ATTACKER > "$T/ledger_bad.json"
mkdir -p "$T/txs"; export TXDIR=$T/txs
for i in 1 2 3; do
  H="$T/op$i"; "$BIN" init --home "$H" >/dev/null; cp "$(dirname "$0")/../scripts/fpl-operator-challenge.sh" "$H/bin/"
  # fake fpl_challenge: records each invocation; idempotent like the real one (per deposit)
  cat > "$H/bin/fpl_challenge" <<'X'
#!/usr/bin/env bash
dep=""; while [ $# -gt 0 ]; do [ "$1" = --deposit ] && dep=$2; shift; done
echo "dry=$FPL_DRY_RUN dep=$dep secret_set=$([ -n "$FPL_OPERATOR_SECRET" ] && echo 1 || echo 0)" >> "$FPL_OP_HOME/fake_challenge.calls"
if [ "$FPL_DRY_RUN" = 1 ]; then echo "DRY_RUN would challenge $dep"; exit 0; fi
f="$FPL_OP_HOME/fake_challenged_$(echo "$dep" | tr ':' _)"
if [ -f "$f" ]; then echo "ALREADY_CHALLENGED $(cat "$f")"; exit 0; fi
t=$(printf '%s' "$dep" | sha256sum | cut -c1-64); echo "$t" > "$f"; echo "CHALLENGE_TXID $t"
X
  chmod +x "$H/bin/fpl_challenge"
  ( umask 077; printf '%064x' $((i*7919)) > "$H/keys/operator.secret" )
  # deposit graph fixture: same public confirm tx (10,000 sats) for all
  mkdir -p "$H/graphs/dep1"; cp "$(dirname "$0")/fixtures/final_min.json" "$H/graphs/dep1/final.json"
  LED=$T/ledger_ok.json; [[ $i == 3 && "${MODE:-}" == disagree ]] && LED=$T/ledger_bad.json
  cat >> "$H/operator.env" <<E
FPL_OP_NAME=op$i
FPL_OP_FALCON_CTL=$T/ctl
FPL_OP_BITCOIN_CLI=$T/btc
FPL_OP_HEALTH_LISTEN=127.0.0.1:$((19580+i))
FPL_OP_P2P_LISTEN=127.0.0.1:$((19590+i))
FPL_OP_POLL_SECS=5
FPL_OP_PREGEN_TARGET=0
FPL_OP_INTENT_WAIT_SECS=6
FPL_OP_PAYOUT_DEADLINE_SECS=25
FPL_OP_CHALLENGE_RETRY_SECS=5
E
  [[ $i == 1 ]] && echo "FPL_OP_MODE=live" >> "$H/operator.env"
  echo "$LED" > "$H/ledger_path"
done
peers='['; for i in 1 2 3; do x=$("$BIN" pubkey --home "$T/op$i"); peers+="{\"name\":\"op$i\",\"xonly\":\"$x\",\"url\":\"http://127.0.0.1:$((19590+i))\"},"; done; peers="${peers%,}]"
for i in 1 2 3; do echo "$peers" > "$T/op$i/peers.json"; done
# op2 is underfunded: 0.0005 BTC = 50,000 sats = the default reserve -> 0 free capacity
for i in 1 2 3; do BAL=1.0; [[ $i == 2 ]] && BAL=0.0005; BAL=$BAL LEDGER=$(cat "$T/op$i/ledger_path") "$BIN" run --home "$T/op$i" > "$T/op$i/run.log" 2>&1 & done
sleep 22
# unregistered signer must be rejected
code=$(curl -s -o /dev/null -w '%{http_code}' -X POST --data '{"v":1,"kind":"Heartbeat","note_id":"","binding":"","data":{},"signer":"'"$(printf 'ab%.0s' {1..32})"'","ts":1,"sig":"00"}' http://127.0.0.1:19591/gossip)
echo "unregistered push -> $code"; [[ $code == 403 ]]
for i in 1 2 3; do echo "op$i: $(curl -s 127.0.0.1:$((19580+i))/status | python3 -c 'import json,sys;d=json.load(sys.stdin);c=d["claims"][0] if d["claims"] else {};print(d["halted"],c.get("phase"),"|",c.get("detail"))')"; done
curl -s 127.0.0.1:19591/feed > "$T/feed.json"
python3 - "$T/feed.json" "${MODE:-agree}" <<'P'
import json,sys
f=json.load(open(sys.argv[1])); mode=sys.argv[2]
print("feed operators:",[(o["name"],o["heartbeats"],o["available_sats"]) for o in f["operators"]])
cap={o["name"]:o["available_sats"] for o in f["operators"]}
assert cap["op2"]==0 and cap["op1"]>=10000 and cap["op3"]>=10000, cap
intents=[r["signer"] for c in f["claims"] for r in c["records"] if r["kind"]=="ClaimIntent"]
op2=[o["xonly"] for o in f["operators"] if o["name"]=="op2"][0]
assert op2 not in intents, "underfunded op2 must not claim"
print("capacity gating ok: op2 sent no intent")
kinds=sorted({r["kind"] for c in f["claims"] for r in c["records"]})
print("feed claim records:",kinds,"disagreements:",len(f["disagreements"]))
assert len(f["operators"])==3 and all(o["heartbeats"]>=1 for o in f["operators"])
if mode=="disagree": assert f["halted"] and len(f["disagreements"])>=1
else: assert "ClaimIntent" in kinds and not f["halted"]
P
# rogue kickoff: op3 signs a Kickoff for a withdrawal that is not in the Falcon ledger -> watchers challenge
echo '{"v":1,"kind":"Kickoff","note_id":"fake-note","binding":"00","data":{"deposit":"7989e45da7d2f5396995b401956a341f6da35687f5882ed3b3bcacc5bed9751c:0"},"signer":"","ts":0,"sig":""}' \
  | "$BIN" sign-record --home "$T/op3" | curl -s -X POST --data-binary @- http://127.0.0.1:19591/gossip >/dev/null
sleep 8
n=$(curl -s 127.0.0.1:19591/feed | python3 -c 'import json,sys;print(sum(1 for r in json.load(sys.stdin)["disagreements"] if r["kind"]=="Challenge" and r["note_id"]=="fake-note"))')
echo "challenges of rogue kickoff in feed: $n"; [[ $n -ge 1 ]]
grep -h "fake-note" "$T"/op*/logs/challenges.log | head -2
# ---- exact payout check + on-chain challenge wiring (op1 live, op2/op3 dry-run) ----
B=$(curl -s 127.0.0.1:19591/feed | python3 -c 'import json,sys;print([c["binding"] for c in json.load(sys.stdin)["claims"] if c["note_id"]=="w1"][0])')
SPK=$("$BIN" payout-spk $HONEST 90 w1 testnet4)
PLAIN=0014751e76e8199196d454941c45d1b3a323f1433bd6
mktx() { echo "{\"txid\":\"$1\",\"confirmations\":1,\"vout\":[{\"n\":0,\"value\":$2,\"scriptPubKey\":{\"hex\":\"$3\"}}]}" > "$TXDIR/$1.json"; }
TXP=$(printf 'a%.0s' {1..64}); TXS=$(printf 'b%.0s' {1..64}); TXG=$(printf 'c%.0s' {1..64}); TXS2=$(printf 'd%.0s' {1..64})
mktx $TXP 0.00010000 $PLAIN     # pays the plain dest address (not the inscribed payout script)
mktx $TXS 0.00005000 $SPK       # exact script but too small
mktx $TXG 0.00009950 $SPK       # exact script, amount - fee: valid
mktx $TXS2 0.00005000 $SPK
dep() { printf '%064x:0' "$1"; }
send() { echo "{\"v\":1,\"kind\":\"$1\",\"note_id\":\"w1\",\"binding\":\"$B\",\"data\":$2,\"signer\":\"\",\"ts\":0,\"sig\":\"\"}" | "$BIN" sign-record --home "$T/op3" | curl -s -X POST --data-binary @- http://127.0.0.1:19591/gossip >/dev/null; }
send Kickoff "{\"deposit\":\"$(dep 1)\"}"
send PegOut "{\"deposit\":\"$(dep 1)\",\"peg_out_txid\":\"$TXP\"}"
send PegOut "{\"deposit\":\"$(dep 1)\",\"peg_out_txid\":\"$TXS2\"}"   # 2nd bad record, same graph: no 2nd challenge
send PegOut "{\"deposit\":\"$(dep 2)\",\"peg_out_txid\":\"$TXS\"}"
send PegOut "{\"deposit\":\"$(dep 3)\",\"peg_out_txid\":\"$TXG\"}"
send Kickoff "{\"deposit\":\"$(dep 3)\"}"                              # honest: valid PegOut -> never challenged
send Kickoff "{\"deposit\":\"$(dep 4)\"}"                              # no PegOut -> challenged after the deadline
sleep 14
echo "early (before deadline) op1 calls:"; cat "$T/op1/fake_challenge.calls"
if grep -q "dep=$(dep 4)" "$T/op1/fake_challenge.calls"; then echo "challenged before deadline"; exit 1; fi
sleep 22
python3 - "$T" "$(dep 1)" "$(dep 2)" "$(dep 3)" "$(dep 4)" <<'P'
import json,sys,urllib.request,collections
T,d1,d2,d3,d4=sys.argv[1:]
calls=lambda i:[l.split() for l in open(f"{T}/op{i}/fake_challenge.calls")]
live=collections.Counter(c[1][4:] for c in calls(1) if c[0]=="dry=0")
print("op1 live fpl_challenge calls per deposit:",{k[-10:]:v for k,v in live.items()})
assert all(c[2]=="secret_set=1" for c in calls(1))
for d in (d1,d2,d4): assert live[d]==1, (d,live[d])
assert live[d3]==0, "valid exact payout must not be challenged"
dry2=calls(2)
assert dry2 and all(c[0]=="dry=1" for c in dry2), "op2 is dry-run: plan only"
f=json.load(urllib.request.urlopen("http://127.0.0.1:19591/feed"))
op={o["xonly"]:o["name"] for o in f["operators"]}
ctx=[(op[r["signer"]],r["data"]["deposit"],r["data"]["challenge_txid"]) for r in f["disagreements"] if r["kind"]=="ChallengeTx"]
print("gossiped ChallengeTx:",[(n,d[-10:],t[:12]) for n,d,t in ctx])
assert all(n=="op1" for n,_,_ in ctx), "dry-run operators must not broadcast"
assert sorted(d for _,d,_ in ctx if d in (d1,d2,d4))==sorted([d1,d2,d4])
reasons=[r["data"]["reason"] for r in f["disagreements"] if r["kind"]=="Challenge" and r["data"].get("deposit") in (d1,d2,d4)]
assert any("expected withdrawal script" in x for x in reasons), reasons
assert any("< minimum" in x for x in reasons), reasons
assert any("deadline" in x for x in reasons), reasons
assert not any(r["kind"] in("Challenge","ChallengeTx") and r["data"].get("deposit")==d3 for r in f["disagreements"])
print("exact payout check + challenge wiring ok")
P
# replay: restart op1, records reloaded from disk
kill %1; sleep 1; LEDGER=$T/ledger_ok.json "$BIN" run --home "$T/op1" > "$T/op1/run2.log" 2>&1 & sleep 2
grep -o "replayed_records=[0-9]*" "$T/op1/run2.log"
echo "secret occurrences in logs: $(cat "$T"/op*/run*.log | grep -c -f <(cat "$T"/op*/keys/operator.secret) || true)"
echo "PASS ${MODE:-agree}"
