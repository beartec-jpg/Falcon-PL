#!/usr/bin/env python3
"""FalconQcBridgeV3 prover. Leaves the V2 prover untouched.

Jump mode (default) proves one height H in (fplTip, fplTip + maxJump] and
would submit it in a single `submitHeaderWithProofs` transaction. maxJump and
the trusted committee come from the V3 contract (`maxJump`, `currentLeaves`),
not from the attestation's `current_committee`. The next committee is still
the one sealed in the height-H attestation (that is what the header rotates
to). Signer index is the on-chain leaf index.

No 1 gwei gas-price floor. Gas limit: `--gas-limit 0` (default) takes
`eth_estimateGas` of the exact submit x1.25, capped at 16,000,000 (below the
EIP-7825 per-tx cap 2**24); if the estimate fails it uses 2,700,000, and if
the estimate says the call reverts (or is itself above 16,000,000) nothing is sent. `--dry-run` is
the default and prints calldata without sending. Pass `--send` to broadcast.

Key source. The signing key is named `FPL_V3_PROVER_KEY` and nothing else
(never a path, never the old deployer key file, never logged). Two ways in:

1. Preferred: systemd `LoadCredential=FPL_V3_PROVER_KEY:/path` (or
   `SetCredentialEncrypted=`). The key is read from
   `$CREDENTIALS_DIRECTORY/FPL_V3_PROVER_KEY` and never enters any
   environment. Setting both this and the env var is refused.
2. Env `FPL_V3_PROVER_KEY`. At start-up the script validates it and then
   re-execs itself (same pid, `os.execve`) with the variable removed from the
   environment; the key crosses the exec in an inherited pipe whose write end
   is already closed, and the new image reads it once and closes the fd. So
   the long-running process's `/proc/<pid>/environ` and cmdline never hold
   the key. In dry-run the variable is dropped the same way and not read.

The key never goes into argv. foundry 1.8.5 `cast send --private-key` has no
env-var form, so on `--send` the key is wrapped in a one-time encrypted V3
keystore (random password) held in two anonymous `memfd`s. cast gets only
`--keystore /dev/fd/N --password-file /dev/fd/M`; nothing touches the disk and
both fds are closed after the send. No child (cast, ctl, the V2 prove helpers)
gets the key in its environment. Needs Linux (`os.memfd_create`) and the
`cryptography` package (AES-128-CTR); without them `--send` refuses to run
rather than fall back. `--private-key` or raw hex in cast argv is refused.

What is NOT scrubbed (residual, all owner/root only):
- The key stays in this process's heap for its whole life (Python strings
  cannot be wiped); root or a same-uid ptrace/`/proc/<pid>/mem` reader can
  get it. Run the prover as its own user.
- With the env route, whatever launched the prover had the key in its own
  environment (the shell, a unit's `Environment=`, which `systemctl show`
  prints), and this pid's pre-exec environ held it for the milliseconds
  before the re-exec. Use LoadCredential to avoid both.
- With LoadCredential the plaintext file sits in systemd's per-unit
  credentials directory (non-swappable, mode 0400, removed when the unit
  stops). `CREDENTIALS_DIRECTORY` is dropped from os.environ once the key is
  read, so ctl / prove helpers / cast are not handed the path, but any
  same-uid process that knows the path can still open the file.
- During a send, the encrypted keystore and its password are readable
  through `/proc/<pid>/fd` by the same uid until the fds close.

  pl-falcon-qc-v3-prover.py --jump --height H          # dry-run calldata
  pl-falcon-qc-v3-prover.py --jump --height H --send   # broadcast
  pl-falcon-qc-v3-prover.py --no-jump --send           # tip+1 only
"""
from __future__ import annotations

import argparse
import json
import os
import shlex
import socket
import subprocess
import sys
import time
from pathlib import Path

_SCRIPT = Path(__file__).resolve()
if str(_SCRIPT.parent) not in sys.path:
    sys.path.insert(0, str(_SCRIPT.parent))
from eth_bridge_config import ETH_RPC, ETH_RPC_FALLBACKS, parse_cast_uint  # noqa: E402

ROOT = Path(os.environ.get("PL_ROOT", str(Path.home() / "falcon-pl-public-testnet-2300")))
CTL = Path(os.environ.get("FPL_CTL", str(ROOT / "bin/falcon-pl-ctl")))
CAST = Path(os.environ.get("CAST", str(Path.home() / ".foundry/bin/cast")))
PK = Path(os.environ.get("FALCON_QC_V3_PK", os.environ.get("FALCON_QC_PK", str(ROOT / "run/snark/falcon-qc-v2-one/pk.bin"))))
KEYS = Path(os.environ.get("FALCON_QC_KEYS", str(ROOT / "keys/falcon-qc")))
SIGS = Path(os.environ.get("FALCON_QC_SIGS", str(ROOT / "keys/falcon-qc/sigs-v2")))
WORK = Path(os.environ.get("FALCON_QC_V3_PROVER_DIR", str(ROOT / "run/snark/falcon-qc-v3-prover")))
ETH_RPC = os.environ.get("ETH_RPC", ETH_RPC)
KEY_ENV = "FPL_V3_PROVER_KEY"
THRESHOLD = int(os.environ.get("FALCON_QC_THRESHOLD", "4"))
# FalconQcBridgeV3 submitHeaderWithProofs takes uint64[4] / proof[4].
# Any other count cannot be ABI-encoded.
V3_PROOF_COUNT = 4


def require_v3_threshold(threshold: int | None = None) -> None:
    n = THRESHOLD if threshold is None else threshold
    if n != V3_PROOF_COUNT:
        raise SystemExit(
            f"FALCON_QC_THRESHOLD={n} is not valid for FalconQcBridgeV3; "
            f"submitHeaderWithProofs takes exactly {V3_PROOF_COUNT} proofs"
        )
NETWORK_ID = int(os.environ.get("FPL_NETWORK_ID", "2300"))
POLL = int(os.environ.get("POLL_SEC", "8"))
GAS_BUMP = float(os.environ.get("FALCON_QC_GAS_BUMP", "1.25"))
# --gas-limit 0 (the default) means: eth_estimateGas x GAS_ESTIMATE_BUFFER, capped
# at GAS_LIMIT_MAX. A real 4-proof submitHeaderWithProofs used 2,207,860 gas on a
# Sepolia fork (V3_TEST_PLAN.md F2), so the old fixed 2,000,000 ran out of gas.
GAS_LIMIT_DEFAULT = 0
GAS_ESTIMATE_BUFFER = 1.25
# Used only when the estimate itself fails (RPC error, no answer). Measured + ~22%.
GAS_LIMIT_FALLBACK = 2_700_000
# EIP-7825 per-transaction cap (2**24). Stay below it with margin.
TX_GAS_CAP = 16_777_216
GAS_LIMIT_MAX = 16_000_000
# Floor is 1 wei. The V2 prover floored at 1 gwei, which made a 3.6M-gas
# transaction cost more than the prover wallet held. Do not restore that.
GAS_FLOOR_WEI = 1
GAS_FALLBACK_WEI = 1_000_000  # 0.001 gwei, used only when eth_gasPrice fails
ONE_GWEI = 1_000_000_000
# Default sequential: each proof peaks at ~5-6 GB RSS (parallel x4 OOM-killed the host1 node 2026-10-09).
PROVE_PARALLEL = os.environ.get("FALCON_QC_PROVE_PARALLEL", "0").lower() in ("1", "true", "yes")
PROVE_WORKERS = max(1, min(THRESHOLD, int(os.environ.get("FALCON_QC_PROVE_WORKERS", "1") or "1")))
CAST_RECV_TIMEOUT = int(os.environ.get("CAST_RECV_TIMEOUT", "120"))
# Sepolia. An RPC that answers with any other chain id is never used.
CHAIN_ID = int(os.environ.get("FALCON_QC_V3_CHAIN_ID", "11155111"))
SUBMIT_ABI = (
    "submitHeaderWithProofs(uint64,bytes32,bytes32,bytes32,bytes32,bytes32,bytes32[6],"
    "uint64[4],uint256[2][4],uint256[2][2][4],uint256[2][4])"
)
SEATS = [
    os.environ.get("FPL_RPC", "127.0.0.1:19301"),
    "100.64.0.2:19302",
    "100.64.0.3:19303",
    "100.64.0.4:19304",
    "100.64.0.5:19305",
    "100.64.0.6:19306",
]
HOST = socket.gethostname()


def build_parser() -> argparse.ArgumentParser:
    ap = argparse.ArgumentParser(description="FalconQcBridgeV3 header prover (dry-run by default)")
    ap.add_argument("--jump", dest="jump", action="store_true", default=True, help="prove fplTip -> H in one tx (default)")
    ap.add_argument("--no-jump", dest="jump", action="store_false", help="submit only fplTip+1")
    ap.add_argument("--height", type=int, default=0, help="chosen H; with --jump, default is min(mesh tip, fplTip+maxJump)")
    ap.add_argument("--bridge", default="", help="V3 bridge (else FALCON_QC_V3_BRIDGE or FPL_SEPOLIA_QC_V3_BRIDGE)")
    ap.add_argument(
        "--gas-limit",
        type=int,
        default=GAS_LIMIT_DEFAULT,
        help=f"0 (default): eth_estimateGas x {GAS_ESTIMATE_BUFFER}, else {GAS_LIMIT_FALLBACK}; max {GAS_LIMIT_MAX}",
    )
    ap.add_argument("--dry-run", dest="dry_run", action="store_true", default=True, help="print calldata, do not send (default)")
    ap.add_argument("--no-dry-run", dest="dry_run", action="store_false")
    ap.add_argument("--send", action="store_true", help="broadcast the transaction (disables dry-run)")
    ap.add_argument("--once", action="store_true")
    return ap


def resolve_args(args: argparse.Namespace) -> argparse.Namespace:
    # Broadcasting needs the explicit --send. --no-dry-run alone must not send.
    if not args.dry_run and not args.send:
        raise SystemExit("--no-dry-run needs --send (only --send broadcasts)")
    args.dry_run = not args.send
    if args.gas_limit < 0:
        raise SystemExit("--gas-limit must be 0 (estimate) or positive")
    if args.gas_limit > GAS_LIMIT_MAX:
        raise SystemExit(f"--gas-limit {args.gas_limit} is above {GAS_LIMIT_MAX} (EIP-7825 cap {TX_GAS_CAP})")
    return args


def bridge_addr(cli: str = "") -> str:
    bridge = (
        cli
        or os.environ.get("FALCON_QC_V3_BRIDGE")
        or os.environ.get("FPL_SEPOLIA_QC_V3_BRIDGE")
        or ""
    ).strip()
    if not bridge:
        raise SystemExit("set --bridge, FALCON_QC_V3_BRIDGE, or FPL_SEPOLIA_QC_V3_BRIDGE")
    return bridge


def norm_hex(s: str) -> str:
    return (s or "").strip().lower().removeprefix("0x")


def b32(s: str) -> str:
    h = norm_hex(s)
    if len(h) != 64:
        raise ValueError(f"need 32-byte hex, got {s!r}")
    return "0x" + h


def norm32(s: str) -> str:
    return norm_hex(s).zfill(64)[-64:]


# Internal: fd of the pipe that carries the key across the env-scrubbing
# re-exec. Set only by reexec_without_key_env().
KEY_FD_ENV = "_FPL_V3_PROVER_KEY_FD"
# systemd LoadCredential name (file under $CREDENTIALS_DIRECTORY).
CRED_NAME = KEY_ENV


def parse_prover_key(raw: str | None, src: str = KEY_ENV) -> str:
    """32-byte hex only. Reject paths and anything else. Never echo the value."""
    if raw is None or raw.strip() == "":
        raise SystemExit(f"{src} is unset")
    raw = raw.strip()
    if any(c in raw for c in "/\\\n\r\t "):
        raise SystemExit(f"{src} must be hex, not a path or file")
    h = raw
    if h.lower().startswith("0x"):
        h = h[2:]
    if len(h) != 64 or any(c not in "0123456789abcdefABCDEF" for c in h):
        raise SystemExit(f"{src} must be 32-byte hex")
    return "0x" + h.lower()


def load_prover_key() -> str:
    """32-byte hex from FPL_V3_PROVER_KEY. Reject paths. Do not log the key."""
    return parse_prover_key(os.environ.get(KEY_ENV), KEY_ENV)


def credential_key_path() -> Path | None:
    """`$CREDENTIALS_DIRECTORY/FPL_V3_PROVER_KEY` if systemd provided it."""
    d = os.environ.get("CREDENTIALS_DIRECTORY", "").strip()
    if not d:
        return None
    p = Path(d) / CRED_NAME
    return p if p.is_file() else None


def load_credential_key(path: Path) -> str:
    try:
        raw = path.read_text(encoding="ascii")
    except (OSError, UnicodeDecodeError) as e:
        raise SystemExit(f"cannot read credential {CRED_NAME}: {type(e).__name__}") from None
    return parse_prover_key(raw, f"credential {CRED_NAME}")


def _read_key_fd(fd_text: str) -> str:
    """Read the key from the pipe handed over by reexec_without_key_env()."""
    import stat

    try:
        fd = int(fd_text)
    except ValueError:
        raise SystemExit(f"{KEY_FD_ENV} is not an fd") from None
    try:
        if not stat.S_ISFIFO(os.fstat(fd).st_mode):
            raise SystemExit(f"{KEY_FD_ENV} is not a pipe")
        chunks = []
        while True:
            b = os.read(fd, 4096)
            if not b:
                break
            chunks.append(b)
            if sum(len(c) for c in chunks) > 4096:
                raise SystemExit(f"{KEY_FD_ENV} carried too much data")
    except OSError as e:
        raise SystemExit(f"{KEY_FD_ENV}: {type(e).__name__}") from None
    finally:
        try:
            os.close(fd)
        except OSError:
            pass
    return parse_prover_key(b"".join(chunks).decode("ascii", "replace"), KEY_ENV)


def take_prover_key() -> str:
    """Load the key and leave none of it in os.environ.

    Order: the re-exec pipe, then the systemd credential, then the env var
    (the last only when called in-process without the re-exec, e.g. tests).
    """
    fd_text = os.environ.pop(KEY_FD_ENV, None)
    if fd_text is not None:
        os.environ.pop(KEY_ENV, None)
        return _read_key_fd(fd_text)
    cred = credential_key_path()
    if cred is not None:
        if os.environ.get(KEY_ENV):
            raise SystemExit(f"set the {CRED_NAME} credential or {KEY_ENV}, not both")
        return load_credential_key(cred)
    key = load_prover_key()
    os.environ.pop(KEY_ENV, None)
    return key


def reexec_without_key_env(argv: list[str]) -> None:
    """If FPL_V3_PROVER_KEY is in our environment, exec ourselves without it.

    `os.execve` keeps the pid and replaces the initial environment, so the
    running prover's /proc/<pid>/environ no longer holds the key. On --send the
    key is validated first (fail closed before exec) and handed over in an
    inherited pipe; its write end is closed before exec. In dry-run the key is
    just dropped. Returns only when there was nothing to scrub.
    """
    if KEY_ENV not in os.environ:
        return
    args = resolve_args(build_parser().parse_args(argv))
    env = {k: v for k, v in os.environ.items() if k not in (KEY_ENV, KEY_FD_ENV)}
    if not sys.executable:
        raise SystemExit(f"cannot re-exec to drop {KEY_ENV}: no sys.executable")
    if not args.dry_run:
        if credential_key_path() is not None:
            raise SystemExit(f"set the {CRED_NAME} credential or {KEY_ENV}, not both")
        key = load_prover_key()
        r, w = os.pipe()
        try:
            os.write(w, key.encode("ascii"))
        finally:
            os.close(w)
        del key
        os.set_inheritable(r, True)
        env[KEY_FD_ENV] = str(r)
    sys.stdout.flush()
    sys.stderr.flush()
    os.execve(sys.executable, [sys.executable, str(_SCRIPT), *argv], env)


def _keccak256(data: bytes) -> bytes:
    import importlib.util

    path = _SCRIPT.with_name("pl-eth-lc-relayer.py")
    spec = importlib.util.spec_from_file_location("pl_eth_lc_relayer_keccak", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load keccak from {path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod.keccak256(data)


def keystore_json(key: str, password: bytes, scrypt_n: int = 8192) -> bytes:
    """Web3 secret-storage V3 JSON for `key` (scrypt + AES-128-CTR, keccak MAC)."""
    import hashlib
    import secrets
    import uuid

    try:
        from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
    except ImportError as e:  # fail closed: never fall back to --private-key
        raise SystemExit(f"--send needs the python 'cryptography' package: {e}") from None
    raw = bytes.fromhex(key[2:] if key.lower().startswith("0x") else key)
    if len(raw) != 32:
        raise SystemExit(f"{KEY_ENV} must be 32-byte hex")
    salt = secrets.token_bytes(32)
    iv = secrets.token_bytes(16)
    dk = hashlib.scrypt(password, salt=salt, n=scrypt_n, r=8, p=1, dklen=32, maxmem=64 * 1024 * 1024)
    enc = Cipher(algorithms.AES(dk[:16]), modes.CTR(iv)).encryptor()
    ct = enc.update(raw) + enc.finalize()
    mac = _keccak256(dk[16:32] + ct)
    body = {
        "version": 3,
        "id": str(uuid.uuid4()),
        "crypto": {
            "cipher": "aes-128-ctr",
            "cipherparams": {"iv": iv.hex()},
            "ciphertext": ct.hex(),
            "kdf": "scrypt",
            "kdfparams": {"dklen": 32, "n": scrypt_n, "r": 8, "p": 1, "salt": salt.hex()},
            "mac": mac.hex(),
        },
    }
    return json.dumps(body).encode()


class MemfdSigner:
    """One-time keystore + password in anonymous memfds. Close after the send.

    `argv()` is the only thing cast sees: two `/dev/fd/N` paths. The memfds are
    not linked anywhere on disk and vanish when closed.
    """

    def __init__(self, key: str) -> None:
        import secrets

        if not hasattr(os, "memfd_create"):
            raise SystemExit("--send needs os.memfd_create (Linux)")
        password = secrets.token_hex(32).encode()
        blob = keystore_json(key, password)
        self.ks_fd = os.memfd_create("v3-ks", 0)
        self.pw_fd = os.memfd_create("v3-pw", 0)
        for fd, data in ((self.ks_fd, blob), (self.pw_fd, password)):
            os.write(fd, data)
            os.lseek(fd, 0, os.SEEK_SET)
        del password, blob

    @property
    def fds(self) -> tuple[int, int]:
        return (self.ks_fd, self.pw_fd)

    def argv(self) -> list[str]:
        return ["--keystore", f"/dev/fd/{self.ks_fd}", "--password-file", f"/dev/fd/{self.pw_fd}"]

    def close(self) -> None:
        for name in ("ks_fd", "pw_fd"):
            fd = getattr(self, name, -1)
            if fd >= 0:
                try:
                    os.ftruncate(fd, 0)
                except OSError:
                    pass
                try:
                    os.close(fd)
                except OSError:
                    pass
                setattr(self, name, -1)

    def __enter__(self) -> "MemfdSigner":
        return self

    def __exit__(self, *exc) -> None:
        self.close()


def child_env() -> dict[str, str]:
    """Environment for cast: no prover key, no stray keystore/password env."""
    env = dict(os.environ)
    for k in (
        KEY_ENV,
        KEY_FD_ENV,
        "CREDENTIALS_DIRECTORY",
        "ETH_KEYSTORE",
        "ETH_KEYSTORE_ACCOUNT",
        "ETH_PASSWORD",
        "ETH_FROM",
    ):
        env.pop(k, None)
    return env


def scrub(text: str, key: str) -> str:
    if not text or not key:
        return text
    bare = key[2:] if key.lower().startswith("0x") else key
    return text.replace(key, "<redacted>").replace("0x" + bare, "<redacted>").replace(bare, "<redacted>")


def gas_price_wei(
    quoted: int | None,
    bump: float = GAS_BUMP,
    floor: int = GAS_FLOOR_WEI,
) -> int:
    """Bump a quoted wei price. The floor is 1 wei, never 1 gwei."""
    if floor >= ONE_GWEI:
        raise ValueError("gas floor must stay below 1 gwei")
    base = GAS_FALLBACK_WEI if quoted is None else max(0, int(quoted))
    bumped = int(base * bump)
    if bumped < floor:
        bumped = floor
    return bumped


def parse_gas_quote(text: str) -> int:
    s = (text or "").strip().split()[0]
    if not s:
        raise ValueError("empty gas price")
    return int(s, 16) if s.startswith(("0x", "0X")) else int(s)


def clamp_jump_target(tip: int, mesh: int, max_jump: int, requested: int, jump: bool) -> int:
    """H in (tip, tip + maxJump]. --no-jump is tip+1. A requested H outside the window errors."""
    if max_jump <= 0:
        raise ValueError("maxJump must be > 0")
    if tip < 0:
        raise ValueError("fplTip")
    hi = tip + max_jump
    if not jump:
        h = tip + 1
        if h > hi:
            raise ValueError(f"tip+1 exceeds maxJump ({max_jump})")
        return h
    if requested:
        if not (tip < requested <= hi):
            raise ValueError(f"height {requested} not in (fplTip={tip}, fplTip+maxJump={hi}]")
        return requested
    if mesh <= tip:
        raise ValueError(f"mesh tip {mesh} is not ahead of fplTip {tip}")
    return min(mesh, hi)


def trusted_seat_order(leaves: list[str], seat_leaves: dict[str, str]) -> list[str]:
    """Seat id per on-chain currentLeaves index. Every leaf must match a local pub."""
    if len(leaves) != 6:
        raise ValueError(f"V3 currentLeaves must be 6, got {len(leaves)}")
    rev: dict[str, str] = {}
    for sid, leaf in seat_leaves.items():
        rev[norm32(leaf)] = str(sid)
    order: list[str] = []
    missing: list[int] = []
    for i, leaf in enumerate(leaves):
        sid = rev.get(norm32(leaf))
        if not sid:
            missing.append(i)
            order.append("")
        else:
            order.append(sid)
    if missing:
        raise ValueError(f"no pub leaf for currentLeaves index {missing}")
    return order


def pick_signers(order: list[str], atts: dict, threshold: int = THRESHOLD) -> list[tuple[int, str]]:
    """Up to `threshold` attestations whose seats sit in the trusted leaf order.

    The index is the on-chain currentLeaves index, not the attestation's
    current_committee position.
    """
    picked: list[tuple[int, str]] = []
    seen: set[str] = set()
    for i, sid in enumerate(order):
        if not sid or sid in seen or sid not in atts:
            continue
        seen.add(sid)
        picked.append((i, sid))
        if len(picked) == threshold:
            return picked
    raise ValueError(f"need {threshold} trusted-committee sigs, got {len(picked)}")


def headers_agree(atts: dict, seats: list[str]) -> None:
    keys = ("header", "parent", "claim_root", "inner_hash", "next_committee_root")
    base = atts[seats[0]]
    for sid in seats[1:]:
        other = atts[sid]
        for k in keys:
            if norm_hex(str(base.get(k) or "")) != norm_hex(str(other.get(k) or "")):
                raise ValueError(f"attestation {k} mismatch {seats[0]} vs {sid}")


def fmt_idx(indices: list[int]) -> str:
    return "[" + ",".join(str(i) for i in indices) + "]"


def fmt_a(proofs: list[dict]) -> str:
    parts = [f"[{int(p['a'][0], 16)},{int(p['a'][1], 16)}]" for p in proofs]
    return "[" + ",".join(parts) + "]"


def fmt_b(proofs: list[dict]) -> str:
    parts = []
    for p in proofs:
        b = p["b"]
        parts.append(
            f"[[{int(b[0][0], 16)},{int(b[0][1], 16)}],[{int(b[1][0], 16)},{int(b[1][1], 16)}]]"
        )
    return "[" + ",".join(parts) + "]"


def fmt_c(proofs: list[dict]) -> str:
    parts = [f"[{int(p['c'][0], 16)},{int(p['c'][1], 16)}]" for p in proofs]
    return "[" + ",".join(parts) + "]"


def fmt_leaves(leaves: list[str]) -> str:
    return "[" + ",".join(b32(x) for x in leaves) + "]"


def submit_command(
    bridge: str,
    height: int,
    meta: dict,
    indices: list[int],
    proofs: list[dict],
    gas_limit: int,
    gas_price: int,
    rpc: str,
    signer_argv: list[str] | None,
    send: bool,
) -> list[str]:
    """cast calldata (dry-run) or cast send. Never carries key material.

    `signer_argv` is `MemfdSigner.argv()` (fd paths only) and is required to
    send. A raw key in it is refused. `cast calldata` takes the signature
    first and no address; `cast send` takes the address, then the signature.
    """
    head = [str(CAST), "send", bridge, SUBMIT_ABI] if send else [str(CAST), "calldata", SUBMIT_ABI]
    cmd = [
        *head,
        str(height),
        b32(meta["header"]),
        b32(meta["parent"]),
        b32(meta["inner"]),
        b32(meta["claim"]),
        b32(meta["next_root"]),
        fmt_leaves(meta["next_leaves"]),
        fmt_idx(indices),
        fmt_a(proofs),
        fmt_b(proofs),
        fmt_c(proofs),
    ]
    if send:
        if not signer_argv:
            raise SystemExit(f"{KEY_ENV} is unset")
        if "--private-key" in signer_argv or any(
            len(norm_hex(a)) == 64 and all(c in "0123456789abcdef" for c in norm_hex(a)) for a in signer_argv
        ):
            raise SystemExit("refusing to put a raw key in argv")
        cmd += [
            "--rpc-url",
            rpc,
            *signer_argv,
            "--gas-limit",
            str(gas_limit),
            "--gas-price",
            str(gas_price),
            "--legacy",
            "--json",
        ]
    return cmd


class EstimateReverted(RuntimeError):
    """eth_estimateGas says the submit would revert. Sending it would only burn gas."""


class EstimateTooHigh(RuntimeError):
    """eth_estimateGas is above GAS_LIMIT_MAX: any allowed limit would run out of gas."""


def gas_limit_from_estimate(estimate: int | None) -> int:
    """estimate x GAS_ESTIMATE_BUFFER (rounded up), capped at GAS_LIMIT_MAX.

    `None` (the estimate failed for a reason other than a revert) gives
    GAS_LIMIT_FALLBACK. Only the buffer is capped: an estimate that itself is
    above GAS_LIMIT_MAX raises EstimateTooHigh and nothing is sent.
    """
    if estimate is None or estimate <= 0:
        return GAS_LIMIT_FALLBACK
    if estimate > GAS_LIMIT_MAX:
        raise EstimateTooHigh(f"gas estimate {estimate} is above GAS_LIMIT_MAX {GAS_LIMIT_MAX}; not sending")
    buffered = -(-estimate * int(GAS_ESTIMATE_BUFFER * 100) // 100)
    return min(buffered, GAS_LIMIT_MAX)


def estimate_submit_gas(bridge: str, submit_args: list[str], rpc: str, sender: str = "") -> int | None:
    """`cast estimate` of the exact submit. Never carries key material.

    `submit_args` are the call arguments after the signature (as built by
    `submit_command`). Returns None when the estimate fails without a revert
    (RPC down, timeout). Raises EstimateReverted when the node says the call
    reverts (stale tip, bad proof): the caller must not send.
    """
    cmd = [str(CAST), "estimate", "--rpc-url", rpc]
    if sender:
        cmd += ["--from", sender]
    cmd += [bridge, SUBMIT_ABI, *submit_args]
    try:
        r = subprocess.run(cmd, text=True, capture_output=True, timeout=60, env=child_env(), stdin=subprocess.DEVNULL)
    except Exception as e:
        print(f"gas estimate failed ({type(e).__name__}); using fallback {GAS_LIMIT_FALLBACK}", flush=True)
        return None
    blob = (r.stdout or "") + (r.stderr or "")
    if r.returncode != 0:
        if "revert" in blob.lower():
            raise EstimateReverted("gas estimate: submit would revert: " + blob.strip()[-400:])
        print(f"gas estimate failed; using fallback {GAS_LIMIT_FALLBACK}: {blob.strip()[-300:]}", flush=True)
        return None
    try:
        return int((r.stdout or "").split()[0], 0)
    except (IndexError, ValueError):
        print(f"gas estimate unparsable; using fallback {GAS_LIMIT_FALLBACK}: {blob.strip()[-200:]}", flush=True)
        return None


def signer_address(signer_argv: list[str], pass_fds: tuple[int, ...]) -> str:
    """Address of the MemfdSigner keystore (`cast wallet address`, fd paths only)."""
    r = subprocess.run(
        [str(CAST), "wallet", "address", *signer_argv],
        text=True,
        capture_output=True,
        timeout=60,
        pass_fds=pass_fds,
        env=child_env(),
        stdin=subprocess.DEVNULL,
    )
    out = (r.stdout or "").strip()
    if r.returncode != 0 or not out.startswith("0x") or len(out) != 42:
        return ""
    return out


def resolve_gas_limit(explicit: int, bridge: str, submit_args: list[str], rpc: str, sender: str = "") -> int:
    """--gas-limit if given, else estimate x buffer (fallback GAS_LIMIT_FALLBACK)."""
    if explicit > 0:
        return min(explicit, GAS_LIMIT_MAX)
    est = estimate_submit_gas(bridge, submit_args, rpc, sender)
    limit = gas_limit_from_estimate(est)
    print(f"gas estimate={est} gas_limit={limit} (x{GAS_ESTIMATE_BUFFER}, max {GAS_LIMIT_MAX})", flush=True)
    return limit


def dry_run_report(cmd: list[str], gas_limit: int, gas_price: int, bridge: str = "") -> str:
    shown = " ".join(shlex.quote(a) for a in cmd)
    to = f" to={bridge}" if bridge else ""
    return f"dry-run{to} gas_limit={gas_limit} gas_price={gas_price}\n{shown}"


def eth_rpcs() -> list[str]:
    out: list[str] = []
    for u in [ETH_RPC, *ETH_RPC_FALLBACKS]:
        if u and u not in out:
            out.append(u)
    return out


def ctl_status(addr: str) -> dict:
    out = subprocess.check_output([str(CTL), "--addr", addr, "status"], text=True, timeout=20)
    i = out.find("{")
    body = json.JSONDecoder().raw_decode(out[i:])[0]
    return body.get("body") or body


def mesh_fpl_tip() -> int:
    best = 0
    for addr in SEATS:
        try:
            h = int(ctl_status(addr).get("tip_height") or 0)
            if h > best:
                best = h
        except Exception:
            continue
    return best


def _cast(args: list[str], timeout: int = 30) -> str:
    return subprocess.check_output(args, text=True, timeout=timeout).strip()


def pick_rpc() -> str:
    """First ETH RPC whose `cast chain-id` is CHAIN_ID (Sepolia).

    One run uses this single RPC for every read and for the send, so a run
    whose reads succeeded never broadcasts through a dead primary, and a
    misconfigured RPC on another chain is never read from or sent to. If it
    fails mid-run the run errors and the next loop picks again.
    """
    last = "no rpc"
    for rpc in eth_rpcs():
        try:
            out = _cast([str(CAST), "chain-id", "--rpc-url", rpc], timeout=15)
            got = int(out.split()[0], 0)
        except Exception as e:
            last = str(e)
            continue
        if got == CHAIN_ID:
            return rpc
        last = f"{rpc} is chain {got}, want {CHAIN_ID}"
        print(f"skip rpc: {last}", flush=True)
    raise RuntimeError(f"no ETH RPC on chain {CHAIN_ID} answered: {last}")


def eth_call_uint(bridge: str, sig: str, rpcs: list[str] | None = None) -> int:
    last = "no rpc"
    for rpc in rpcs or eth_rpcs():
        try:
            out = _cast([str(CAST), "call", bridge, sig, "--rpc-url", rpc])
            return parse_cast_uint(out)
        except Exception as e:
            last = str(e)
            continue
    raise RuntimeError(f"cast {sig} failed: {last}")


def load_onchain_leaves(bridge: str, rpcs: list[str] | None = None) -> list[str]:
    leaves: list[str] = []
    last = "no rpc"
    for rpc in rpcs or eth_rpcs():
        try:
            got: list[str] = []
            for i in range(6):
                out = _cast(
                    [str(CAST), "call", bridge, "currentLeaves(uint256)(bytes32)", str(i), "--rpc-url", rpc]
                )
                got.append(b32(out.split()[0]))
            leaves = got
            break
        except Exception as e:
            last = str(e)
            continue
    if len(leaves) != 6:
        raise RuntimeError(f"currentLeaves failed: {last}")
    return leaves


def load_seat_leaves(keys_dir: Path | None = None) -> dict[str, str]:
    """id -> leaf hex via `falcon-pl-ctl qc-leaves` (same binary as submit-eth-v3-deposit)."""
    keys = keys_dir or KEYS
    out = subprocess.check_output(
        [str(CTL), "qc-leaves", "--keys-dir", str(keys)],
        text=True,
        timeout=120,
    )
    rows = json.loads(out)
    seat_leaves: dict[str, str] = {}
    for row in rows:
        sid = str(row.get("id") or "")
        leaf = str(row.get("leaf") or "")
        if sid and leaf:
            seat_leaves[sid] = leaf
    return seat_leaves


def load_atts(height: int) -> dict[str, dict]:
    out: dict[str, dict] = {}
    if not SIGS.is_dir():
        return out
    for p in SIGS.glob(f"{height:010}-*.json"):
        try:
            att = json.loads(p.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        sig = att.get("sig") or {}
        sid = (sig.get("id") if isinstance(sig, dict) else None) or att.get("id")
        if sid and isinstance(sig, dict) and sig.get("sig_hex"):
            out[str(sid)] = att
    return out


def poll_atts() -> None:
    SIGS.mkdir(parents=True, exist_ok=True)
    for addr in SEATS:
        try:
            st = ctl_status(addr)
        except Exception:
            continue
        fq = st.get("falcon_qc") or {}
        for att in fq.get("attestations_v2") or []:
            if not isinstance(att, dict):
                continue
            h = int(att.get("height") or 0)
            sid = ((att.get("sig") or {}).get("id") if isinstance(att.get("sig"), dict) else None) or ""
            if h and sid:
                dest = SIGS / f"{h:010}-{sid}.json"
                if not dest.exists():
                    dest.write_text(json.dumps(att) + "\n")


def fetch_gas_quote(rpcs: list[str] | None = None) -> int | None:
    for rpc in rpcs or eth_rpcs():
        try:
            return parse_gas_quote(_cast([str(CAST), "gas-price", "--rpc-url", rpc]))
        except Exception:
            continue
    return None


def _v2():
    import importlib.util

    path = _SCRIPT.with_name("pl-falcon-qc-v2-prover.py")
    spec = importlib.util.spec_from_file_location("pl_falcon_qc_v2_prover", path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {path}")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def work_dir_for_leaves(leaves: list[str] | None) -> Path:
    """Proof cache root bound to the on-chain trusted leaves.

    The V2 helpers cache `h-<height>/idx-<i>-<seat>/proof.json`. If V3's
    `currentLeaves` change while the mesh height is the same, that path would
    serve a proof for the old committee forever. Keying the root by the
    leaves makes a committee change start a fresh cache.
    """
    if not leaves:
        return WORK
    import hashlib

    digest = hashlib.sha256("".join(norm32(x) for x in leaves).encode()).hexdigest()[:16]
    return WORK / f"leaves-{digest}"


def prove_v3(
    height: int,
    order: list[str],
    picked: list[tuple[int, str]],
    atts: dict,
    leaves: list[str] | None = None,
) -> tuple[list[int], list[dict], dict]:
    """Prove `picked` signers against on-chain leaf order. Next keys follow the attestation."""
    v2 = _v2()
    work = work_dir_for_leaves(leaves)
    v2.WORK = work
    v2.PK = PK
    v2.KEYS = KEYS
    v2.NETWORK_ID = NETWORK_ID
    v2.PROVE_PARALLEL = PROVE_PARALLEL
    v2.PROVE_WORKERS = PROVE_WORKERS
    v2.die_if_v1_pk()
    if not PK.is_file():
        raise SystemExit(f"missing v2 pk.bin at {PK}")
    seats = [sid for _, sid in picked]
    headers_agree(atts, seats)
    sample = atts[seats[0]]
    next_ids = v2.sealed_six(sample, "next_committee")
    if len(next_ids) != 6:
        raise RuntimeError(f"h={height} need sealed next committee of 6, got {len(next_ids)}")
    pubs = v2.load_pubs()
    cur_dir = work / f"h-{height}" / "cur6"
    next_dir = work / f"h-{height}" / "next6"
    v2.write_ordered_keys(cur_dir, order, pubs)
    v2.write_ordered_keys(next_dir, next_ids, pubs)
    parent = v2.norm_hex(sample.get("parent") or "")
    header = v2.norm_hex(sample.get("header") or "")
    claim = v2.norm_hex(sample.get("claim_root") or "")
    inner = v2.norm_hex(sample.get("inner_hash") or "")
    next_root = v2.norm_hex(sample.get("next_committee_root") or "")
    # Each proof is independent (own idx-<i>-<seat> dir) and single-threaded (~16 min on
    # host1), so prove them side by side like the V2 prover does. Order is kept.
    workers = min(PROVE_WORKERS, len(picked)) if PROVE_PARALLEL else 1
    if workers > 1:
        from concurrent.futures import ThreadPoolExecutor

        print(f"prove h={height} parallel workers={workers} signers={seats}", flush=True)
        with ThreadPoolExecutor(max_workers=workers) as ex:
            futs = [
                ex.submit(v2.prove_one_open, height, parent, header, claim, atts[sid], sid, ix, cur_dir, next_dir)
                for ix, sid in picked
            ]
            proofs: list[dict] = [f.result() for f in futs]
    else:
        proofs = []
        for ix, sid in picked:
            proofs.append(
                v2.prove_one_open(height, parent, header, claim, atts[sid], sid, ix, cur_dir, next_dir)
            )
    meta = {
        "parent": parent,
        "header": header,
        "claim": claim,
        "inner": inner,
        "next_root": next_root,
        "next_leaves": v2.next_leaves_from_proof(proofs[0]),
    }
    return [ix for ix, _ in picked], proofs, meta


def parse_tx_hash(out: str) -> str:
    """`transactionHash` from `cast send --json` (or its text receipt).

    Not the first 32-byte hex token: the receipt prints `blockHash` first.
    """
    i = out.find("{")
    if i >= 0:
        try:
            body = json.JSONDecoder().raw_decode(out[i:])[0]
            h = str(body.get("transactionHash") or "") if isinstance(body, dict) else ""
            if h.startswith("0x") and len(h) == 66:
                return h
        except json.JSONDecodeError:
            pass
    for line in out.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[0] == "transactionHash" and len(parts[1]) == 66:
            return parts[1]
    return ""


def emit(
    cmd: list[str],
    gas_limit: int,
    gas_price: int,
    send: bool,
    key: str,
    pass_fds: tuple[int, ...] = (),
    bridge: str = "",
) -> str:
    if not send:
        report = dry_run_report(cmd, gas_limit, gas_price, bridge)
        extra = ""
        if CAST.exists():
            r = subprocess.run(cmd, text=True, capture_output=True, timeout=60)
            blob = (r.stdout or "") + (r.stderr or "")
            if r.returncode != 0:
                raise RuntimeError(blob[-800:] or "cast calldata failed")
            extra = (r.stdout or "").strip()
        text = report if not extra else report + "\n" + extra
        print(text, flush=True)
        return text
    r = subprocess.run(
        cmd,
        text=True,
        capture_output=True,
        timeout=180,
        pass_fds=pass_fds,
        env=child_env(),
        stdin=subprocess.DEVNULL,
    )
    blob = scrub((r.stdout or "") + (r.stderr or ""), key)
    if r.returncode != 0:
        raise RuntimeError(blob[-1200:])
    tx = parse_tx_hash(r.stdout or "")
    if not tx:
        raise RuntimeError("cast send produced no tx hash: " + blob[-400:])
    print(f"submitted tx={tx} gas_limit={gas_limit} gas_price={gas_price}", flush=True)
    return tx


def run_once(args: argparse.Namespace, key: str = "") -> None:
    require_v3_threshold()
    bridge = bridge_addr(args.bridge)
    if not CAST.exists():
        raise SystemExit(f"cast not found at {CAST}")
    rpc = pick_rpc()
    only = [rpc]
    tip = eth_call_uint(bridge, "fplTip()(uint64)", only)
    max_jump = eth_call_uint(bridge, "maxJump()(uint64)", only)
    mesh = mesh_fpl_tip()
    target = clamp_jump_target(tip, mesh, max_jump, args.height, args.jump)
    print(
        f"v3 prover jump={args.jump} tip={tip} target={target} maxJump={max_jump} "
        f"mesh={mesh} dry_run={args.dry_run} gas_limit={args.gas_limit} rpc={rpc} host={HOST}",
        flush=True,
    )
    poll_atts()
    leaves = load_onchain_leaves(bridge, only)
    order = trusted_seat_order(leaves, load_seat_leaves())
    atts = load_atts(target)
    picked = pick_signers(order, atts, THRESHOLD)
    indices, proofs, meta = prove_v3(target, order, picked, atts, leaves)
    quote = fetch_gas_quote(only)
    gas_price = gas_price_wei(quote)
    # Same call arguments as the send; the estimate never sees the key.
    call_args = submit_command(bridge, target, meta, indices, proofs, 1, gas_price, rpc, None, send=False)[3:]
    if args.dry_run:
        gas_limit = resolve_gas_limit(args.gas_limit, bridge, call_args, rpc)
        cmd = submit_command(
            bridge, target, meta, indices, proofs, gas_limit, gas_price, rpc, None, send=False
        )
        emit(cmd, gas_limit, gas_price, send=False, key="", bridge=bridge)
        return
    if not key:
        raise SystemExit(f"{KEY_ENV} is unset")
    with MemfdSigner(key) as signer:
        sender = signer_address(signer.argv(), signer.fds) if args.gas_limit == 0 else ""
        gas_limit = resolve_gas_limit(args.gas_limit, bridge, call_args, rpc, sender)
        cmd = submit_command(
            bridge,
            target,
            meta,
            indices,
            proofs,
            gas_limit,
            gas_price,
            rpc,
            signer.argv(),
            send=True,
        )
        emit(cmd, gas_limit, gas_price, send=True, key=key, pass_fds=signer.fds)


def main(argv: list[str] | None = None) -> int:
    require_v3_threshold()
    args = resolve_args(build_parser().parse_args(argv))
    once = args.once or os.environ.get("FALCON_QC_ONCE", "") in ("1", "true", "yes")
    # Read once (re-exec pipe, credential, or env), then make sure neither the
    # key nor its fd marker is left in os.environ before any subprocess runs.
    key = take_prover_key() if not args.dry_run else ""
    os.environ.pop(KEY_ENV, None)
    os.environ.pop(KEY_FD_ENV, None)
    # Children (ctl, prove helpers, cast) run as this user; without the
    # directory variable they are not pointed at the plaintext credential.
    os.environ.pop("CREDENTIALS_DIRECTORY", None)
    while True:
        err: BaseException | None = None
        try:
            run_once(args, key)
        except SystemExit:
            raise
        except Exception as e:
            err = e
            msg = scrub(str(e), key)
            print(f"loop err: {msg}", flush=True)
        if once or args.dry_run:
            return 1 if err else 0
        time.sleep(POLL)


if __name__ == "__main__":
    reexec_without_key_env(sys.argv[1:])
    raise SystemExit(main())
