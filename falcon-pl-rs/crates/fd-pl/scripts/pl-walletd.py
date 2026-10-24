#!/usr/bin/env python3
"""Local FPL wallet API for 2300 (ctl pay / vault-activate / account / dest-lock mint).

Listens on WALLETD_BIND:WALLETD_PORT (default 127.0.0.1:8788). Audit M-1:
  * Auth: if a key is configured (WALLETD_API_KEY or the file WALLETD_KEY_FILE), every
    request except GET /health must send "Authorization: Bearer <key>". Binding to a
    non-loopback address without a key is refused at startup.
  * Faucet: per-recipient limit (FPL_FAUCET_RECIPIENT_MAX drips per
    FPL_FAUCET_RECIPIENT_WINDOW_S, default 1 per 24 h) and a global limit
    (FPL_FAUCET_GLOBAL_MAX per hour, default 500). Counted only after a successful drip;
    state is kept in FPL_FAUCET_STATE so it survives restarts.
"""
import hashlib
import hmac
import ipaddress
import json
import os
import re
import subprocess
import sys
import tempfile
import time
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

CTL = os.environ.get("FALCON_PL_CTL", "/home/falcon/falcon-pl-public-testnet-2300/bin/falcon-pl-ctl")
KEYS = os.environ.get("FALCON_PL_KEYS", "/home/falcon/falcon-pl-public-testnet-2300/keys")
ADDR = os.environ.get("FALCON_PL_RPC", "127.0.0.1:19301")
NID = os.environ.get("FALCON_PL_NETWORK_ID", "2300")
SCHEME = "falcon-512"
MINTD = os.environ.get(
    "FPL_ETH_AUTO_MINT",
    "/home/falcon/falcon-pl-public-testnet-2300/scripts/pl-eth-auto-mint.py",
)
CLAIMD = os.environ.get(
    "FPL_ETH_CLAIM_PROOF",
    "/home/falcon/falcon-pl-public-testnet-2300/scripts/pl-eth-claim-proof.py",
)
MINT_KICK_LOG = "/home/falcon/falcon-pl-public-testnet-2300/logs/eth-auto-mint.kick.log"
CAST = os.environ.get("CAST", "/home/falcon/.foundry/bin/cast")
SEPOLIA_BRIDGE = os.environ.get(
    "SEPOLIA_BRIDGE",
    "0xdBF6855b00B78c047A729A21E13bfE5f4C991C05",
).strip()
# Upstreamed from the live host1 walletd: FPL_SEPOLIA_QC_V2_BRIDGE wins, then
# FALCON_QC_V2_BRIDGE (pl-bridge-install-config.py writes both with the same value),
# then the public Sepolia FalconQcBridgeV2 address.
QC_V2_BRIDGE = (
    os.environ.get("FPL_SEPOLIA_QC_V2_BRIDGE")
    or os.environ.get("FALCON_QC_V2_BRIDGE")
    or "0x811854827627024B38926Ea9DCc0f88ACd5fB23e"
).strip()
V2_BRIDGE = QC_V2_BRIDGE  # name used by the live copy
QC_V2_LIVE = bool(QC_V2_BRIDGE)
# FalconQcBridgeV3 (not deployed yet). Unset means V3 is off: a note past the V2 header
# keeps returning 409 "waiting for bridge header". Only FALCON_QC_V3_BRIDGE (the name
# pl-bridge-install-config.py writes); the seat/consensus FPL_SEPOLIA_QC_V3_BRIDGE is not read.
QC_V3_BRIDGE = (os.environ.get("FALCON_QC_V3_BRIDGE") or "").strip()
# The V3 constructor's anchor: the V2 fplTip it copied at deploy (from the deploy record,
# run/bridge/sepolia-qc-v3.json "anchor_height"). With V3 set, walletd opens on V2 only while
# V2 fplTip <= this height, and never a note V3 already holds. Unset/invalid with both bridges
# set: no claim opens at all, V2 or V3 (fail closed). FalconQcBridgeV2 does not know V3 exists, so this is the guard.
QC_V3_ANCHOR_HEIGHT = (os.environ.get("FALCON_QC_V3_ANCHOR_HEIGHT") or "").strip()
EVM_ADDR_RE = re.compile(r"^0x[0-9a-fA-F]{40}$")
ETH_RPC = os.environ.get("ETH_RPC", "https://sepolia.gateway.tenderly.co").strip()
# Public, keyless Sepolia RPCs tried in order when a call fails (live behaviour).
ETH_RPCS: list = []
for _u in (ETH_RPC, "https://sepolia.gateway.tenderly.co", "https://ethereum-sepolia-rpc.publicnode.com"):
    if _u and _u not in ETH_RPCS:
        ETH_RPCS.append(_u)
ETH_PK_FILE = os.environ.get(
    "ETH_PK_FILE",
    "/home/falcon/falcon-pl-vault-test/eth-wallets/deployer.privhex",
)
# ThreadingHTTPServer: a long claim-proof split search must not block other requests.
# Every state-changing POST action still runs one at a time under WRITE_LOCK, as it did on
# the single-threaded server. Only these read-only actions skip it; eth-open-claim builds
# its proof unlocked and takes WRITE_LOCK just for the cast send.
WRITE_LOCK = threading.RLock()
UNLOCKED_POST_ACTIONS = frozenset({"claim-proof", "eth-open-claim"})
# Bound the proof subprocesses (each can run a split search for up to 30 s). Past the limit a
# request is answered "busy, retry" at once instead of queueing threads and subprocesses.
PROOF_CONCURRENCY = max(1, int(os.environ.get("WALLETD_PROOF_CONCURRENCY", "2") or "2"))
PROOF_SLOTS = threading.BoundedSemaphore(PROOF_CONCURRENCY)
PROOF_BUSY = "claim proof busy, retry shortly"


class ProofBusy(RuntimeError):
    pass


_SLOT = threading.local()


class _proof_slot:
    """Admission for proof work. eth-open-claim takes it before any cast call (bridge
    selection reads fplTip/headers) and holds it through the V2/V3 proof attempts;
    claim_proof() inside that window does not take a second slot."""

    def __enter__(self):
        if getattr(_SLOT, "held", False):
            self.owner = False
            return self
        if not PROOF_SLOTS.acquire(blocking=False):
            raise ProofBusy(PROOF_BUSY)
        _SLOT.held = True
        self.owner = True
        return self

    def __exit__(self, *exc):
        if self.owner:
            _SLOT.held = False
            PROOF_SLOTS.release()
        return False
ACCT_RE = re.compile(r"^[A-Za-z0-9._-]{2,64}$")
TXID_RE = re.compile(r"^(0x)?[0-9a-fA-F]{64}$")

NAME_STORE = os.environ.get("FALCON_PL_NAME_STORE", "/home/falcon/falcon-pl-public-testnet-2300/run/name-reservations.json")
NAME_RESERVE_MS = 30 * 60 * 1000
RESERVED_NAMES = {"faucet","treasury","admin","falcon","fpl","pool","lend","bridge","community","builder","watcher","watcher-browser","genesis","root","operator","alice","bob","carol","dave","v1","v2","v3","v4","v5","v6","v7"}

def _name_norm(raw: str) -> str:
    s = (raw or "").strip().lower()
    if len(s) < 3 or len(s) > 32:
        raise ValueError("name: length must be 3–32")
    if s.startswith(".") or s.endswith(".") or ".." in s:
        raise ValueError("name: no leading, trailing, or double dots")
    if any(c not in "abcdefghijklmnopqrstuvwxyz0123456789." for c in s):
        raise ValueError("name: use a–z, 0–9, and optional dots")
    if s in RESERVED_NAMES:
        raise ValueError(f"name: '{s}' is reserved")
    return s

def _name_fee(name: str) -> int:
    n = len(name.replace(".", ""))
    if n <= 3: return 50000
    if n == 4: return 20000
    if n == 5: return 5000
    if n == 6: return 2000
    if n <= 8: return 500
    if n <= 12: return 200
    return 100

def _names_load() -> list:
    try:
        with open(NAME_STORE) as f:
            d = json.load(f)
        return d if isinstance(d, list) else []
    except Exception:
        return []

def _names_save(rows: list) -> None:
    os.makedirs(os.path.dirname(NAME_STORE), exist_ok=True)
    with open(NAME_STORE, "w") as f:
        json.dump(rows, f)

def _name_reserve(body: dict) -> dict:
    name = _name_norm(str(body.get("name") or ""))
    pk = str(body.get("publicKey") or "").strip()
    cred = str(body.get("credentialId") or "").strip()[:64]
    if not pk:
        raise ValueError("public key required")
    now = int(time.time() * 1000)
    rows = [r for r in _names_load() if r.get("activated") or int(r.get("reservedUntil") or 0) > now]
    existing = next((r for r in rows if r.get("name") == name), None)
    if existing and existing.get("activated"):
        raise ValueError("Name already activated")
    if existing and existing.get("publicKey") != pk:
        raise ValueError("Name is reserved by another wallet")
    if existing and existing.get("publicKey") == pk:
        return existing
    if cred and cred != "pending":
        held = next((r for r in rows if not r.get("activated") and r.get("credentialHash") == cred), None)
        if held and held.get("name") != name:
            raise ValueError(f"This passkey already reserved {held.get('name')}")
    heldk = next((r for r in rows if not r.get("activated") and r.get("publicKey") == pk), None)
    if heldk and heldk.get("name") != name:
        raise ValueError(f"This key already reserved {heldk.get('name')}")
    rec = {"name": name, "publicKey": pk, "credentialHash": cred, "reservedUntil": now + NAME_RESERVE_MS, "activated": False, "fee": _name_fee(name)}
    rows = [r for r in rows if r.get("name") != name]
    rows.append(rec)
    _names_save(rows)
    return rec

# v2.9.65 (audit C-1): from the chain's key-bind height a name can only be
# spent by a key its funder committed to. With FPL_FAUCET_PAY_TO_KEY=1 (set it
# once the chain passes TESTNET_2300_KEY_BIND_FROM_HEIGHT) the faucet drips with
# PayToKey: the key hash comes from the request (keyHash, or publicKey) or from
# this walletd's name reservation for `to`. Classic r-addresses need none.
_HEX64 = re.compile(r"^[0-9a-f]{64}$")


def _key_hash_of(pk_hex: str) -> str:
    raw = bytes.fromhex(pk_hex.strip())
    if len(raw) == 898 and raw[0] == 0xFB:
        raw = raw[1:]
    if len(raw) != 897:
        raise ValueError("publicKey must be a Falcon-512 public key")
    return hashlib.sha256(raw).hexdigest()


def _account_exists(name: str) -> bool:
    try:
        return bool(account(name).get("exists"))
    except Exception as e:
        raise ValueError(f"faucet: cannot check account {name}: {e}")


def faucet_key_hash(to: str, body: dict, exists=None) -> str:
    if os.environ.get("FPL_FAUCET_PAY_TO_KEY", "").strip() not in ("1", "true", "yes"):
        return ""
    if re.match(r"^r[1-9A-HJ-NP-Za-km-z]{24,34}$", to):
        return ""
    # PayToKey only for an account that does not exist yet. An existing
    # account gets a plain Pay top-up:
    # - bound account: the node refuses PayToKey ("destination already has a
    #   key"), and that error is not a permanent mempool failure, so the drip
    #   sits in the mempool at the faucet's sequence (seen live 2026-10-16);
    # - funded but unbound, uncommitted account (e.g. the pre-571000 ones):
    #   committing the key of whoever reserved the name here would hand them
    #   the balance (a C-1 takeover through the faucet);
    # - committed, not yet spent: the commitment is kept either way.
    if (exists or _account_exists)(to):
        return ""
    kh = str(body.get("keyHash") or "").strip().lower()
    if kh:
        if not _HEX64.match(kh):
            raise ValueError("keyHash must be 64 hex")
        return kh
    pk = str(body.get("publicKey") or "").strip()
    if not pk:
        rec = next((r for r in _names_load() if r.get("name") == to.strip().lower()), None)
        pk = str((rec or {}).get("publicKey") or "")
    if not pk:
        raise ValueError("faucet: this name has no key on file; reserve it in the wallet first")
    try:
        return _key_hash_of(pk)
    except ValueError as e:
        raise ValueError(str(e))


def _name_activate(body: dict) -> dict:
    name = _name_norm(str(body.get("name") or ""))
    pk = str(body.get("publicKey") or "").strip()
    now = int(time.time() * 1000)
    rows = [r for r in _names_load() if r.get("activated") or int(r.get("reservedUntil") or 0) > now]
    rec = next((r for r in rows if r.get("name") == name), None)
    if rec and rec.get("activated"):
        if rec.get("publicKey") != pk:
            raise ValueError("Name already activated")
        return rec
    if rec and rec.get("publicKey") != pk:
        raise ValueError("Name is reserved by another wallet")
    nextrec = {"name": name, "publicKey": pk, "credentialHash": (rec or {}).get("credentialHash"), "reservedUntil": 0, "activated": True, "fee": (rec or {}).get("fee") or _name_fee(name)}
    rows = [r for r in rows if r.get("name") != name]
    rows.append(nextrec)
    _names_save(rows)
    return nextrec



FROST_COORD = os.environ.get(
    "FPL_FROST_COORD",
    "/home/falcon/falcon-pl-public-testnet-2300/scripts/pl-btc-frost-coord.sh",
)
ABORT_SEED = os.environ.get(
    "FPL_ABORT_SEED",
    "/home/falcon/falcon-pl-public-testnet-2300/keys/bitvm-abort-seed",
)
FROST_LOCK = "/home/falcon/falcon-pl-public-testnet-2300/run/frost-kickoff.lock"
OLD_BTC = ("tb1q7dnl", "tb1qesum")
BTC_KICKOFF_FEE = 1000
BTC_EXIT_MODE = os.environ.get("BTC_EXIT_MODE", "watch").lower().strip()
BITVM2_INSTANCE_ADDRESS = os.environ.get(
    "BITVM2_INSTANCE_ADDRESS",
    "tb1p2xuekx55w9llxe023y070lf32kk0z873nv6pse0awg75ll7l930suzcgn5",
).strip()
BITVM2_INSTANCE_SPK = os.environ.get(
    "BITVM2_INSTANCE_SPK",
    "512051b99b1a94717ff365ea891fe7fd3155acf11fd19b341865fd723d4fffdf2c5f",
).strip()
BITVM2_CLAIMER = os.environ.get(
    "BITVM2_CLAIMER_FILE",
    "/home/falcon/falcon-pl-public-testnet-2300/keys/bitvm2-optimistic/claimer.privhex",
)
BITVM2_SETUP_XONLY = os.environ.get(
    "BITVM2_SETUP_XONLY",
    "b37f4f1e804b71860201193a322fb0c16aa5031a9b448798aee4d1c57c56806f,"
    "819e2473a327c8a3b43a7a608c8cdceb585e213272845c378d3970067689999a",
)
BITVM2_CSV = int(os.environ.get("BITVM2_PAYOUT_CSV", "144"))
BITVM2_ASSERT_CSV = int(os.environ.get("BITVM2_ASSERT_CSV", "72"))
BITVM2_CLAIM_CSV = int(os.environ.get("BITVM2_CLAIM_CSV", "6"))
BITVM2_KICKOFF_FEE = int(os.environ.get("BITVM2_KICKOFF_FEE", "1000"))
BITVM2_TAKE_FEE = int(os.environ.get("BITVM2_TAKE_FEE", "1000"))
BITVM2_MIN_OUT = 2000

# Fail-closed kill switch for every walletd action that signs or broadcasts a BTC
# spend (btc-kickoff: claimer key / FROST vault spend of the instance UTXO;
# btc-take: dest-lock take). Off unless FPL_BTC_KICKOFF_ENABLED is exactly "1",
# read on every request. Unset, "", "0", "true", "yes", " 1" ... all mean off.
# This is a stop-gap: per-note burn/dedupe checks follow in #54.
BTC_KICKOFF_ENV = "FPL_BTC_KICKOFF_ENABLED"
BTC_KICKOFF_DISABLED = "btc kickoff disabled"
BTC_SPEND_ACTIONS = frozenset({"btc-kickoff", "btc-take"})


class BtcKickoffDisabled(RuntimeError):
    """Raised when a BTC spend is attempted with the kill switch off (HTTP 403)."""


def btc_kickoff_enabled() -> bool:
    return os.environ.get(BTC_KICKOFF_ENV) == "1"


def require_btc_kickoff_enabled() -> None:
    """Raise before any key load, UTXO lookup or signing when the switch is off."""
    if not btc_kickoff_enabled():
        raise BtcKickoffDisabled(BTC_KICKOFF_DISABLED)


# Request hardening (#55), fail-closed. Every account-like field is checked before any
# key path is built or falcon-pl-ctl runs. pay / vault-activate need the `from` account in
# an allowlist read from the environment on every request (default empty = refuse all).
NAME_STRICT_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{0,63}$")
# Classic r-address (base58, mixed case): allowed only where a field is a recipient that
# never names a key file (pay `to`, vault `destination`, faucet recipient, GET ?account=).
CLASSIC_ADDR_RE = re.compile(r"^r[1-9A-HJ-NP-Za-km-z]{24,34}$")
PAY_FROM_ALLOW_ENV = "FPL_WALLETD_PAY_FROM_ALLOW"
VAULT_ALLOW_ENV = "FPL_WALLETD_VAULT_ALLOW"
FAUCET_MAX_ENV = "FPL_FAUCET_MAX"
# What the site's faucet sends (Falcon-faucet-wallet api/faucet PL_DRIP and the faucet page).
FAUCET_DEFAULT = 2000
# Actions whose account/from/to/name fields are PL names (lower-cased by their handlers).
_NAME_FIELD_ACTIONS = frozenset({
    "eth-open-claim", "claim-proof", "mint-eth-deposit", "mint-status",
    "btc-kickoff", "btc-take", "eth-kickoff", "name-status", "name-reserve", "name-activate",
})


class BadRequest(ValueError):
    def __init__(self, code: int, msg: str):
        super().__init__(msg)
        self.code = code


def strict_name(v: object) -> bool:
    return isinstance(v, str) and ".." not in v and bool(NAME_STRICT_RE.fullmatch(v))


def recipient_ok(v: object) -> bool:
    return strict_name(v) or (isinstance(v, str) and bool(CLASSIC_ADDR_RE.fullmatch(v)))


def env_allowlist(var: str) -> frozenset:
    return frozenset(x.strip() for x in os.environ.get(var, "").split(",") if x.strip())


def faucet_max() -> int:
    raw = os.environ.get(FAUCET_MAX_ENV, "").strip()
    if raw.isdigit() and 0 < int(raw) <= 10**12:
        return int(raw)
    return FAUCET_DEFAULT


def positive_int(v: object):
    """int for a positive integer (JSON int or decimal string), else None."""
    if isinstance(v, bool):
        return None
    if isinstance(v, int):
        return v if v > 0 else None
    if isinstance(v, str):
        t = v.strip()
        if t.isascii() and t.isdigit() and len(t) <= 30 and int(t) > 0:
            return int(t)
    return None


def _field(body: dict, *keys: str):
    """First present (non-empty) value among keys, stripped if a string."""
    for k in keys:
        v = body.get(k)
        if v is None or v == "":
            continue
        return v.strip() if isinstance(v, str) else v
    return None


def faucet_amount(body: dict) -> int:
    raw = body.get("amount")
    cap = faucet_max()
    if raw is None or raw == "":
        amt = min(FAUCET_DEFAULT, cap)
    else:
        amt = positive_int(raw)
        if amt is None:
            raise BadRequest(400, "amount must be a positive integer")
    if amt > cap:
        raise BadRequest(400, f"faucet amount above max {cap}")
    return amt


def check_request(action: str, body: dict) -> None:
    """Raise BadRequest before any handler work. 400 = malformed, 403 = not allowed."""
    if action == "pay":
        frm, to = _field(body, "from"), _field(body, "to")
        if not strict_name(frm):
            raise BadRequest(400, "from must be a PL account name")
        if not recipient_ok(to):
            raise BadRequest(400, "to must be a PL account name or address")
        if positive_int(body.get("amount")) is None:
            raise BadRequest(400, "amount must be a positive integer")
        if frm not in env_allowlist(PAY_FROM_ALLOW_ENV):
            raise BadRequest(403, "pay from this account is not enabled")
        return
    if action == "vault-activate":
        acct = _field(body, "account", "from")
        dest = _field(body, "destination", "to")
        if not strict_name(acct):
            raise BadRequest(400, "account must be a PL account name")
        if not recipient_ok(dest):
            raise BadRequest(400, "destination must be a PL account name or address")
        if acct not in env_allowlist(VAULT_ALLOW_ENV):
            raise BadRequest(403, "vault-activate for this account is not enabled")
        return
    if action == "faucet":
        to = _field(body, "to", "account")
        if not recipient_ok(to):
            raise BadRequest(400, "account must be a PL account name or address")
        faucet_amount(body)
        return
    if action in _NAME_FIELD_ACTIONS:
        for k in ("account", "from", "to", "name"):
            v = body.get(k)
            if v is None or v == "":
                continue
            # ASCII before lower(): e.g. KELVIN SIGN lower-cases to ASCII "k".
            if not isinstance(v, str) or not v.isascii() or not strict_name(v.strip().lower()):
                raise BadRequest(400, f"{k} must be a PL account name")


def rail_btc() -> dict:
    raw = ctl("status")
    i = raw.find("{")
    body = json.JSONDecoder().raw_decode(raw[i:])[0]
    body = body.get("body") or body
    for r in body.get("rails") or []:
        if r.get("asset") == "BTC":
            return r
    raise RuntimeError("no BTC rail")


def _load_abort_seed() -> bytes:
    raw = open(ABORT_SEED, "rb").read()
    if len(raw) == 32:
        return raw
    txt = raw.decode("ascii", "ignore").strip().lower().replace("0x", "")
    if len(txt) >= 64:
        b = bytes.fromhex(txt[:64])
        if len(b) == 32:
            return b
    if len(raw) >= 32:
        return raw[:32]
    raise RuntimeError("bitvm-abort-seed missing")


def btc_note_id(account: str, amount: int, dest: str) -> str:
    return hashlib.sha256(f"wd:BTC:{account}:{amount}:{dest}".encode()).hexdigest()


def abort_commit_hex(account: str, amount: int, dest: str) -> str:
    seed = _load_abort_seed()
    note = btc_note_id(account, amount, dest)
    h = hashlib.sha256()
    h.update(b"fpl-bitvm-abort-v1|")
    h.update(seed)
    h.update(note.encode())
    pre = h.digest()
    d = os.path.join(KEYS, "bitvm-abort")
    os.makedirs(d, exist_ok=True)
    p = os.path.join(d, note)
    with open(p, "w", encoding="utf-8") as f:
        f.write(pre.hex() + "\n")
    os.chmod(p, 0o600)
    return hashlib.sha256(pre).hexdigest()


def instance_utxo() -> tuple[str, int, int]:
    import urllib.request

    addr = BITVM2_INSTANCE_ADDRESS
    if not addr:
        raise RuntimeError("BITVM2_INSTANCE_ADDRESS missing")
    req = urllib.request.Request(
        f"https://mempool.space/testnet/api/address/{addr}/utxo",
        headers={"User-Agent": "fpl-walletd-bitvm2/2.9.46"},
    )
    with urllib.request.urlopen(req, timeout=20) as resp:
        chain_utxos = json.loads(resp.read().decode())
    confirmed = [u for u in chain_utxos if (u.get("status") or {}).get("confirmed")]
    if not confirmed:
        raise RuntimeError("BitVM2 instance has no confirmed Bitcoin UTXO")
    u = max(confirmed, key=lambda x: int(x.get("value") or 0))
    return str(u["txid"]), int(u["vout"]), int(u["value"])


def btc_bitvm2_kickoff(account: str, amount: int, dest: str) -> dict:
    require_btc_kickoff_enabled()
    dest = dest.strip()
    if amount < BITVM2_MIN_OUT:
        raise RuntimeError(f"withdraw below min {BITVM2_MIN_OUT} sats")
    txid, vout, sats = instance_utxo()
    fee = BITVM2_KICKOFF_FEE
    if sats < amount + fee:
        raise RuntimeError(f"instance {sats} sats cannot Kickoff {amount}+{fee} fee")
    commit = abort_commit_hex(account, amount, dest)
    utxo = f"{txid}:{vout}:{sats}"
    raw = ctl(
        "btc-bitvm2-user-kickoff",
        "--utxo",
        utxo,
        "--claimer-secret-file",
        BITVM2_CLAIMER,
        "--setup-xonly",
        BITVM2_SETUP_XONLY,
        "--dest",
        dest,
        "--amount",
        str(amount),
        "--abort-commit-hex",
        commit,
        "--claim-csv",
        str(BITVM2_CLAIM_CSV),
        "--fee",
        str(fee),
        "--csv",
        str(BITVM2_CSV),
        "--assert-csv",
        str(BITVM2_ASSERT_CSV),
        timeout=30,
    ).strip().splitlines()[-1]
    if any(h in raw.lower() for h in OLD_BTC):
        raise RuntimeError("kickoff referenced a retired hold")
    return {
        "signed_btc_tx": raw,
        "mode": "bitvm2-dest-lock",
        "amount": amount,
        "change": sats - amount - fee,
        "utxo": utxo,
        "instance": BITVM2_INSTANCE_ADDRESS,
        "claim_csv": BITVM2_CLAIM_CSV,
        "fee": fee,
    }


def btc_dest_lock_take(
    account: str,
    dest: str,
    amount: int,
    dest_secret: str,
    prev_txid: str,
    vout: int,
    sats: int,
) -> dict:
    require_btc_kickoff_enabled()
    dest_secret = dest_secret.replace("0x", "").lower()
    if len(dest_secret) != 64:
        raise RuntimeError("destSecret must be 32-byte hex")
    fd, path = tempfile.mkstemp(prefix="btc-dest-", suffix=".privhex")
    try:
        os.write(fd, (dest_secret + "\n").encode())
        os.close(fd)
        os.chmod(path, 0o600)
        raw = ctl(
            "btc-dest-lock-take",
            "--privhex-file",
            path,
            "--prev-txid",
            prev_txid,
            "--vout",
            str(vout),
            "--sats",
            str(sats),
            "--dest",
            dest,
            "--from",
            account,
            "--amount",
            str(amount),
            "--abort-seed-file",
            ABORT_SEED,
            "--fee",
            str(BITVM2_TAKE_FEE),
            timeout=30,
        ).strip().splitlines()[-1]
    finally:
        try:
            os.remove(path)
        except OSError:
            pass
    return {"signed_btc_tx": raw, "mode": "dest-lock-take", "amount": amount}


def btc_kickoff(account: str, amount: int, dest: str) -> dict:
    require_btc_kickoff_enabled()
    if BTC_EXIT_MODE == "bitvm2":
        return btc_bitvm2_kickoff(account, amount, dest)
    dest = dest.strip()
    if any(h in dest.lower() for h in OLD_BTC):
        raise RuntimeError("retired BTC hold — refused")
    if amount < 10000:
        raise RuntimeError("withdraw below min 10000 sats")
    import fcntl

    os.makedirs(os.path.dirname(FROST_LOCK), exist_ok=True)
    with open(FROST_LOCK, "a+", encoding="utf-8") as lk:
        fcntl.flock(lk.fileno(), fcntl.LOCK_EX)
        rail = rail_btc()
        vp = rail.get("vault_pub") or {}
        vault_spk = str(vp.get("spk_hex") or "").strip()
        addr = str(rail.get("vault_address") or "").strip()
        if not vault_spk.startswith("5120") or not addr.startswith("tb1p"):
            raise RuntimeError("BTC vault is not FROST P2TR")
        if any(h in addr.lower() for h in OLD_BTC):
            raise RuntimeError("retired BTC hold — refused")
        import urllib.request
        req = urllib.request.Request(
            f"https://mempool.space/testnet/api/address/{addr}/utxo",
            headers={"User-Agent": "fpl-walletd-kickoff/2.9.36"},
        )
        with urllib.request.urlopen(req, timeout=20) as resp:
            chain_utxos = json.loads(resp.read().decode())
        confirmed = [u for u in chain_utxos if (u.get("status") or {}).get("confirmed")]
        if not confirmed:
            raise RuntimeError("vault has no confirmed Bitcoin UTXO — cannot Kickoff")
        u = max(confirmed, key=lambda x: int(x.get("value") or 0))
        utxo = f"{u['txid']}:{int(u['vout'])}:{int(u['value'])}"
        unsigned = json.loads(
            ctl(
                "btc-frost-kickoff-unsigned",
                "--utxo",
                utxo,
                "--dest",
                dest,
                "--amount",
                str(amount),
                "--fee",
                str(BTC_KICKOFF_FEE),
                "--vault-spk",
                vault_spk,
                "--from",
                account,
                "--abort-seed-file",
                ABORT_SEED,
                timeout=30,
            )
        )
        sighash = str(unsigned.get("sighash") or "")
        if len(sighash) != 64:
            raise RuntimeError("kickoff unsigned missing sighash")
        out = subprocess.run(
            ["bash", FROST_COORD, "sign-message", sighash],
            capture_output=True,
            text=True,
            timeout=180,
        )
        if out.returncode != 0:
            raise RuntimeError((out.stderr or out.stdout or "frost coord failed")[-400:])
        lines = [ln.strip() for ln in out.stdout.splitlines() if ln.strip()]
        sig = lines[-1] if lines else ""
        if len(sig) != 128:
            raise RuntimeError("frost coord did not print 64-byte sig")
        raw = ctl(
            "btc-frost-kickoff-finish",
            "--utxo",
            utxo,
            "--dest",
            dest,
            "--amount",
            str(amount),
            "--fee",
            str(BTC_KICKOFF_FEE),
            "--vault-spk",
            vault_spk,
            "--from",
            account,
            "--abort-seed-file",
            ABORT_SEED,
            "--sig-hex",
            sig,
            timeout=30,
        ).strip().splitlines()[-1]
        if any(h in raw.lower() for h in OLD_BTC):
            raise RuntimeError("kickoff referenced a retired hold")
        return {
            "signed_btc_tx": raw,
            "note_id": unsigned.get("note_id"),
            "claim_spk": unsigned.get("claim_spk"),
            "amount": unsigned.get("amount"),
            "change": unsigned.get("change"),
            "utxo": utxo,
        }


def ctl(*args: str, timeout: int = 30) -> str:
    cmd = [CTL, "--addr", ADDR, *args]
    p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if p.returncode != 0:
        raise RuntimeError((p.stderr or p.stdout or f"exit {p.returncode}").strip())
    return p.stdout


def account(name: str) -> dict:
    raw = ctl("account", "--account", name)
    d = json.loads(raw)
    return d.get("body") or d


def tx_id_of(out: str) -> str:
    for part in out.split():
        if part.startswith("tx_id="):
            return part.split("=", 1)[1]
    return ""


def _mint_json(args: list, timeout: int = 45) -> dict:
    p = subprocess.run(
        [sys.executable, MINTD, *args],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    blob = (p.stdout or "") + (p.stderr or "")
    last = None
    i = blob.find("{")
    while i >= 0:
        try:
            last, end = json.JSONDecoder().raw_decode(blob[i:])
            i = blob.find("{", i + end)
        except json.JSONDecodeError:
            i = blob.find("{", i + 1)
    if p.returncode != 0 and not last:
        raise RuntimeError((p.stderr or p.stdout or f"exit {p.returncode}").strip()[-500:])
    if last is None:
        raise RuntimeError(blob[-400:] or "mint produced no json")
    return last


def mint_queue(account: str, txid: str, asset: str = "") -> dict:
    args = ["--queue", "--account", account, "--txid", txid]
    if asset:
        args += ["--asset", asset]
    rec = _mint_json(args, timeout=50)
    if rec.get("status") in ("queued", "error"):
        extra = ["--once", "--account", account, "--txid", txid]
        if asset:
            extra += ["--asset", asset]
        os.makedirs(os.path.dirname(MINT_KICK_LOG), exist_ok=True)
        logf = open(MINT_KICK_LOG, "a")
        subprocess.Popen(
            [sys.executable, MINTD, *extra],
            stdout=logf,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    return rec


def mint_status(account: str, txid: str) -> dict:
    args = ["--status", "--txid", txid]
    if account:
        args += ["--account", account]
    return _mint_json(args, timeout=15)


def _eth_pk() -> str:
    raw = open(ETH_PK_FILE, encoding="utf-8").read().strip()
    return raw if raw.startswith("0x") else "0x" + raw


OPEN_CLAIM_SIG = "openClaim(bytes32,address,uint256,bool,bytes32[],uint32,uint64)"
# F4 (V3_TEST_PLAN): measured openClaim use is 650-670k gas.
OPEN_CLAIM_GAS_BUFFER_NUM, OPEN_CLAIM_GAS_BUFFER_DEN = 5, 4  # x1.25
OPEN_CLAIM_GAS_MAX = 2_000_000


def _env_int(name: str, default: int, lo: int, hi: int) -> int:
    try:
        v = int(str(os.environ.get(name, "")).strip() or default)
    except ValueError:
        return default
    return v if lo <= v <= hi else default


def _open_claim_gas(bridge: str, call_args: list) -> tuple:
    """(gas_limit, gas_price_wei) for an openClaim send.

    gas_limit: eth_estimateGas (cast estimate --from the claim key) x1.25, at most
    OPEN_CLAIM_GAS_MAX. If no RPC can estimate (RPC down, no key address) use
    FPL_OPEN_CLAIM_GAS (default 850000). An estimate that reverts raises: the send
    would revert too (e.g. "exists", "header", "pool").
    gas_price: 2x eth_gasPrice, clamped to [FPL_ETH_GAS_PRICE_FLOOR_WEI (default
    0.1 gwei), FPL_ETH_GAS_PRICE_MAX_WEI (default 2 gwei, the old fixed price)].
    """
    fallback = _env_int("FPL_OPEN_CLAIM_GAS", 850_000, 700_000, OPEN_CLAIM_GAS_MAX)
    floor = _env_int("FPL_ETH_GAS_PRICE_FLOOR_WEI", 100_000_000, 1, 10**12)
    cap = _env_int("FPL_ETH_GAS_PRICE_MAX_WEI", 2_000_000_000, 1, 10**12)
    if floor > cap:
        floor = cap
    sender = ""
    try:
        a = subprocess.run(
            [CAST, "wallet", "address", "--private-key", _eth_pk()],
            capture_output=True, text=True, timeout=30,
        )
        if a.returncode == 0 and a.stdout.strip().startswith("0x"):
            sender = a.stdout.strip().split()[0]
    except (OSError, subprocess.SubprocessError):
        sender = ""
    gas_limit = 0
    gas_price = 0
    for rpc in ETH_RPCS or [ETH_RPC]:
        if sender and not gas_limit:
            e = subprocess.run(
                [CAST, "estimate", bridge, OPEN_CLAIM_SIG, *call_args, "--from", sender, "--rpc-url", rpc],
                capture_output=True, text=True, timeout=60,
            )
            out = (e.stdout or "").strip()
            if e.returncode == 0 and out.split() and out.split()[0].isdigit():
                est = int(out.split()[0])
                gas_limit = min(OPEN_CLAIM_GAS_MAX, est * OPEN_CLAIM_GAS_BUFFER_NUM // OPEN_CLAIM_GAS_BUFFER_DEN)
                if est > OPEN_CLAIM_GAS_MAX:
                    raise RuntimeError(f"openClaim estimate {est} gas is above {OPEN_CLAIM_GAS_MAX}; not sending")
            elif "revert" in ((e.stderr or "") + out).lower():
                raise RuntimeError("openClaim would revert: " + ((e.stderr or "") + out)[-300:])
        if not gas_price:
            g = subprocess.run([CAST, "gas-price", "--rpc-url", rpc], capture_output=True, text=True, timeout=30)
            gp = (g.stdout or "").strip()
            if g.returncode == 0 and gp.split() and gp.split()[0].isdigit():
                gas_price = int(gp.split()[0]) * 2
        if gas_limit and gas_price:
            break
    if not gas_limit:
        gas_limit = fallback
    gas_price = min(cap, max(floor, gas_price or cap))
    return gas_limit, gas_price


# --- live-only helpers from host1 (10-09), preserved on M-1 deploy ---
def _parse_uint(raw: str) -> int:
    s = (raw or "").strip().split()[0] if (raw or "").strip() else "0"
    if s.startswith("0x"):
        return int(s, 16)
    return int(s, 10)


def _tx_hash(blob: str) -> str:
    found = [t.strip('",') for t in (blob or "").replace("=", " ").split() if t.startswith("0x") and len(t.strip('",')) == 66]
    return found[-1] if found else ""


def eth_fpl_tip() -> int:
    for rpc in ETH_RPCS:
        try:
            out = subprocess.check_output(
                [CAST, "call", V2_BRIDGE, "fplTip()(uint64)", "--rpc-url", rpc],
                text=True,
                timeout=20,
            )
            n = _parse_uint(out)
            if n > 0:
                return n
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError, ValueError):
            continue
    return 0


def on_chain_claim_root(height: int) -> str:
    sig = "headers(uint64)(bytes32,bytes32,bytes32,bytes32,bytes32,bool)"
    for rpc in ETH_RPCS:
        try:
            out = subprocess.check_output(
                [CAST, "call", V2_BRIDGE, sig, str(height), "--rpc-url", rpc],
                text=True,
                timeout=20,
            ).strip()
            parts = [p.strip().strip(",") for p in out.replace("(", " ").replace(")", " ").split() if p.startswith("0x") and len(p.strip(",")) == 66]
            # hash, parent, inner, claimRoot, next, finalized
            if len(parts) >= 4:
                return parts[3].lower()
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired, OSError):
            continue
    return ""



def _note_external_to(note_id: str) -> str:
    """external_to of the packed rail withdrawal with this note id (live behaviour)."""
    note = note_id.lower().removeprefix("0x")
    raw = subprocess.check_output([CTL, "--addr", ADDR, "status"], text=True, timeout=20)
    i = raw.find("{")
    body = json.JSONDecoder().raw_decode(raw[i:])[0]
    st = body.get("body") or body
    for rail in st.get("rails") or []:
        if not isinstance(rail, dict):
            continue
        for w in rail.get("withdrawals") or []:
            if not isinstance(w, dict):
                continue
            nid = str(w.get("note_id") or "").lower().removeprefix("0x")
            if nid == note:
                dest = str(w.get("external_to") or "").strip()
                if dest.startswith("0x") and len(dest) == 42:
                    return dest
                h = dest.lower().removeprefix("0x")
                if len(h) == 40:
                    return "0x" + h
    raise RuntimeError("noteId not in packed rail withdrawals — Kickoff refused")


class KickoffMismatch(ValueError):
    """Kickoff request does not exactly match its burn note; refused before any send."""


def _note_amount_asset(note_id: str) -> tuple[int, str]:
    """(amount, rail asset) of the packed rail withdrawal with this note id. Fail closed:
    the note must appear exactly once, on an ETH or USDC rail, with a positive integer amount."""
    note = note_id.lower().removeprefix("0x")
    raw = subprocess.check_output([CTL, "--addr", ADDR, "status"], text=True, timeout=20)
    i = raw.find("{")
    body = json.JSONDecoder().raw_decode(raw[i:])[0]
    st = body.get("body") or body
    hits: list[tuple[int, str]] = []
    for rail in st.get("rails") or []:
        if not isinstance(rail, dict):
            continue
        asset = str(rail.get("asset") or "").strip().upper()
        for w in rail.get("withdrawals") or []:
            if not isinstance(w, dict):
                continue
            nid = str(w.get("note_id") or "").lower().removeprefix("0x")
            if nid != note:
                continue
            amt = w.get("amount")
            if isinstance(amt, bool) or not isinstance(amt, (int, str)):
                raise KickoffMismatch("burn note amount unreadable — Kickoff refused")
            try:
                n = int(str(amt).strip())
            except ValueError:
                raise KickoffMismatch("burn note amount unreadable — Kickoff refused")
            hits.append((n, asset))
    if len(hits) != 1:
        raise KickoffMismatch("burn note not uniquely found — Kickoff refused")
    n, asset = hits[0]
    if asset not in ("ETH", "USDC") or n < 1:
        raise KickoffMismatch("burn note is not a positive ETH/USDC withdrawal — Kickoff refused")
    return n, asset


def eth_kickoff(note_id: str, dest: str, amount: int, usdc: bool) -> dict:
    if os.environ.get("DESTLOCK_EXIT", "") != "1":
        raise RuntimeError("claimer Kickoff is leftover-only; DESTLOCK_EXIT=1 required")
    note = note_id.lower().removeprefix("0x")
    if len(note) != 64:
        raise RuntimeError("noteId must be 32-byte hex")
    dest = dest.strip()
    if not dest.startswith("0x") or len(dest) != 42:
        raise RuntimeError("dest must be a 20-byte 0x address")
    if amount < 1:
        raise RuntimeError("amount must be positive")
    leaf_dest = _note_external_to(note)
    if dest.lower() != leaf_dest.lower():
        raise RuntimeError(f"Kickoff dest must be burn external_to {leaf_dest} (got {dest})")
    note_amount, note_asset = _note_amount_asset(note)
    want_asset = "USDC" if usdc else "ETH"
    if want_asset != note_asset:
        raise KickoffMismatch(f"Kickoff asset must be burn note asset {note_asset} (got {want_asset})")
    if int(amount) != note_amount:
        raise KickoffMismatch(f"Kickoff amount must be burn note amount {note_amount} (got {amount})")
    p = subprocess.run(
        [
            CAST,
            "send",
            SEPOLIA_BRIDGE,
            "kickoff(bytes32,address,uint256,bool)",
            "0x" + note,
            dest,
            str(amount),
            "true" if usdc else "false",
            "--rpc-url",
            ETH_RPC,
            "--private-key",
            _eth_pk(),
            "--legacy",
            "--gas-price",
            "2000000000",
            "--gas-limit",
            "200000",
        ],
        capture_output=True,
        text=True,
        timeout=180,
    )
    blob = (p.stdout or "") + (p.stderr or "")
    if p.returncode != 0:
        raise RuntimeError(blob[-500:] or "eth kickoff failed")
    tx = ""
    for part in blob.replace('"', " ").split():
        if part.startswith("0x") and len(part) == 66:
            tx = part
            break
    if not tx:
        raise RuntimeError("eth kickoff produced no tx hash")
    return {"ok": True, "tx": tx, "noteId": "0x" + note, "mode": "dest-lock-kickoff"}


def _cast_uint(sig: str, *args: str, bridge: str = "") -> int:
    """cast call `sig` on `bridge` (default V2), trying each RPC in ETH_RPCS. Pass the bridge
    explicitly so the tip, the header and the send in one request read the same contract."""
    p = None
    for rpc in ETH_RPCS or [ETH_RPC]:
        p = subprocess.run(
            [CAST, "call", bridge or QC_V2_BRIDGE, sig, *args, "--rpc-url", rpc],
            capture_output=True,
            text=True,
            timeout=30,
        )
        if p.returncode == 0:
            break
    if p is None or p.returncode != 0:
        raise RuntimeError(((p.stderr or p.stdout) if p else "")[-300:] or "cast call failed")
    raw = (p.stdout or "").strip().split()[0]
    return int(raw, 16) if raw.startswith("0x") else int(raw or "0")


HEADER_SIG = "headers(uint64)(bytes32,bytes32,bytes32,bytes32,bytes32,bool)"
# FalconQcBridgeV2 and V3 Claim getter (same 8 fields: dest, amount, usdc, readyBlock,
# fplHeight, leaf, open, taken); walletd uses `dest` and `taken`.
CLAIMS_SIG = "claims(bytes32)(address,uint256,bool,uint64,uint64,bytes32,bool,bool)"
ZERO_ADDR = "0x" + "00" * 20
WAITING_HEADER = "waiting for bridge header"
NODE_BEHIND = "local node behind bridge header"


def _parse_qc_header(out: str) -> dict:
    """cast output of FalconQcBridgeV2/V3 headers(h): hash, parent, innerHash, claimRoot,
    nextCommitteeRoot, finalized (same FplHeader layout in both contracts)."""
    words = re.findall(r"0x[0-9a-fA-F]{64}", out or "")
    flags = re.findall(r"\b(true|false)\b", out or "")
    if len(words) != 5 or len(flags) != 1:
        raise RuntimeError(f"unexpected headers() output: {(out or '').strip()[:200]}")
    return {
        "hash": words[0].lower(),
        "parent": words[1].lower(),
        "innerHash": words[2].lower(),
        "claimRoot": words[3].lower(),
        "nextCommitteeRoot": words[4].lower(),
        "finalized": flags[0] == "true",
    }


def _qc_header(height: int, bridge: str = "") -> dict:
    """Read headers[height] from a FalconQc bridge (V2 and V3 have the same getter)."""
    p = None
    for rpc in ETH_RPCS or [ETH_RPC]:
        p = subprocess.run(
            [CAST, "call", bridge or QC_V2_BRIDGE, HEADER_SIG, str(height), "--rpc-url", rpc],
            capture_output=True,
            text=True,
            timeout=30,
        )
        if p.returncode == 0:
            break
    if p is None or p.returncode != 0:
        raise RuntimeError(((p.stderr or p.stdout) if p else "")[-300:] or "cast call headers failed")
    return _parse_qc_header(p.stdout or "")


def _claim_state(bridge: str, note: str) -> dict:
    """claims(note) on a FalconQc bridge: {"dest": lowercase 0x address, "taken": bool}.
    dest == ZERO_ADDR means never opened there. The getter has three bools (usdc, open,
    taken), in that order; anything else is refused."""
    p = subprocess.run(
        [CAST, "call", bridge, CLAIMS_SIG, note, "--rpc-url", ETH_RPC],
        capture_output=True,
        text=True,
        timeout=30,
    )
    if p.returncode != 0:
        raise RuntimeError((p.stderr or p.stdout or "cast call claims failed")[-300:])
    out = p.stdout or ""
    first = (out.strip().split() or [""])[0]
    flags = re.findall(r"\b(true|false)\b", out)
    if not EVM_ADDR_RE.match(first) or len(flags) != 3:
        raise RuntimeError(f"unexpected claims() output: {out.strip()[:200]}")
    return {"dest": first.lower(), "taken": flags[2] == "true"}


def _claim_dest(bridge: str, note: str) -> str:
    """claims(note).dest on a FalconQc bridge, lowercase; ZERO_ADDR means never opened there."""
    return _claim_state(bridge, note)["dest"]


def _v3_anchor() -> int:
    """FALCON_QC_V3_ANCHOR_HEIGHT as a positive int, or RuntimeError (no claim opens then)."""
    raw = QC_V3_ANCHOR_HEIGHT
    if not raw.isdigit() or int(raw) < 1:
        raise RuntimeError(
            "FALCON_QC_V3_BRIDGE is set but FALCON_QC_V3_ANCHOR_HEIGHT is not a positive height; "
            "not opening on V2"
        )
    return int(raw)


# Startup/first-claim check (FM nit 4 on #38): V3 headers(anchor).hash must equal V2
# headers(anchor).hash, i.e. FALCON_QC_V3_ANCHOR_HEIGHT really is the header the V3 constructor
# copied from this V2. Read-only. "ok" is cached; "mismatch" is sticky (config error, restart
# with a fixed config); a read error is retried on the next claim. Until "ok", no claim opens
# while both bridges are set.
_ANCHOR_CHECK = {"state": ""}
_ANCHOR_CHECK_LOCK = threading.Lock()


def _check_v3_anchor() -> None:
    """Raise unless V3 headers(anchor).hash == V2 headers(anchor).hash (both non-zero).
    No-op unless both bridges are set."""
    v2 = QC_V2_BRIDGE if QC_V2_LIVE else ""
    v3 = QC_V3_BRIDGE
    if not (v2 and v3):
        return
    with _ANCHOR_CHECK_LOCK:
        state = _ANCHOR_CHECK["state"]
        if state == "ok":
            return
        if state.startswith("mismatch"):
            raise RuntimeError(state + "; not opening claims")
        anchor = _v3_anchor()
        try:
            h3 = _qc_header(anchor, v3)["hash"]
            h2 = _qc_header(anchor, v2)["hash"]
        except Exception as e:
            raise RuntimeError(f"V3 anchor check could not read headers({anchor}): {e}; not opening claims")
        zero = "0x" + "00" * 32
        if h3 == zero or h2 == zero or h3 != h2:
            _ANCHOR_CHECK["state"] = (
                f"mismatch: V3 headers({anchor}).hash {h3} != V2 headers({anchor}).hash {h2} "
                "(check FALCON_QC_V3_ANCHOR_HEIGHT and FALCON_QC_V3_BRIDGE)"
            )
            raise RuntimeError(_ANCHOR_CHECK["state"] + "; not opening claims")
        _ANCHOR_CHECK["state"] = "ok"


def _startup_anchor_check() -> None:
    """Run the anchor check once at startup and log the result. A failure does not stop
    walletd (other actions keep working); claims stay refused until the check passes."""
    try:
        _check_v3_anchor()
    except Exception as e:
        print(f"walletd: V3 anchor check failed, eth-open-claim refused: {e}", file=sys.stderr, flush=True)


def v3_refund_note(deposit_id: str) -> str:
    """0x + sha256("refund|" || depositId): the V3 refund note FPL appends when a deposit
    resolves as a refund (bridge::v3_refund_note_id, FalconQcBridgeV3.refundNoteId)."""
    b = bytes.fromhex(deposit_id.strip().lower().removeprefix("0x"))
    if len(b) != 32:
        raise ValueError("depositId must be 32-byte hex")
    return "0x" + hashlib.sha256(b"refund|" + b).hexdigest()


def _refund_note_from_body(body: dict) -> tuple[str, str]:
    """(depositId, refund note) from a request, or ("", "") without depositId.
    Raises ValueError on a malformed id or a noteId that is not that deposit's refund note."""
    dep = str(body.get("depositId") or body.get("deposit_id") or "").strip()
    if not dep:
        return "", ""
    try:
        note = v3_refund_note(dep)
    except ValueError:
        raise ValueError("depositId must be 32-byte hex")
    given = str(body.get("noteId") or body.get("note") or "").strip().lower().removeprefix("0x")
    if given and "0x" + given != note:
        raise ValueError("noteId is not the refund note of depositId")
    return dep, note


def _withdraw_leaf(note_hex: str, dest: str, amount: int, usdc: bool) -> bytes:
    """Same bytes as FalconQcBridgeV2/V3 withdrawLeaf."""
    buf = (
        b"evm-wd-v1|"
        + bytes.fromhex(note_hex.lower().removeprefix("0x"))
        + bytes.fromhex(dest.lower().removeprefix("0x"))
        + int(amount).to_bytes(8, "big")
        + (b"\x01" if usdc else b"\x00")
    )
    return hashlib.sha256(buf).digest()


def _merkle_ok(leaf: bytes, proof: list, index: int, root_hex: str) -> bool:
    """Same fold as the contract's _merkleSha256(leaf, proof, index) == claimRoot."""
    if index < 0 or index >= 2**32:
        return False
    h = leaf
    idx = index
    for sib in proof:
        p = bytes.fromhex(str(sib).lower().removeprefix("0x"))
        if len(p) != 32:
            return False
        h = hashlib.sha256(h + p).digest() if idx % 2 == 0 else hashlib.sha256(p + h).digest()
        idx //= 2
    return h.hex() == root_hex.lower().removeprefix("0x")


def _claim_on(
    bridge: str, version: str, note_id: str, dest: str, amount: int, usdc: bool, account: str, tip: int | None = None
) -> dict:
    """Read fplTip (unless given) and headers[fplTip] from `bridge` and build the proof against
    that header's claimRoot. Every chain read uses `bridge`; nothing is sent."""
    if tip is None:
        tip = _cast_uint("fplTip()(uint64)", bridge=bridge)
    if tip < 1:
        raise RuntimeError(f"{version.upper()} headers not moving yet (fplTip=0)")
    hdr = _qc_header(tip, bridge)
    if not hdr["finalized"]:
        raise RuntimeError(f"{version.upper()} header {tip} is not finalized")
    rec = claim_proof(
        account,
        "USDC" if usdc else "ETH",
        dest,
        amount,
        note_id,
        fpl_height=tip,
        header_claim_root=hdr["claimRoot"],
    )
    return {"bridge": bridge, "version": version, "tip": tip, "header": hdr, "rec": rec}


def _pick_claim_bridge(note_id: str, dest: str, amount: int, usdc: bool, account: str) -> dict:
    """Pick the bridge for this request once, then use only that address.

    Routing by note height: a note that headers[V2 fplTip] already commits is opened on V2.
    A note past the V2 header (the proof tool answers "waiting") goes to V3 when
    QC_V3_BRIDGE is set; V3 anchors at the V2 tip and its later headers commit the
    cumulative claim tree. With QC_V3_BRIDGE unset, the V2 "waiting" answer stands (409).
    With V2 unset and V3 set, every note goes to V3.

    With both set, V2 is used only while V2 fplTip <= the V3 anchor (FALCON_QC_V3_ANCHOR_HEIGHT);
    past it every note goes to V3. eth_open_claim re-checks this, and V3 claims(note), under
    WRITE_LOCK right before the send (V2 cannot see V3, so a note V3 holds must not open on V2)."""
    v2 = QC_V2_BRIDGE if QC_V2_LIVE else ""
    v3 = QC_V3_BRIDGE
    if not v2 and not v3:
        raise RuntimeError(
            "no claim bridge configured: set FALCON_QC_V2_BRIDGE and/or FALCON_QC_V3_BRIDGE "
            "(V3 alone routes every note to V3)"
        )
    for name, addr in (("FALCON_QC_V2_BRIDGE", v2), ("FALCON_QC_V3_BRIDGE", v3)):
        if addr and (not EVM_ADDR_RE.match(addr) or addr.lower() == ZERO_ADDR):
            raise RuntimeError(f"{name} is not a non-zero 20-byte 0x address; not routing claims")
    if v2 and v3 and v2.lower() == v3.lower():
        raise RuntimeError("FALCON_QC_V3_BRIDGE equals the V2 bridge; not routing claims")
    anchor = _v3_anchor() if (v2 and v3) else None
    if anchor is not None:
        _check_v3_anchor()
    if v2:
        v2_tip = _cast_uint("fplTip()(uint64)", bridge=v2)
        if anchor is not None and v2_tip > anchor:
            # V2 headers moved past the V3 anchor: V2 is closed for claims (FM ruling on #38).
            return _claim_on(v3, "v3", note_id, dest, amount, usdc, account)
        got = _claim_on(v2, "v2", note_id, dest, amount, usdc, account, tip=v2_tip)
        # Only "header does not commit this note yet" moves to V3. A local node that is behind
        # (node_behind_bridge_header) or a ready proof stays on V2.
        if got["rec"].get("status") != "waiting_for_bridge_header" or not v3:
            return got
    return _claim_on(v3, "v3", note_id, dest, amount, usdc, account)


def eth_open_claim(note_id: str, dest: str, amount: int, usdc: bool, account: str) -> dict:
    """Open a claim against headers[fplTip] of the bridge picked for this note (V2 or V3, see
    _pick_claim_bridge). The proof is built from the leaves that header's claimRoot commits
    and checked locally before cast send. A note no header covers yet returns
    {"waiting": True, ...} (HTTP 409) and nothing is sent.

    openClaim is permissionless on V2 and V3: walletd's hot account only pays gas. Payout is
    `take(noteId)`, which only the recorded dest may call, so walletd never sends it."""
    # Admission before any RPC/subprocess; released before the send (which is under WRITE_LOCK).
    with _proof_slot():
        sel = _pick_claim_bridge(note_id, dest, amount, usdc, account)
    bridge, version, tip, hdr, rec = sel["bridge"], sel["version"], sel["tip"], sel["header"], sel["rec"]
    note = rec.get("noteId") or ("0x" + note_id.lower().removeprefix("0x"))
    # A V3 refund request may name only the deposit; dest and amount then come from the
    # node's note (and are still checked against the header root below).
    if rec.get("kind") == "v3_refund":
        if not dest and rec.get("dest"):
            dest = str(rec["dest"])
        if amount < 1 and rec.get("amount"):
            amount = int(rec["amount"])
    if rec.get("waiting"):
        status = rec.get("status") or "waiting_for_bridge_header"
        msg = NODE_BEHIND if status == "node_behind_bridge_header" else WAITING_HEADER
        return {
            "ok": False,
            "waiting": True,
            "retryable": True,
            "status": status,
            "message": msg,
            "error": msg,
            "detail": rec.get("message"),
            "noteId": note,
            "fplHeight": tip,
            "bridge": bridge,
            "bridgeVersion": version,
            "committedLeaves": rec.get("committedLeaves"),
            "noteLeaf": rec.get("currentIndex"),
            **({"kind": rec["kind"], "depositId": rec.get("depositId")} if rec.get("kind") else {}),
        }
    if not rec.get("ready"):
        raise RuntimeError(f"claim proof not ready for {version.upper()} header {tip}")
    proof = rec.get("proof") or []
    idx = int(rec.get("index") or 0)
    # Check what the contract will check: withdrawLeaf over the exact send args.
    leaf = _withdraw_leaf(note, dest, amount, usdc)
    if not _merkle_ok(leaf, proof, idx, hdr["claimRoot"]):
        raise RuntimeError(
            f"claim proof does not verify against headers[{tip}].claimRoot {hdr['claimRoot']} "
            f"({version.upper()} {bridge}); not sending"
        )
    # Serialised with the other state-changing actions (same hot account, so no nonce race
    # with eth-kickoff). The proof search above ran outside the lock; the double-pay checks
    # run inside it, right before the send, so no other walletd open can slip in between.
    with WRITE_LOCK:
        held = _open_guard(bridge, version, note)
        if held is not None:
            # Already open on this bridge: nothing sent. `taken` tells the site whether
            # take() was already paid (take() would revert "state"); `dest` is the only
            # address take() accepts.
            return {
                "ok": True,
                "tx": "",
                "alreadyOpen": True,
                "taken": held["taken"],
                "dest": held["dest"],
                "noteId": note,
                "fplHeight": tip,
                "bridge": bridge,
                "bridgeVersion": version,
                "mode": f"qc-{version}-open",
                **({"kind": rec["kind"], "depositId": rec.get("depositId")} if rec.get("kind") else {}),
            }
        p = _send_open_claim(bridge, note, dest, amount, usdc, proof, idx, tip)
    blob = (p.stdout or "") + (p.stderr or "")
    if p.returncode != 0:
        raise RuntimeError(blob[-500:] or "eth openClaim failed")
    tx = ""
    for part in blob.replace('"', " ").split():
        if part.startswith("0x") and len(part) == 66:
            tx = part
            break
    if not tx:
        raise RuntimeError("eth openClaim produced no tx hash")
    return {
        "ok": True,
        "tx": tx,
        "noteId": note,
        "fplHeight": tip,
        "bridge": bridge,
        "bridgeVersion": version,
        "mode": f"qc-{version}-open",
        **({"kind": rec["kind"], "depositId": rec.get("depositId")} if rec.get("kind") else {}),
    }


def _open_guard(bridge: str, version: str, note: str) -> dict | None:
    """Last checks before openClaim, called under WRITE_LOCK. Returns `bridge`'s claims(note)
    state {"dest", "taken"} when `note` is already open there (nothing to send; this also
    answers the second of two racing requests instead of a reverted "exists" tx), else None.
    Raises when opening would pay the note twice:
      - V2 with V3 set: V2 fplTip must still be <= the V3 anchor, and V3 claims(note).dest
        must be zero. FalconQcBridgeV2 has no V3 check of its own.
      - V3 with V2 set: V2 claims(note).dest must be zero (V3 also reverts "paid-v2")."""
    v2 = QC_V2_BRIDGE if QC_V2_LIVE else ""
    v3 = QC_V3_BRIDGE
    if version == "v2" and v3:
        anchor = _v3_anchor()
        t = _cast_uint("fplTip()(uint64)", bridge=bridge)
        if t > anchor:
            raise RuntimeError(
                f"V2 fplTip {t} is past the V3 anchor {anchor}: V2 is closed for claims; not opening on V2"
            )
        if _claim_dest(v3, note) != ZERO_ADDR:
            raise RuntimeError(f"note {note} is already opened on V3 {v3}; not opening on V2")
    if version == "v3" and v2 and _claim_dest(v2, note) != ZERO_ADDR:
        raise RuntimeError(f"note {note} is already opened on V2 {v2}; not opening on V3")
    st = _claim_state(bridge, note)
    return st if st["dest"] != ZERO_ADDR else None


def _send_open_claim(
    bridge: str, note: str, dest: str, amount: int, usdc: bool, proof: list, idx: int, tip: int
) -> subprocess.CompletedProcess:
    args = [note, dest, str(amount), "true" if usdc else "false", "[" + ",".join(proof) + "]", str(idx), str(tip)]
    # F4 (#63): limit from eth_estimateGas x1.25, price 2x network in [floor, cap]; a reverting
    # estimate raises before any send.
    gas_limit, gas_price = _open_claim_gas(bridge, args)
    # Retry on another RPC only when cast exits non-zero (live behaviour). A resend of an
    # already-opened claim reverts on-chain (gas only).
    p = None
    for rpc in ETH_RPCS or [ETH_RPC]:
        p = subprocess.run(
            [
                CAST,
                "send",
                bridge,
                OPEN_CLAIM_SIG,
                *args,
                "--rpc-url",
                rpc,
                "--private-key",
                _eth_pk(),
                "--legacy",
                "--gas-price",
                str(gas_price),
                "--gas-limit",
                str(gas_limit),
            ],
            capture_output=True,
            text=True,
            timeout=180,
        )
        if p.returncode == 0:
            break
    return p


def claim_proof(
    account: str,
    asset: str,
    dest: str,
    amount: int,
    note: str = "",
    fpl_height: int = 0,
    header_claim_root: str = "",
) -> dict:
    args = ["--account", account, "--asset", asset, "--dest", dest, "--amount", str(amount)]
    if note:
        args += ["--note", note]
    if fpl_height > 0:
        args += ["--fpl-height", str(fpl_height)]
    if header_claim_root:
        args += ["--header-claim-root", header_claim_root]
    with _proof_slot():
        p = subprocess.run(
            [sys.executable, CLAIMD, *args],
            capture_output=True,
            text=True,
            timeout=30,
        )
    blob = (p.stdout or "") + (p.stderr or "")
    last = None
    i = blob.find("{")
    while i >= 0:
        try:
            last, end = json.JSONDecoder().raw_decode(blob[i:])
            i = blob.find("{", i + end)
        except json.JSONDecodeError:
            i = blob.find("{", i + 1)
    if p.returncode != 0:
        err = ""
        if isinstance(last, dict):
            err = str(last.get("error") or "")
        raise RuntimeError(err or blob.strip()[-400:] or f"claim-proof exit {p.returncode}")
    if not isinstance(last, dict):
        raise RuntimeError("claim-proof produced no json")
    return last


# ─── M-1: listen address, shared-key auth, faucet limits ─────────────────────

# Live host1 units set FPL_WALLETD_HOST/FPL_WALLETD_PORT (10-09 live patch); honour them as fallbacks.
WALLETD_BIND = (os.environ.get("WALLETD_BIND") or os.environ.get("FPL_WALLETD_HOST") or "127.0.0.1").strip() or "127.0.0.1"
WALLETD_PORT = int(os.environ.get("WALLETD_PORT") or os.environ.get("FPL_WALLETD_PORT") or "8788")
WALLETD_KEY_FILE = os.environ.get(
    "WALLETD_KEY_FILE", "/home/falcon/.config/falcon-pl-walletd/shared.key"
)


def walletd_key() -> str:
    """The shared key, or "" when none is configured. Read per call so rotation needs no restart."""
    k = os.environ.get("WALLETD_API_KEY", "").strip()
    if k:
        return k
    if not WALLETD_KEY_FILE or not os.path.isfile(WALLETD_KEY_FILE):
        return ""
    try:
        with open(WALLETD_KEY_FILE, "r", encoding="utf-8") as f:
            return f.read().strip()
    except OSError:
        return ""


def is_loopback(host: str) -> bool:
    if host in ("localhost",):
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def check_bind_safe(host: str, key: str) -> None:
    if not is_loopback(host) and not key:
        raise SystemExit(
            f"refusing to listen on {host}: no WALLETD_API_KEY / WALLETD_KEY_FILE configured "
            "(bind 127.0.0.1 or set a key)"
        )
    if host in ("0.0.0.0", "::"):
        raise SystemExit("refusing to listen on all interfaces; set WALLETD_BIND to 127.0.0.1 or the tailnet IP")


def authorized(headers) -> bool:
    key = walletd_key()
    if not key:
        return True  # loopback-only mode (startup refuses non-loopback binds without a key)
    got = str((headers or {}).get("Authorization") or "")
    if not got.startswith("Bearer "):
        return False
    return hmac.compare_digest(got[7:].strip().encode(), key.encode())


FAUCET_STATE = os.environ.get(
    "FPL_FAUCET_STATE", "/home/falcon/falcon-pl-public-testnet-2300/run/faucet-recipients.json"
)
FAUCET_LOCK = threading.Lock()


def _int_env(name: str, default: int) -> int:
    try:
        v = int(os.environ.get(name, "") or default)
        return v if v > 0 else default
    except ValueError:
        return default


class FaucetLimiter:
    """Sliding-window drip counts per recipient plus a global hourly cap."""

    def __init__(self, path: str | None):
        self.path = path
        self.mem: dict | None = None

    def _load(self) -> dict:
        if self.mem is not None:
            return self.mem
        data: dict = {"r": {}, "g": []}
        if self.path:
            try:
                with open(self.path, "r", encoding="utf-8") as f:
                    raw = json.load(f)
                if isinstance(raw, dict) and isinstance(raw.get("r"), dict) and isinstance(raw.get("g"), list):
                    data = raw
            except (OSError, ValueError):
                pass
        self.mem = data
        return data

    def _save(self, data: dict) -> None:
        if not self.path:
            return
        try:
            d = os.path.dirname(self.path) or "."
            fd, tmp = tempfile.mkstemp(dir=d, prefix=".faucet-", suffix=".json")
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump(data, f)
            os.replace(tmp, self.path)
        except OSError as e:
            print(f"[walletd] faucet state not saved: {e}", file=sys.stderr)

    @staticmethod
    def _key(to: str) -> str:
        # Names are already validated lowercase; classic r-addresses are case-sensitive.
        return to.strip()

    def check(self, to: str, now: float | None = None) -> str | None:
        """Return an error string if this drip would exceed a limit, else None."""
        now = time.time() if now is None else now
        win = _int_env("FPL_FAUCET_RECIPIENT_WINDOW_S", 86_400)
        per = _int_env("FPL_FAUCET_RECIPIENT_MAX", 1)
        glob = _int_env("FPL_FAUCET_GLOBAL_MAX", 500)
        with FAUCET_LOCK:
            data = self._load()
            hits = [t for t in data["r"].get(self._key(to), []) if now - t < win]
            if len(hits) >= per:
                retry = int(win - (now - min(hits))) + 1
                return f"faucet limit for this recipient reached; retry in {retry}s"
            g = [t for t in data["g"] if now - t < 3600]
            if len(g) >= glob:
                return "faucet is busy (global limit); retry later"
        return None

    def record(self, to: str, now: float | None = None) -> None:
        now = time.time() if now is None else now
        win = _int_env("FPL_FAUCET_RECIPIENT_WINDOW_S", 86_400)
        with FAUCET_LOCK:
            data = self._load()
            data["r"] = {
                k: [t for t in v if now - t < win] for k, v in data["r"].items()
            }
            data["r"] = {k: v for k, v in data["r"].items() if v}
            data["r"].setdefault(self._key(to), []).append(now)
            data["g"] = [t for t in data["g"] if now - t < 3600] + [now]
            self._save(data)


FAUCET_LIMITER = FaucetLimiter(FAUCET_STATE)


class H(BaseHTTPRequestHandler):
    def _json(self, code: int, obj: dict) -> None:
        b = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_OPTIONS(self) -> None:
        # Server-to-server API (gateway/site backend); browsers are not expected to call it.
        self.send_response(204)
        self.end_headers()

    def do_GET(self) -> None:
        from urllib.parse import parse_qs, urlparse

        path = urlparse(self.path).path.rstrip("/")
        if path in ("/health", "/healthz"):
            self._json(200, {"ok": True, "service": "falcon-pl-walletd"})
            return
        if not authorized(getattr(self, "headers", None)):
            self._json(401, {"ok": False, "error": "unauthorized"})
            return
        if path in ("/btc-deposit", "/btc-bridge"):
            addr = BITVM2_INSTANCE_ADDRESS
            self._json(
                200,
                {
                    "address": addr,
                    "spk": BITVM2_INSTANCE_SPK,
                    "mode": BTC_EXIT_MODE,
                    "note": "peg-in with FALC memo; 369k NUMS vault is not this address",
                },
            )
            return
        q = parse_qs(urlparse(self.path).query)
        name = (q.get("account") or [""])[0].strip()
        if not name:
            self._json(400, {"error": "missing account"})
            return
        if not recipient_ok(name):
            self._json(400, {"error": "account must be a PL account name or address"})
            return
        try:
            self._json(200, account(name))
        except Exception as e:
            self._json(503, {"error": str(e)})

    def do_POST(self) -> None:
        if not authorized(getattr(self, "headers", None)):
            self._json(401, {"ok": False, "error": "unauthorized"})
            return
        try:
            n = int(self.headers.get("Content-Length") or 0)
            if n < 0 or n > 1 << 20:
                raise ValueError("length")
            body = json.loads(self.rfile.read(n) or b"{}")
        except (ValueError, TypeError, UnicodeDecodeError):
            self._json(400, {"ok": False, "error": "invalid JSON body"})
            return
        if not isinstance(body, dict):
            self._json(400, {"ok": False, "error": "JSON body must be an object"})
            return
        action = body.get("action", "pay")
        if not isinstance(action, str):
            self._json(400, {"ok": False, "error": "action must be a string"})
            return
        action = action or "pay"

        # Kill switch first: no validation, key load, UTXO lookup or signing for a
        # BTC spend unless FPL_BTC_KICKOFF_ENABLED=1 (checked per request).
        if str(action).strip().lower() in BTC_SPEND_ACTIONS and not btc_kickoff_enabled():
            self._json(403, {"ok": False, "error": BTC_KICKOFF_DISABLED})
            return

        # #55: validate every account-like field and the pay / vault-activate allowlists
        # before any key path, ctl call or signing.
        try:
            check_request(action, body)
        except BadRequest as e:
            self._json(e.code, {"ok": False, "error": str(e)})
            return

        # `action` is a str here; read-only actions skip the write lock.
        if action in UNLOCKED_POST_ACTIONS:
            self._post(body, action)
            return
        with WRITE_LOCK:
            self._post(body, action)

    def _post(self, body: dict, action: str) -> None:

        if action == "vault-activate":
            acct = str(body.get("account") or body.get("from") or "").strip()
            dest = str(body.get("destination") or body.get("to") or "").strip()
            if not acct or not dest:
                self._json(400, {"error": "account and destination required"})
                return
            if acct == dest:
                self._json(400, {"error": "Nominate a different address than the vault itself"})
                return
            try:
                cur = account(acct)
                if str(cur.get("account_type") or "hot") == "vault" and cur.get("vault_locked"):
                    self._json(400, {"error": "Vault already locked"})
                    return
                seq0 = int(cur.get("sequence") or 0)
                open_out = ctl(
                    "vault-open",
                    "--account",
                    acct,
                    "--destination",
                    dest,
                    "--scheme",
                    SCHEME,
                    "--keys-dir",
                    KEYS,
                    "--network-id",
                    NID,
                    "--fee",
                    "2",
                )
                for _ in range(40):
                    time.sleep(0.25)
                    mid = account(acct)
                    if int(mid.get("sequence") or 0) > seq0:
                        break
                lock_out = ctl(
                    "vault-lock",
                    "--account",
                    acct,
                    "--scheme",
                    SCHEME,
                    "--keys-dir",
                    KEYS,
                    "--network-id",
                    NID,
                    "--fee",
                    "2",
                )
                after = account(acct)
                self._json(
                    200,
                    {
                        "ok": True,
                        "action": "vault-activate",
                        "account": acct,
                        "destination": dest,
                        "openTx": tx_id_of(open_out),
                        "lockTx": tx_id_of(lock_out),
                        "accountType": after.get("account_type"),
                        "allowlist": after.get("allowlist"),
                        "vaultLocked": after.get("vault_locked"),
                    },
                )
            except Exception as e:
                self._json(503, {"error": str(e)})
            return

        if action == "name-status":
            name = str(body.get("name") or "").strip().lower()
            if name in RESERVED_NAMES:
                self._json(200, {"name": name, "available": False, "status": "blocked", "fee": 0, "error": "System name"})
                return
            recs = _names_load()
            now = int(time.time() * 1000)
            recs = [r for r in recs if r.get("activated") or int(r.get("reservedUntil") or 0) > now]
            rec = next((r for r in recs if r.get("name") == name), None)
            fee = _name_fee(name)
            if not rec:
                self._json(200, {"name": name, "available": True, "status": "free", "fee": fee})
                return
            if rec.get("activated"):
                self._json(200, {"name": name, "available": False, "status": "activated", "fee": rec.get("fee", fee), "publicKey": rec.get("publicKey")})
                return
            self._json(200, {"name": name, "available": False, "status": "reserved", "fee": rec.get("fee", fee), "reservedUntil": rec.get("reservedUntil"), "publicKey": rec.get("publicKey")})
            return

        if action == "name-reserve":
            try:
                rec = _name_reserve(body)
                self._json(200, {"ok": True, **rec})
            except Exception as e:
                self._json(400, {"error": str(e)})
            return

        if action == "name-activate":
            try:
                rec = _name_activate(body)
                self._json(200, {"ok": True, **rec})
            except Exception as e:
                self._json(400, {"error": str(e)})
            return

        if action == "faucet":
            to = str(body.get("to") or body.get("account") or "").strip()
            try:
                amt = faucet_amount(body)
            except BadRequest as e:
                self._json(e.code, {"ok": False, "error": str(e)})
                return
            if not to or amt < 1:
                self._json(400, {"error": "account required"})
                return
            limited = FAUCET_LIMITER.check(to)
            if limited:
                self._json(429, {"ok": False, "error": limited})
                return
            try:
                kh = faucet_key_hash(to, body)
            except ValueError as e:
                self._json(400, {"ok": False, "error": str(e)})
                return
            try:
                out = ctl(
                    "faucet",
                    "--to",
                    to,
                    "--amount",
                    str(amt),
                    "--scheme",
                    SCHEME,
                    "--keys-dir",
                    KEYS,
                    "--network-id",
                    NID,
                    "--fee",
                    "2",
                    *(["--key-hash", kh] if kh else []),
                )
                FAUCET_LIMITER.record(to)
                self._json(200, {"ok": True, "raw": out[-400:], "txId": tx_id_of(out)})
            except Exception as e:
                self._json(503, {"error": str(e)})
            return

        if action == "claim-proof":
            acct = str(body.get("account") or "").strip().lower()
            dest = str(body.get("dest") or body.get("externalTo") or "").strip()
            asset = str(body.get("asset") or "ETH").strip().upper()
            note = str(body.get("noteId") or body.get("note") or "").strip()
            try:
                amount = int(str(body.get("amount") or "0").strip() or "0")
            except ValueError:
                self._json(400, {"error": "amount must be an integer (wei / USDC units)"})
                return
            try:
                dep, refund_note = _refund_note_from_body(body)
            except ValueError as e:
                self._json(400, {"error": str(e)})
                return
            if dep:
                # V3 refund: the note has no FPL account; dest/amount are optional filters.
                note = refund_note
            if (acct or not dep) and not ACCT_RE.match(acct):
                self._json(400, {"error": "account must be a PL name"})
                return
            if asset not in ("ETH", "USDC"):
                self._json(400, {"error": "asset must be ETH or USDC"})
                return
            if (dest or not dep) and (not dest.startswith("0x") or len(dest) != 42):
                self._json(400, {"error": "dest must be a 20-byte 0x address"})
                return
            # 0 is the refund sentinel only; negative is never valid (same as eth-open-claim).
            if amount < 0 or (amount < 1 and not dep):
                self._json(400, {"error": "amount must be positive"})
                return
            try:
                rec = claim_proof(acct, asset, dest, amount, note)
                self._json(200, rec)
            except ProofBusy as e:
                self._json(429, {"error": str(e), "status": "proof_busy", "retryable": True})
            except Exception as e:
                self._json(503, {"error": str(e)})
            return

        if action in ("mint-eth-deposit", "mint-status"):
            acct = str(body.get("account") or body.get("to") or "").strip().lower()
            txid = str(body.get("txHash") or body.get("txid") or "").strip()
            asset = str(body.get("asset") or "").strip().upper()
            if asset and asset not in ("ETH", "USDC"):
                self._json(400, {"error": "asset must be ETH or USDC"})
                return
            if not ACCT_RE.match(acct):
                self._json(400, {"error": "account must be a PL name"})
                return
            if not TXID_RE.match(txid):
                self._json(400, {"error": "txHash must be 32-byte hex"})
                return
            try:
                if action == "mint-status":
                    rec = mint_status(acct, txid)
                else:
                    rec = mint_queue(acct, txid, asset)
                self._json(200, {"ok": True, **rec})
            except Exception as e:
                self._json(503, {"error": str(e)})
            return

        if action == "eth-open-claim":
            note = str(body.get("noteId") or body.get("note") or "").strip()
            dest = str(body.get("dest") or body.get("externalTo") or "").strip()
            asset = str(body.get("asset") or "ETH").strip().upper()
            acct = str(body.get("account") or "").strip().lower()
            try:
                amount = int(str(body.get("amount") or "0").strip() or "0")
            except ValueError:
                self._json(400, {"error": "amount must be an integer"})
                return
            try:
                dep, refund_note = _refund_note_from_body(body)
            except ValueError as e:
                self._json(400, {"error": str(e)})
                return
            if dep:
                # V3 refund (F1, #40): anyone may open it (pays the deposit sender only);
                # no FPL account, and dest/amount default to the node's refund note.
                note = refund_note
            if acct and not ACCT_RE.match(acct):
                self._json(400, {"error": "account must be a PL name"})
                return
            if asset not in ("ETH", "USDC"):
                self._json(400, {"error": "asset must be ETH or USDC"})
                return
            if (dest or not dep) and (len(dest) != 42 or not dest.startswith("0x")):
                self._json(400, {"error": "dest must be a 20-byte 0x address"})
                return
            # 0 is the refund sentinel only ("take the amount from the refund note").
            if amount < 0 or (amount < 1 and not dep):
                self._json(400, {"error": "amount must be positive"})
                return
            if not acct and not dep:
                self._json(400, {"error": "account required"})
                return
            try:
                rec = eth_open_claim(note, dest, amount, asset == "USDC", acct)
                # 409 + waiting is what the site polls on (no red error, retry later). Covers
                # waiting_for_bridge_header and node_behind_bridge_header.
                self._json(409 if rec.get("waiting") else 200, rec)
            except ProofBusy as e:
                # Same 409 + waiting the site already polls on, with its own status.
                self._json(409, {"ok": False, "waiting": True, "retryable": True,
                                 "status": "proof_busy", "message": str(e), "error": str(e)})
            except Exception as e:
                self._json(503, {"error": str(e)})
            return

        if action == "eth-kickoff":
            if os.environ.get("DESTLOCK_EXIT", "") != "1":
                self._json(
                    403,
                    {"error": "claimer Kickoff is leftover-only; DESTLOCK_EXIT=1 required"},
                )
                return
            note = str(body.get("noteId") or body.get("note") or "").strip()
            dest = str(body.get("dest") or body.get("externalTo") or "").strip()
            asset = str(body.get("asset") or "ETH").strip().upper()
            try:
                amount = int(str(body.get("amount") or "0").strip() or "0")
            except ValueError:
                self._json(400, {"error": "amount must be an integer"})
                return
            if asset not in ("ETH", "USDC"):
                self._json(400, {"error": "asset must be ETH or USDC"})
                return
            if len(dest) != 42 or not dest.startswith("0x"):
                self._json(400, {"error": "dest must be a 20-byte 0x address"})
                return
            try:
                rec = eth_kickoff(note, dest, amount, asset == "USDC")
                self._json(200, rec)
            except KickoffMismatch as e:
                self._json(409, {"error": str(e)})
            except Exception as e:
                self._json(503, {"error": str(e)})
            return

        if action == "btc-kickoff":
            acct = str(body.get("account") or body.get("from") or "").strip().lower()
            dest = str(body.get("dest") or body.get("externalTo") or "").strip()
            try:
                amount = int(str(body.get("amount") or "0").strip() or "0")
            except ValueError:
                self._json(400, {"error": "amount must be an integer (sats)"})
                return
            if not ACCT_RE.match(acct):
                self._json(400, {"error": "account must be a PL name"})
                return
            if len(dest) < 26:
                self._json(400, {"error": "dest must be a Bitcoin address"})
                return
            if amount < BITVM2_MIN_OUT:
                self._json(400, {"error": f"amount must be ≥ {BITVM2_MIN_OUT} sats"})
                return
            try:
                rec = btc_kickoff(acct, amount, dest)
                self._json(200, {"ok": True, **rec})
            except BtcKickoffDisabled:
                self._json(403, {"ok": False, "error": BTC_KICKOFF_DISABLED})
            except Exception as e:
                self._json(503, {"error": str(e)})
            return

        if action == "btc-take":
            acct = str(body.get("account") or body.get("from") or "").strip().lower()
            dest = str(body.get("dest") or body.get("externalTo") or "").strip()
            dest_secret = str(body.get("destSecret") or "").replace("0x", "").lower()
            prev = str(body.get("prevTxid") or "").strip().lower()
            try:
                amount = int(str(body.get("amount") or "0").strip() or "0")
                vout = int(str(body.get("vout") or "0").strip() or "0")
                sats = int(str(body.get("sats") or amount).strip() or "0")
            except ValueError:
                self._json(400, {"error": "amount/vout/sats must be integers"})
                return
            if not ACCT_RE.match(acct):
                self._json(400, {"error": "account must be a PL name"})
                return
            if len(dest) < 26:
                self._json(400, {"error": "dest must be a Bitcoin address"})
                return
            if len(prev) != 64:
                self._json(400, {"error": "prevTxid must be 32-byte hex"})
                return
            try:
                rec = btc_dest_lock_take(acct, dest, amount, dest_secret, prev, vout, sats)
                self._json(200, {"ok": True, **rec})
            except BtcKickoffDisabled:
                self._json(403, {"ok": False, "error": BTC_KICKOFF_DISABLED})
            except Exception as e:
                self._json(503, {"error": str(e)})
            return

        if action != "pay":
            self._json(400, {"error": "unknown action"})
            return
        frm = str(body.get("from") or "").strip()
        to = str(body.get("to") or "").strip()
        amt = int(body.get("amount") or 0)
        if not frm or not to or amt < 1:
            self._json(400, {"error": "from, to, amount required"})
            return
        try:
            sender = account(frm)
            if str(sender.get("account_type") or "hot") == "vault":
                allow = [str(x) for x in (sender.get("allowlist") or [])]
                if to not in allow:
                    only = allow[0] if allow else "the nominated address"
                    self._json(400, {"error": f"Vault is locked — protocol will only pay {only}"})
                    return
            out = ctl(
                "pay",
                "--from",
                frm,
                "--to",
                to,
                "--amount",
                str(amt),
                "--scheme",
                SCHEME,
                "--keys-dir",
                KEYS,
                "--network-id",
                NID,
                "--fee",
                "2",
            )
            self._json(200, {"ok": True, "raw": out[-400:]})
        except Exception as e:
            self._json(503, {"error": str(e)})

    def log_message(self, fmt: str, *args: object) -> None:
        return


MAX_HANDLER_THREADS = max(2, int(os.environ.get("WALLETD_MAX_THREADS", "16") or "16"))


class BoundedThreadingHTTPServer(ThreadingHTTPServer):
    """ThreadingHTTPServer with at most MAX_HANDLER_THREADS live handler threads. When all are
    busy the accept loop waits (connections queue in the listen backlog, as they did on the
    old single-threaded server) instead of spawning more threads."""

    daemon_threads = True

    def __init__(self, *a, max_threads: int = MAX_HANDLER_THREADS, **k):
        self._slots = threading.BoundedSemaphore(max_threads)
        super().__init__(*a, **k)

    def process_request(self, request, client_address):
        self._slots.acquire()
        try:
            super().process_request(request, client_address)
        except BaseException:
            self._slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self._slots.release()


if __name__ == "__main__":
    _startup_anchor_check()
    check_bind_safe(WALLETD_BIND, walletd_key())
    print(f"[walletd] listening on {WALLETD_BIND}:{WALLETD_PORT} (auth {'on' if walletd_key() else 'off, loopback only'})", file=sys.stderr)
    BoundedThreadingHTTPServer((WALLETD_BIND, WALLETD_PORT), H).serve_forever()
