"""Consensus-critical protocol parameters for Falcon DAG."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class ProtocolConfig:
    unl_threshold: float = 0.80
    max_batch_parents: int = 6
    target_batch_interval_s: float = 0.4
    max_batch_txs: int = 128
    hard_finality_h: int = 6
    ordering_interval_s: float = 0.5
    starvation_boost_per_s: float = 0.01
    network_id: int = 9002  # Falcon DAG regtest; not production
    # Adaptive batch interval (load-aware). Under high mempool depth, *slow*
    # batch production so each batch packs more txs → higher throughput at
    # the cost of slightly higher soft-final latency. Under idle load, *fast*
    # intervals for low latency.
    adaptive_interval: bool = False
    batch_interval_min_s: float = 0.15  # low load (fast / low latency)
    batch_interval_max_s: float = 1.2   # high load (slower / fatter batches)
    adaptive_mempool_low: int = 16      # at/below → min interval
    adaptive_mempool_high: int = 512    # at/above → max interval
    # Worker sharding: each validator only packs txs whose hash maps to its shard.
    # Reduces duplicate batches under parallel proposers (Narwhal worker pattern).
    worker_sharding: bool = True
    # If a tx sits longer than this, any worker may pack it (covers down workers).
    # Non-primary adoption ramps linearly with age/this value (Fix 5).
    shard_orphan_age_s: float = 2.0
    # Require cryptographic verification of batch signatures (KeyRing).
    require_sig_verify: bool = True
    # Crypto backend: "falcon-512" (default) or "hmac-sha256-dev"
    crypto_scheme: str = "falcon-512"
    # --- DoS / resource bounds ---
    max_known_batches: int = 50_000
    max_uncertified_batches: int = 2_000
    # Fix 6: multi-packer needs headroom for concurrent open batches.
    # Rule of thumb: ≥ ceil(cert_latency_s / batch_interval_s) × 2
    max_uncertified_per_creator: int = 128
    max_mempool_txs: int = 50_000
    # Snapshot interval for crash recovery (0 = disabled)
    snapshot_interval_s: float = 5.0
    # --- Rate limits / pruning ---
    # Max new signature messages accepted per validator per rolling window
    max_sigs_per_validator_per_s: float = 50.0
    sig_rate_window_s: float = 1.0
    # Drop uncertified batches older than this (seconds) when over soft pressure
    uncertified_ttl_s: float = 120.0
    # Keep at most this many hard-final historical batches (0 = unlimited)
    max_hard_final_history: int = 10_000


DEFAULT_CONFIG = ProtocolConfig()
