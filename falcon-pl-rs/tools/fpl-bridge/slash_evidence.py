#!/usr/bin/env python3
"""Build an OperatorSlash body (BRIDGE) from a mined BitVM2 Disprove /
DisproveChain on Bitcoin, using a local bitcoind (read-only RPC: getrawtransaction,
getblock, getblockheader). Prints JSON: {"body": <TxBody>, "header": {...}, "graph": {...}}.

  slash_evidence.py --cli "bitcoin-cli -datadir=D" --txid <disprove txid> \
      --operator-xonly <64hex> --deposit <peg-in deposit txid>

Submit with: falcon-pl-ctl operator --account <you> --body "$(jq -c .body out.json)"
The ledger re-checks everything (SPV against its BTC rail, tx shape against the
operator's bound graph); this script only gathers the bytes.
"""
import argparse, hashlib, json, shlex, subprocess

def cli(base, *a):
    return subprocess.check_output(shlex.split(base) + list(a), text=True).strip()

def h256(b):
    return hashlib.sha256(hashlib.sha256(b).digest()).digest()

def varint(b, i):
    n = b[i]
    if n < 0xfd: return n, i + 1
    w = {0xfd: 2, 0xfe: 4, 0xff: 8}[n]
    return int.from_bytes(b[i + 1:i + 1 + w], "little"), i + 1 + w

def strip_witness(raw_hex):
    """Legacy (non-witness) serialization: same txid, no witness bytes. The
    ledger only needs txid + inputs + outputs, and a Disprove witness is ~1 MB."""
    b = bytes.fromhex(raw_hex)
    if len(b) < 6 or b[4] != 0 or b[5] != 1:
        return raw_hex
    i = 6
    out = bytearray(b[:4])
    start = i
    n_in, i = varint(b, i)
    for _ in range(n_in):
        i += 36
        sl, i = varint(b, i)
        i += sl + 4
    n_out, i = varint(b, i)
    for _ in range(n_out):
        i += 8
        sl, i = varint(b, i)
        i += sl
    out += b[start:i]
    out += b[-4:]
    return bytes(out).hex()

def merkle_path(txids_display, idx):
    level = [bytes.fromhex(t)[::-1] for t in txids_display]
    path = []
    while len(level) > 1:
        if len(level) % 2:
            level.append(level[-1])
        sib = idx ^ 1
        path.append(level[sib].hex())  # internal byte order, as the ledger expects
        level = [h256(level[i] + level[i + 1]) for i in range(0, len(level), 2)]
        idx //= 2
    return path, level[0][::-1].hex()

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cli", required=True)
    ap.add_argument("--txid", required=True)
    ap.add_argument("--operator-xonly", default="")
    ap.add_argument("--deposit", default="")
    ap.add_argument("--pegin", action="store_true",
                    help="BRIDGE BTC4: build a btc4_peg_in body for a graph PegInConfirm txid")
    a = ap.parse_args()
    if not a.pegin and (not a.operator_xonly or not a.deposit):
        ap.error("--operator-xonly and --deposit are required for slash evidence")
    tx = json.loads(cli(a.cli, "getrawtransaction", a.txid, "true"))
    blk = json.loads(cli(a.cli, "getblock", tx["blockhash"], "1"))
    idx = blk["tx"].index(a.txid)
    path, root = merkle_path(blk["tx"], idx)
    assert root == blk["merkleroot"], "merkle root mismatch"
    hdr_hex = cli(a.cli, "getblockheader", tx["blockhash"], "false")
    vin = [(i["txid"], i["vout"]) for i in tx["vin"]]
    graph = {}
    if len(vin) == 2 and vin[0][0] == vin[1][0]:
        graph["kind"] = "disprove"
        graph["assert_final_txid"] = vin[0][0]
        # AssertFinal input 0 = connector D from AssertInitial; AssertInitial input 0 = KickOff2:1
        af = json.loads(cli(a.cli, "getrawtransaction", vin[0][0], "true"))
        ai = json.loads(cli(a.cli, "getrawtransaction", af["vin"][0]["txid"], "true"))
        graph["assert_initial_txid"] = af["vin"][0]["txid"]
        graph["kick_off_2_txid"] = ai["vin"][0]["txid"]
        graph["kick_off_2_vout_spent_by_assert_initial"] = ai["vin"][0]["vout"]
    elif len(vin) == 1:
        graph["kind"] = "disprove_chain"
        graph["kick_off_2_txid"] = vin[0][0]
    assert h256(bytes.fromhex(strip_witness(tx["hex"])))[::-1].hex() == a.txid, "stripped txid mismatch"
    if a.pegin:
        print(json.dumps({"body": {"kind": "btc4_peg_in", "raw_tx_hex": strip_witness(tx["hex"]),
                                    "btc_height": blk["height"], "merkle_path": path, "merkle_index": idx},
                          "confirmations": tx.get("confirmations", 0)}, indent=1))
        return
    out = {
        "body": {
            "kind": "operator_slash",
            "operator_btc_xonly": a.operator_xonly,
            "deposit_txid": a.deposit,
            "raw_tx_hex": strip_witness(tx["hex"]),
            "btc_height": blk["height"],
            "merkle_path": path,
            "merkle_index": idx,
        },
        "header": {"height": blk["height"], "hash": blk["hash"], "prev": blk.get("previousblockhash", ""),
                   "merkle_root": blk["merkleroot"], "raw": hdr_hex},
        "graph": graph,
        "outputs": [{"sats": round(o["value"] * 1e8), "spk": o["scriptPubKey"]["hex"]} for o in tx["vout"]],
    }
    print(json.dumps(out, indent=1))

if __name__ == "__main__":
    main()
