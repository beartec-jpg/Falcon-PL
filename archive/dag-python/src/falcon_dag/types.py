"""Core data structures: Transaction, Batch, Certificate."""

from __future__ import annotations

import hashlib
import json
import time
from dataclasses import dataclass, field
from typing import Any, Iterable


def sha256_hex(data: bytes | str) -> str:
    if isinstance(data, str):
        data = data.encode()
    return hashlib.sha256(data).hexdigest()


@dataclass(frozen=True)
class Transaction:
    tx_id: str
    account: str
    sequence: int
    destination: str
    amount: int
    fee: int
    first_seen: float = 0.0

    @staticmethod
    def create(
        account: str,
        sequence: int,
        destination: str,
        amount: int,
        fee: int = 10,
        first_seen: float | None = None,
    ) -> Transaction:
        payload = f"{account}:{sequence}:{destination}:{amount}:{fee}"
        return Transaction(
            tx_id=sha256_hex(payload),
            account=account,
            sequence=sequence,
            destination=destination,
            amount=amount,
            fee=fee,
            first_seen=first_seen if first_seen is not None else time.time(),
        )

    @property
    def fee_density(self) -> float:
        return float(self.fee)

    def to_dict(self) -> dict[str, Any]:
        return {
            "tx_id": self.tx_id,
            "account": self.account,
            "sequence": self.sequence,
            "destination": self.destination,
            "amount": self.amount,
            "fee": self.fee,
            "first_seen": self.first_seen,
        }

    @staticmethod
    def from_dict(d: dict[str, Any]) -> Transaction:
        return Transaction(
            tx_id=d["tx_id"],
            account=d["account"],
            sequence=int(d["sequence"]),
            destination=d["destination"],
            amount=int(d["amount"]),
            fee=int(d["fee"]),
            first_seen=float(d.get("first_seen", 0.0)),
        )


# Genesis certificate anchor (no batch body)
GENESIS_BATCH_HASH = sha256_hex("falcon-dag-genesis-batch-v1")
GENESIS_CERT_ID = sha256_hex("falcon-dag-genesis-cert-v1")


@dataclass
class Batch:
    batch_hash: str
    tx_ids: list[str]
    parents: list[str]  # parent batch hashes (prefer certified tips)
    creator: str
    timestamp: float
    txs: dict[str, Transaction] = field(default_factory=dict)
    # partial signatures: validator_id -> signature hex (HMAC-dev or Falcon later)
    signatures: dict[str, str] = field(default_factory=dict)

    @staticmethod
    def compute_hash(
        tx_ids: Iterable[str],
        parents: Iterable[str],
        creator: str,
        timestamp: float,
    ) -> str:
        body = {
            "tx_ids": list(tx_ids),
            "parents": sorted(parents),
            "creator": creator,
            "timestamp": timestamp,
        }
        return sha256_hex(json.dumps(body, sort_keys=True, separators=(",", ":")))

    @classmethod
    def create(
        cls,
        txs: list[Transaction],
        parents: list[str],
        creator: str,
        timestamp: float | None = None,
    ) -> Batch:
        ts = timestamp if timestamp is not None else time.time()
        tx_ids = [t.tx_id for t in txs]
        h = cls.compute_hash(tx_ids, parents, creator, ts)
        return cls(
            batch_hash=h,
            tx_ids=tx_ids,
            parents=list(parents),
            creator=creator,
            timestamp=ts,
            txs={t.tx_id: t for t in txs},
            signatures={},  # filled by engine after crypto.sign
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "batch_hash": self.batch_hash,
            "tx_ids": list(self.tx_ids),
            "parents": list(self.parents),
            "creator": self.creator,
            "timestamp": self.timestamp,
            "txs": {k: v.to_dict() for k, v in self.txs.items()},
            "signatures": dict(self.signatures),
        }

    @staticmethod
    def from_dict(d: dict[str, Any]) -> Batch:
        txs = {k: Transaction.from_dict(v) for k, v in d.get("txs", {}).items()}
        return Batch(
            batch_hash=d["batch_hash"],
            tx_ids=list(d["tx_ids"]),
            parents=list(d["parents"]),
            creator=d["creator"],
            timestamp=float(d["timestamp"]),
            txs=txs,
            signatures=dict(d.get("signatures", {})),
        )


@dataclass
class Certificate:
    """Proof that a batch is available (UNL_THRESHOLD signatures)."""

    cert_id: str
    batch_hash: str
    signers: list[str]
    parents: list[str]  # parent batch hashes (from the batch)
    creator: str
    timestamp: float
    round: int = 0

    @staticmethod
    def compute_id(batch_hash: str, signers: Iterable[str]) -> str:
        body = {"batch_hash": batch_hash, "signers": sorted(signers)}
        return sha256_hex(json.dumps(body, sort_keys=True, separators=(",", ":")))

    @classmethod
    def from_batch(cls, batch: Batch, round_num: int = 0) -> Certificate:
        signers = sorted(vid for vid, ok in batch.signatures.items() if ok)
        return cls(
            cert_id=cls.compute_id(batch.batch_hash, signers),
            batch_hash=batch.batch_hash,
            signers=signers,
            parents=list(batch.parents),
            creator=batch.creator,
            timestamp=batch.timestamp,
            round=round_num,
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "cert_id": self.cert_id,
            "batch_hash": self.batch_hash,
            "signers": list(self.signers),
            "parents": list(self.parents),
            "creator": self.creator,
            "timestamp": self.timestamp,
            "round": self.round,
        }

    @staticmethod
    def from_dict(d: dict[str, Any]) -> Certificate:
        return Certificate(
            cert_id=d["cert_id"],
            batch_hash=d["batch_hash"],
            signers=list(d["signers"]),
            parents=list(d.get("parents", [])),
            creator=d.get("creator", ""),
            timestamp=float(d.get("timestamp", 0.0)),
            round=int(d.get("round", 0)),
        )


@dataclass
class PartialSignature:
    batch_hash: str
    validator_id: str
    signature: str = ""
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> dict[str, Any]:
        return {
            "batch_hash": self.batch_hash,
            "validator_id": self.validator_id,
            "signature": self.signature,
            "timestamp": self.timestamp,
        }

    @staticmethod
    def from_dict(d: dict[str, Any]) -> PartialSignature:
        return PartialSignature(
            batch_hash=d["batch_hash"],
            validator_id=d["validator_id"],
            signature=str(d.get("signature", "")),
            timestamp=float(d.get("timestamp", 0.0)),
        )
