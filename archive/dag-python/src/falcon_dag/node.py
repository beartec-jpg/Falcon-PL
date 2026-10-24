"""Falcon DAG validator node: DA + ordering + HTTP RPC."""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import signal
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread
from typing import Any
from urllib.parse import urlparse

from falcon_dag.config import DEFAULT_CONFIG, ProtocolConfig
from falcon_dag.crypto import CryptoBackend, create_keyring, falcon_available
from falcon_dag.engine import FalconDAGEngine
from falcon_dag.network import PeerClient, PeerServer
from falcon_dag.types import Batch, Certificate, PartialSignature, Transaction

log = logging.getLogger("falcon-dag.node")


class Node:
    def __init__(
        self,
        validator_id: str,
        unl: dict[str, float],
        rpc_host: str = "127.0.0.1",
        rpc_port: int = 18001,
        peer_host: str = "127.0.0.1",
        peer_port: int = 19001,
        peers: list[tuple[str, int]] | None = None,
        config: ProtocolConfig = DEFAULT_CONFIG,
        genesis_balances: dict[str, int] | None = None,
        keyring: CryptoBackend | None = None,
        worker_index: int = 0,
        worker_count: int = 1,
        keys_dir: str | None = None,
        data_dir: str | None = None,
    ) -> None:
        self.validator_id = validator_id
        self.config = config
        self.rpc_host = rpc_host
        self.rpc_port = rpc_port
        self.peer_host = peer_host
        self.peer_port = peer_port
        self.peer_addrs = peers or []
        self.data_dir = data_dir
        self.started_at = time.time()
        self._loop: asyncio.AbstractEventLoop | None = None
        self._running = False
        self._peer_server: PeerServer | None = None
        self._peer_clients: list[PeerClient] = []
        self._http: ThreadingHTTPServer | None = None
        self._seen_batches: set[str] = set()
        self._seen_certs: set[str] = set()
        self._last_snapshot = 0.0

        self.engine = FalconDAGEngine(
            validator_id=validator_id,
            unl=unl,
            config=config,
            genesis_balances=genesis_balances,
            keyring=keyring,
            worker_index=worker_index,
            worker_count=worker_count,
            keys_dir=keys_dir,
            on_batch=self._emit_batch,
            on_sig=self._emit_sig,
            on_cert=self._emit_cert,
        )
        if data_dir:
            from falcon_dag.persist import load_snapshot

            snap_path = f"{data_dir.rstrip('/')}/snapshot.json"
            try:
                if load_snapshot(snap_path, self.engine):
                    log.info("loaded snapshot %s", snap_path)
            except Exception as e:
                log.warning("snapshot load failed: %s", e)

    def _queue(self, msg: dict[str, Any]) -> None:
        if self._loop and self._loop.is_running():
            asyncio.run_coroutine_threadsafe(self._broadcast(msg), self._loop)

    def _emit_batch(self, batch: Batch) -> None:
        self._seen_batches.add(batch.batch_hash)
        self._queue({"type": "batch", "batch": batch.to_dict()})

    def _emit_sig(self, sig: PartialSignature) -> None:
        self._queue({"type": "sig", "sig": sig.to_dict()})

    def _emit_cert(self, cert: Certificate) -> None:
        if cert.cert_id in self._seen_certs:
            return
        self._seen_certs.add(cert.cert_id)
        self._queue({"type": "cert", "cert": cert.to_dict()})

    async def _broadcast(self, msg: dict[str, Any]) -> None:
        if self._peer_server:
            await self._peer_server.broadcast(msg)
        for c in self._peer_clients:
            await c.send(msg)

    async def _on_peer_message(self, mtype: str, msg: dict[str, Any]) -> None:
        if mtype == "batch":
            batch = Batch.from_dict(msg["batch"])
            if batch.batch_hash in self._seen_batches:
                # still merge sigs
                self.engine.on_remote_batch(batch)
                return
            self._seen_batches.add(batch.batch_hash)
            self.engine.on_remote_batch(batch)
            await self._broadcast({"type": "batch", "batch": batch.to_dict()})
        elif mtype == "sig":
            sig = PartialSignature.from_dict(msg["sig"])
            self.engine.on_remote_sig(sig)
        elif mtype == "cert":
            cert = Certificate.from_dict(msg["cert"])
            if cert.cert_id in self._seen_certs:
                return
            self._seen_certs.add(cert.cert_id)
            self.engine.on_remote_cert(cert)
            await self._broadcast({"type": "cert", "cert": cert.to_dict()})
        elif mtype == "tx":
            tx = Transaction.from_dict(msg["tx"])
            if self.engine.submit_tx(tx):
                await self._broadcast({"type": "tx", "tx": tx.to_dict()})

    def _rpc(self, body: dict[str, Any]) -> dict[str, Any]:
        method = body.get("method", "")
        params = body.get("params") or [{}]
        p = params[0] if isinstance(params, list) and params else (params or {})
        try:
            if method == "ping":
                return {"pong": True, "validator_id": self.validator_id}
            if method == "server_info":
                m = self.engine.metrics()
                return {
                    "info": {
                        "server_state": "proposing" if self._running else "stopped",
                        "validator_id": self.validator_id,
                        "network_id": self.config.network_id,
                        "uptime_s": int(time.time() - self.started_at),
                        "build_version": "falcon-dag/0.3.0-falcon512",
                        "architecture": "narwhal-da + federated-ordering + falcon-512-certs",
                        "metrics": m,
                    }
                }
            if method == "submit":
                if "tx" in p:
                    tx = Transaction.from_dict(p["tx"])
                else:
                    tx = Transaction.create(
                        p["account"],
                        int(p["sequence"]),
                        p.get("destination", "acct1"),
                        int(p.get("amount", 1)),
                        int(p.get("fee", 10)),
                    )
                ok = self.engine.submit_tx(tx)
                self._queue({"type": "tx", "tx": tx.to_dict()})
                return {
                    "engine_result": "tesSUCCESS" if ok else "tefALREADY",
                    "tx_id": tx.tx_id,
                }
            if method == "canonical":
                return {
                    "ordered_batches": list(self.engine.ordering.ordered),
                    "txs": list(self.engine.execution.executed_txs),
                    "hard_final": sorted(self.engine.ordering.hard_final),
                }
            if method == "dag_status":
                return self.engine.metrics()
            if method == "crypto_stats":
                m = self.engine.dag.metrics()
                return {
                    "scheme": m.get("crypto_scheme"),
                    "crypto": m.get("crypto", {}),
                    "avg_sig_bytes": m.get("avg_sig_bytes"),
                    "est_cert_bytes_4sigs": m.get("est_cert_bytes_4sigs"),
                    "equivocations": m.get("equivocations", 0),
                    "rejected_sigs": m.get("rejected_sigs", 0),
                }
            if method == "snapshot":
                if not self.data_dir:
                    return {"error": "no data_dir configured"}
                from falcon_dag.persist import save_snapshot

                path = f"{self.data_dir.rstrip('/')}/snapshot.json"
                save_snapshot(path, self.engine)
                return {"saved": path, "batches": len(self.engine.dag.batches)}
            return {"error": f"unknown method: {method}"}
        except Exception as e:
            log.exception("rpc")
            return {"error": str(e)}

    def _start_http(self) -> None:
        node = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, fmt: str, *args: Any) -> None:
                log.debug("http " + fmt, *args)

            def do_GET(self) -> None:
                if urlparse(self.path).path in ("/", "/health", "/status"):
                    body = json.dumps(node._rpc({"method": "server_info"})).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                else:
                    self.send_error(404)

            def do_POST(self) -> None:
                n = int(self.headers.get("Content-Length", 0))
                raw = self.rfile.read(n) if n else b"{}"
                try:
                    body = json.loads(raw.decode() or "{}")
                except json.JSONDecodeError:
                    body = {}
                payload = json.dumps({"result": node._rpc(body)}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

        self._http = ThreadingHTTPServer((self.rpc_host, self.rpc_port), Handler)
        Thread(target=self._http.serve_forever, daemon=True).start()
        log.info("rpc listen %s:%s", self.rpc_host, self.rpc_port)

    async def run(self) -> None:
        self._loop = asyncio.get_running_loop()
        self._running = True
        self._start_http()
        self._peer_server = PeerServer(self.peer_host, self.peer_port, self._on_peer_message)
        await self._peer_server.start()
        for host, port in self.peer_addrs:
            c = PeerClient(host, port, self._on_peer_message, name=f"{host}:{port}")
            self._peer_clients.append(c)
            await c.start()
        log.info("node %s up (rpc=%s peer=%s)", self.validator_id, self.rpc_port, self.peer_port)
        try:
            while self._running:
                b = self.engine.tick()
                if b:
                    log.info(
                        "batch %s txs=%d certs=%d ordered=%d exec=%d",
                        b.batch_hash[:12],
                        len(b.tx_ids),
                        len(self.engine.dag.certificates),
                        len(self.engine.ordering.ordered),
                        len(self.engine.execution.executed_txs),
                    )
                # Periodic crash-recovery snapshot
                iv = self.config.snapshot_interval_s
                if self.data_dir and iv > 0 and (time.time() - self._last_snapshot) >= iv:
                    try:
                        from falcon_dag.persist import save_snapshot

                        save_snapshot(
                            f"{self.data_dir.rstrip('/')}/snapshot.json", self.engine
                        )
                        self._last_snapshot = time.time()
                    except Exception as e:
                        log.warning("snapshot save failed: %s", e)
                await asyncio.sleep(0.1)
        finally:
            await self.shutdown()

    async def shutdown(self) -> None:
        self._running = False
        if self.data_dir:
            try:
                from falcon_dag.persist import save_snapshot

                save_snapshot(
                    f"{self.data_dir.rstrip('/')}/snapshot.json", self.engine
                )
                log.info("final snapshot saved")
            except Exception as e:
                log.warning("final snapshot failed: %s", e)
        for c in self._peer_clients:
            await c.stop()
        if self._peer_server:
            await self._peer_server.stop()
        if self._http:
            self._http.shutdown()
            self._http = None


def main(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description="Falcon DAG node (Narwhal + federated order)")
    ap.add_argument("--config", required=True)
    ap.add_argument("--log-level", default="INFO")
    args = ap.parse_args(argv)
    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )
    with open(args.config, encoding="utf-8") as f:
        cfg = json.load(f)
    unl = {k: float(v) for k, v in cfg["unl"].items()}
    peers = [(h, int(p)) for h, p in cfg.get("peers", [])]
    vids = list(unl.keys())
    scheme = str(cfg.get("crypto_scheme", DEFAULT_CONFIG.crypto_scheme))
    if scheme.startswith("falcon") and not falcon_available():
        log.warning("falcon-python not installed; falling back to hmac-sha256-dev")
        scheme = "hmac-sha256-dev"
    pcfg = ProtocolConfig(
        unl_threshold=float(cfg.get("unl_threshold", DEFAULT_CONFIG.unl_threshold)),
        max_batch_parents=int(cfg.get("max_batch_parents", DEFAULT_CONFIG.max_batch_parents)),
        target_batch_interval_s=float(
            cfg.get("target_batch_interval_s", DEFAULT_CONFIG.target_batch_interval_s)
        ),
        max_batch_txs=int(cfg.get("max_batch_txs", DEFAULT_CONFIG.max_batch_txs)),
        hard_finality_h=int(cfg.get("hard_finality_h", DEFAULT_CONFIG.hard_finality_h)),
        network_id=int(cfg.get("network_id", DEFAULT_CONFIG.network_id)),
        worker_sharding=bool(cfg.get("worker_sharding", True)),
        adaptive_interval=bool(cfg.get("adaptive_interval", False)),
        require_sig_verify=bool(cfg.get("require_sig_verify", True)),
        crypto_scheme=scheme,
        max_uncertified_batches=int(
            cfg.get("max_uncertified_batches", DEFAULT_CONFIG.max_uncertified_batches)
        ),
        max_uncertified_per_creator=int(
            cfg.get(
                "max_uncertified_per_creator",
                DEFAULT_CONFIG.max_uncertified_per_creator,
            )
        ),
        snapshot_interval_s=float(
            cfg.get("snapshot_interval_s", DEFAULT_CONFIG.snapshot_interval_s)
        ),
    )
    keys_dir = cfg.get("keys_dir")
    keyring = create_keyring(
        scheme,
        vids,
        keys_dir=keys_dir,
        local_id=cfg["validator_id"],
    )
    wid = int(cfg.get("worker_index", vids.index(cfg["validator_id"]) if cfg["validator_id"] in vids else 0))
    data_dir = cfg.get("data_dir")
    if data_dir:
        import os

        os.makedirs(data_dir, exist_ok=True)
    log.info("crypto scheme=%s keys_dir=%s data_dir=%s", keyring.scheme, keys_dir, data_dir)
    node = Node(
        validator_id=cfg["validator_id"],
        unl=unl,
        rpc_host=cfg.get("rpc_host", "127.0.0.1"),
        rpc_port=int(cfg["rpc_port"]),
        peer_host=cfg.get("peer_host", "127.0.0.1"),
        peer_port=int(cfg["peer_port"]),
        peers=peers,
        config=pcfg,
        genesis_balances=cfg.get("genesis_balances"),
        keyring=keyring,
        worker_index=wid,
        worker_count=int(cfg.get("worker_count", len(vids))),
        keys_dir=keys_dir,
        data_dir=data_dir,
    )

    def _stop(*_a: Any) -> None:
        node._running = False

    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGTERM, _stop)
    asyncio.run(node.run())


if __name__ == "__main__":
    main(sys.argv[1:])
