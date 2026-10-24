"""Validator authentication for batch / certificate signatures.

Backends
--------
* ``hmac-sha256-dev`` — local research only (shared seeds; not public-key).
* ``falcon-512`` — NIST PQC Falcon-512 via ``falcon-python`` (real public-key
  verify). Signatures ~650–660 bytes; public keys 897 bytes.

Interface (shared):

    sign(validator_id, message) -> signature_hex
    verify(validator_id, message, signature_hex) -> bool

Ordering still only moves certificates; Falcon-512 cost is paid on the DA
attestation path and amortized across fat batches.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import time
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any


def derive_seed(validator_id: str, network_salt: str = "falcon-dag-v1") -> bytes:
    return hashlib.sha256(f"{network_salt}:{validator_id}".encode()).digest()


def batch_sign_message(batch_hash: str) -> str:
    """Canonical message validators sign for a batch (data availability)."""
    return f"FALCON-DAG-BATCH-V1:{batch_hash}"


def cert_sign_message(batch_hash: str, signers: list[str]) -> str:
    return f"FALCON-DAG-CERT-V1:{batch_hash}:{','.join(sorted(signers))}"


class CryptoBackend(ABC):
    scheme: str

    @abstractmethod
    def sign(self, validator_id: str, message: str) -> str: ...

    @abstractmethod
    def verify(self, validator_id: str, message: str, signature: str) -> bool: ...

    @abstractmethod
    def public_ids(self) -> list[str]: ...

    def export_public(self) -> dict[str, str]:
        """validator_id -> public key hex (empty for HMAC)."""
        return {}

    def metrics(self) -> dict[str, Any]:
        return {"scheme": self.scheme}


# ---------------------------------------------------------------------------
# HMAC (dev)
# ---------------------------------------------------------------------------


@dataclass
class HmacKeyRing(CryptoBackend):
    """Dev/regtest HMAC-SHA256. Secrets known to all nodes — NOT for production."""

    seeds: dict[str, bytes] = field(default_factory=dict)
    scheme: str = "hmac-sha256-dev"

    @classmethod
    def for_unl(cls, validator_ids: list[str], network_salt: str = "falcon-dag-v1") -> HmacKeyRing:
        return cls(seeds={vid: derive_seed(vid, network_salt) for vid in validator_ids})

    def register(self, validator_id: str, seed: bytes | None = None) -> None:
        self.seeds[validator_id] = seed if seed is not None else derive_seed(validator_id)

    def sign(self, validator_id: str, message: str) -> str:
        secret = self.seeds.get(validator_id)
        if secret is None:
            raise KeyError(f"unknown validator: {validator_id}")
        return hmac.new(secret, message.encode(), hashlib.sha256).hexdigest()

    def verify(self, validator_id: str, message: str, signature: str) -> bool:
        secret = self.seeds.get(validator_id)
        if secret is None or not signature:
            return False
        expected = hmac.new(secret, message.encode(), hashlib.sha256).hexdigest()
        return hmac.compare_digest(expected, signature)

    def public_ids(self) -> list[str]:
        return sorted(self.seeds.keys())


# ---------------------------------------------------------------------------
# Falcon-512 (PQ)
# ---------------------------------------------------------------------------


def _falcon512():
    try:
        from falcon_python import Falcon512

        return Falcon512
    except ImportError as e:
        raise ImportError(
            "falcon-python is required for Falcon-512 certs. "
            "Install with: pip install falcon-python"
        ) from e


@dataclass
class Falcon512KeyRing(CryptoBackend):
    """Falcon-512 public-key attestations for batch certificates.

    * ``public_keys``: UNL material (safe to distribute)
    * ``secret_keys``: only keys this process may sign with
    """

    public_keys: dict[str, bytes] = field(default_factory=dict)
    secret_keys: dict[str, bytes] = field(default_factory=dict)
    scheme: str = "falcon-512"

    # NIST / liboqs-typical sizes (sig is compressed-variable ~650–690)
    PK_LEN = 897
    SK_LEN = 1281

    @classmethod
    def generate_unl(cls, validator_ids: list[str]) -> Falcon512KeyRing:
        F = _falcon512()
        pubs: dict[str, bytes] = {}
        secs: dict[str, bytes] = {}
        for vid in validator_ids:
            pk, sk = F.generate_keypair()
            if len(pk) != cls.PK_LEN or len(sk) != cls.SK_LEN:
                raise RuntimeError(
                    f"unexpected Falcon-512 key sizes pk={len(pk)} sk={len(sk)}"
                )
            pubs[vid] = bytes(pk)
            secs[vid] = bytes(sk)
        return cls(public_keys=pubs, secret_keys=secs)

    @classmethod
    def from_public_map(
        cls,
        public_hex: dict[str, str],
        local_id: str | None = None,
        local_sk_hex: str | None = None,
    ) -> Falcon512KeyRing:
        pubs = {k: bytes.fromhex(v) for k, v in public_hex.items()}
        secs: dict[str, bytes] = {}
        if local_id and local_sk_hex:
            secs[local_id] = bytes.fromhex(local_sk_hex)
        return cls(public_keys=pubs, secret_keys=secs)

    def sign(self, validator_id: str, message: str) -> str:
        sk = self.secret_keys.get(validator_id)
        if sk is None:
            raise KeyError(f"no Falcon secret for {validator_id}")
        F = _falcon512()
        sig = F.detached_sign(sk, message.encode())
        return bytes(sig).hex()

    def verify(self, validator_id: str, message: str, signature: str) -> bool:
        pk = self.public_keys.get(validator_id)
        if pk is None or not signature:
            return False
        try:
            sig = bytes.fromhex(signature)
        except ValueError:
            return False
        F = _falcon512()
        try:
            return bool(F.verify_detached_sign(sig, message.encode(), pk))
        except Exception:
            return False

    def public_ids(self) -> list[str]:
        return sorted(self.public_keys.keys())

    def export_public(self) -> dict[str, str]:
        return {k: v.hex() for k, v in self.public_keys.items()}

    def export_secret(self, validator_id: str) -> str:
        return self.secret_keys[validator_id].hex()

    def metrics(self) -> dict[str, Any]:
        # Sample one signature for size reporting
        sample_sig_len = None
        if self.secret_keys:
            vid = next(iter(self.secret_keys))
            try:
                sample_sig_len = len(bytes.fromhex(self.sign(vid, "size-probe")))
            except Exception:
                sample_sig_len = None
        return {
            "scheme": self.scheme,
            "public_key_bytes": self.PK_LEN,
            "secret_key_bytes": self.SK_LEN,
            "sample_sig_bytes": sample_sig_len,
            "unl_size": len(self.public_keys),
        }

    def save_dir(self, directory: str | Path) -> None:
        """Write per-validator key files + unl_public.json."""
        d = Path(directory)
        d.mkdir(parents=True, exist_ok=True)
        pub = self.export_public()
        (d / "unl_public.json").write_text(json.dumps(pub, indent=2) + "\n")
        for vid, sk in self.secret_keys.items():
            payload = {
                "validator_id": vid,
                "scheme": self.scheme,
                "public_key": self.public_keys[vid].hex(),
                "secret_key": sk.hex(),
            }
            (d / f"{vid}.json").write_text(json.dumps(payload, indent=2) + "\n")
            os.chmod(d / f"{vid}.json", 0o600)

    @classmethod
    def load_for_validator(cls, directory: str | Path, validator_id: str) -> Falcon512KeyRing:
        d = Path(directory)
        pub_path = d / "unl_public.json"
        if not pub_path.is_file():
            raise FileNotFoundError(pub_path)
        public_hex = json.loads(pub_path.read_text())
        local = d / f"{validator_id}.json"
        local_sk = None
        if local.is_file():
            local_sk = json.loads(local.read_text())["secret_key"]
        return cls.from_public_map(public_hex, validator_id, local_sk)


# ---------------------------------------------------------------------------
# Timing wrapper
# ---------------------------------------------------------------------------


@dataclass
class TimedCrypto(CryptoBackend):
    """Wrap any backend and record sign/verify latency + signature sizes."""

    inner: CryptoBackend
    sign_times_ms: list[float] = field(default_factory=list)
    verify_times_ms: list[float] = field(default_factory=list)
    sig_sizes: list[int] = field(default_factory=list)
    max_samples: int = 512

    @property
    def scheme(self) -> str:  # type: ignore[override]
        return self.inner.scheme

    def _record(self, bucket: list[float], value: float) -> None:
        bucket.append(value)
        if len(bucket) > self.max_samples:
            del bucket[: len(bucket) - self.max_samples]

    def sign(self, validator_id: str, message: str) -> str:
        t0 = time.perf_counter()
        sig = self.inner.sign(validator_id, message)
        self._record(self.sign_times_ms, (time.perf_counter() - t0) * 1000.0)
        try:
            self.sig_sizes.append(len(bytes.fromhex(sig)))
            if len(self.sig_sizes) > self.max_samples:
                del self.sig_sizes[: len(self.sig_sizes) - self.max_samples]
        except ValueError:
            pass
        return sig

    def verify(self, validator_id: str, message: str, signature: str) -> bool:
        t0 = time.perf_counter()
        ok = self.inner.verify(validator_id, message, signature)
        self._record(self.verify_times_ms, (time.perf_counter() - t0) * 1000.0)
        return ok

    def public_ids(self) -> list[str]:
        return self.inner.public_ids()

    def export_public(self) -> dict[str, str]:
        return self.inner.export_public()

    def metrics(self) -> dict[str, Any]:
        base = self.inner.metrics() if hasattr(self.inner, "metrics") else {"scheme": self.scheme}

        def stats(xs: list[float]) -> dict[str, float | int]:
            if not xs:
                return {"n": 0}
            s = sorted(xs)
            return {
                "n": len(s),
                "min_ms": round(s[0], 4),
                "p50_ms": round(s[len(s) // 2], 4),
                "p95_ms": round(s[int(len(s) * 0.95)], 4),
                "max_ms": round(s[-1], 4),
                "mean_ms": round(sum(s) / len(s), 4),
            }

        sig_stats: dict[str, Any] = {"n": len(self.sig_sizes)}
        if self.sig_sizes:
            sig_stats.update(
                {
                    "min_b": min(self.sig_sizes),
                    "max_b": max(self.sig_sizes),
                    "mean_b": round(sum(self.sig_sizes) / len(self.sig_sizes), 1),
                }
            )
        base["timing"] = {
            "sign": stats(self.sign_times_ms),
            "verify": stats(self.verify_times_ms),
            "signature_bytes": sig_stats,
        }
        return base


# ---------------------------------------------------------------------------
# Factory / compatibility alias
# ---------------------------------------------------------------------------


def create_keyring(
    scheme: str,
    validator_ids: list[str],
    *,
    keys_dir: str | Path | None = None,
    local_id: str | None = None,
    timed: bool = True,
) -> CryptoBackend:
    scheme = (scheme or "falcon-512").lower()
    if scheme in ("hmac", "hmac-sha256-dev", "dev"):
        ring: CryptoBackend = HmacKeyRing.for_unl(validator_ids)
    elif scheme in ("falcon-512", "falcon512", "falcon"):
        if keys_dir and local_id and (Path(keys_dir) / "unl_public.json").is_file():
            ring = Falcon512KeyRing.load_for_validator(keys_dir, local_id)
        elif keys_dir and not (Path(keys_dir) / "unl_public.json").is_file():
            ring = Falcon512KeyRing.generate_unl(validator_ids)
            ring.save_dir(keys_dir)
            if local_id and local_id not in ring.secret_keys:
                raise KeyError(local_id)
        else:
            ring = Falcon512KeyRing.generate_unl(validator_ids)
    else:
        raise ValueError(f"unknown crypto scheme: {scheme}")
    return TimedCrypto(inner=ring) if timed else ring


# Back-compat name used across the codebase
KeyRing = HmacKeyRing


def falcon_available() -> bool:
    try:
        _falcon512()
        return True
    except ImportError:
        return False


def benchmark_crypto(scheme: str = "falcon-512", rounds: int = 50) -> dict[str, Any]:
    """Micro-benchmark keygen / sign / verify for a scheme."""
    ids = ["bench1"]
    t_keygen0 = time.perf_counter()
    ring = create_keyring(scheme, ids, timed=False)
    keygen_ms = (time.perf_counter() - t_keygen0) * 1000.0
    msg = batch_sign_message("ff" * 32)
    # warmup
    sig = ring.sign("bench1", msg)
    ring.verify("bench1", msg, sig)

    sign_ms: list[float] = []
    verify_ms: list[float] = []
    sizes: list[int] = []
    for i in range(rounds):
        m = f"{msg}:{i}"
        t0 = time.perf_counter()
        s = ring.sign("bench1", m)
        sign_ms.append((time.perf_counter() - t0) * 1000.0)
        sizes.append(len(bytes.fromhex(s)))
        t1 = time.perf_counter()
        assert ring.verify("bench1", m, s)
        verify_ms.append((time.perf_counter() - t1) * 1000.0)

    def summary(xs: list[float]) -> dict[str, float]:
        s = sorted(xs)
        return {
            "min_ms": round(s[0], 4),
            "p50_ms": round(s[len(s) // 2], 4),
            "p95_ms": round(s[int(len(s) * 0.95)], 4),
            "max_ms": round(s[-1], 4),
            "mean_ms": round(sum(s) / len(s), 4),
        }

    return {
        "scheme": ring.scheme,
        "rounds": rounds,
        "keygen_ms": round(keygen_ms, 3),
        "sign": summary(sign_ms),
        "verify": summary(verify_ms),
        "sig_bytes": {
            "min": min(sizes),
            "max": max(sizes),
            "mean": round(sum(sizes) / len(sizes), 1),
        },
    }
