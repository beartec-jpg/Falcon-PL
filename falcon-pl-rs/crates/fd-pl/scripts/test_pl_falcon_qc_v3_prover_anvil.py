#!/usr/bin/env python3
"""Live `--send` path of pl-falcon-qc-v3-prover.py against a local anvil.

Starts anvil on a free port, loads anvil's public test key 0 through
FPL_V3_PROVER_KEY, and sends a real `submitHeaderWithProofs` call (to a plain
address, so it just carries calldata) through MemfdSigner + emit(). While cast
runs, every other process's /proc/<pid>/cmdline and /proc/<pid>/environ is
scanned for the key. Skips when anvil/cast are missing. Kills anvil on exit.

This scan skips the test's own pid, so it cannot see the prover process's
own environ. `ProverEnvironAfterReexec` in test_pl_falcon_qc_v3_prover.py runs
the real script and checks the running prover's /proc/<pid>/environ.
"""
import importlib.util
import os
import socket
import subprocess
import threading
import time
import unittest

_HERE = os.path.dirname(os.path.realpath(__file__))
_PATH = os.path.join(_HERE, "pl-falcon-qc-v3-prover.py")
_spec = importlib.util.spec_from_file_location("v3_prover_anvil", _PATH)
prov = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(prov)

FOUNDRY = os.path.expanduser("~/.foundry/bin")
ANVIL = os.environ.get("ANVIL", os.path.join(FOUNDRY, "anvil"))
CAST = os.environ.get("CAST", os.path.join(FOUNDRY, "cast"))
# anvil's public default mnemonic; account 0 is anvil's well-known funded
# test account. Derived at runtime so no key literal lives in the repo.
ANVIL_MNEMONIC = "test test test test test test test test test test test junk"
ANVIL_ADDR0 = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266"


def anvil_key0() -> str:
    return subprocess.check_output(
        [CAST, "wallet", "private-key", ANVIL_MNEMONIC, "0"], text=True, timeout=60
    ).strip()
DUMMY_TO = "0x000000000000000000000000000000000000dEaD"


def free_port() -> int:
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


class ProcScan(threading.Thread):
    """Poll /proc for the key in any other process's cmdline or environ."""

    def __init__(self, needle: str) -> None:
        super().__init__(daemon=True)
        self.needle = needle.lower().encode()
        self.stop = threading.Event()
        self.hits: list[str] = []
        self.cast_send_seen: list[str] = []
        self.scans = 0

    def run(self) -> None:
        me = str(os.getpid())
        while not self.stop.is_set():
            for pid in os.listdir("/proc"):
                if not pid.isdigit() or pid == me:
                    continue
                for part in ("cmdline", "environ"):
                    try:
                        with open(f"/proc/{pid}/{part}", "rb") as f:
                            raw = f.read()
                    except OSError:
                        continue
                    if self.needle in raw.lower():
                        self.hits.append(f"{pid}/{part}")
                    if part == "cmdline":
                        argv = raw.split(b"\0")
                        if argv and argv[0].endswith(b"cast") and b"send" in argv and b"--keystore" in argv:
                            if pid not in self.cast_send_seen:
                                self.cast_send_seen.append(pid)
            self.scans += 1
            time.sleep(0.002)


@unittest.skipUnless(os.path.exists(ANVIL) and os.path.exists(CAST), "anvil/cast not installed")
class AnvilSend(unittest.TestCase):
    def setUp(self) -> None:
        self.port = free_port()
        self.rpc = f"http://127.0.0.1:{self.port}"
        # 3s blocks: cast send waits for the receipt, so the child lives long
        # enough for the /proc scan to see it.
        self.anvil = subprocess.Popen(
            [ANVIL, "--port", str(self.port), "--block-time", "3", "--silent"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        for _ in range(100):
            r = subprocess.run([CAST, "chain-id", "--rpc-url", self.rpc], capture_output=True, text=True)
            if r.returncode == 0:
                break
            time.sleep(0.1)
        else:
            self.fail("anvil did not start")
        self._old = os.environ.get(prov.KEY_ENV)
        self.key0 = anvil_key0()

    def tearDown(self) -> None:
        self.anvil.terminate()
        try:
            self.anvil.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.anvil.kill()
            self.anvil.wait()
        if self._old is None:
            os.environ.pop(prov.KEY_ENV, None)
        else:
            os.environ[prov.KEY_ENV] = self._old

    def test_send_signs_with_env_key_and_key_never_in_proc(self):
        os.environ[prov.KEY_ENV] = self.key0
        key = prov.take_prover_key()
        self.assertNotIn(prov.KEY_ENV, os.environ)
        meta = {
            "header": "11" * 32,
            "parent": "22" * 32,
            "inner": "33" * 32,
            "claim": "44" * 32,
            "next_root": "55" * 32,
            "next_leaves": [f"{i + 1:064x}" for i in range(6)],
        }
        proof = {"a": ["0x1", "0x2"], "b": [["0x3", "0x4"], ["0x5", "0x6"]], "c": ["0x7", "0x8"]}
        quote = prov.parse_gas_quote(
            subprocess.check_output([CAST, "gas-price", "--rpc-url", self.rpc], text=True)
        )
        gas_price = prov.gas_price_wei(quote)
        scan = ProcScan(key[2:])
        scan.start()
        try:
            with prov.MemfdSigner(key) as signer:
                # Default --gas-limit 0: estimate from the signer's own address, x1.25.
                sender = prov.signer_address(signer.argv(), signer.fds)
                self.assertEqual(sender.lower(), ANVIL_ADDR0)
                call_args = prov.submit_command(
                    DUMMY_TO, 400_000, meta, [0, 1, 3, 4], [proof] * 4, 1, gas_price, self.rpc, None, send=False,
                )[3:]
                gas_limit = prov.resolve_gas_limit(prov.GAS_LIMIT_DEFAULT, DUMMY_TO, call_args, self.rpc, sender)
                self.assertNotEqual(gas_limit, prov.GAS_LIMIT_FALLBACK)
                cmd = prov.submit_command(
                    DUMMY_TO, 400_000, meta, [0, 1, 3, 4], [proof] * 4,
                    gas_limit, gas_price, self.rpc, signer.argv(), send=True,
                )
                self.assertNotIn(key[2:], " ".join(cmd).lower())
                tx = prov.emit(cmd, gas_limit, gas_price, send=True, key=key, pass_fds=signer.fds)
        finally:
            scan.stop.set()
            scan.join(timeout=5)
        print(f"\n  /proc scans={scan.scans} cast-send pids seen={scan.cast_send_seen} key hits={scan.hits}")
        self.assertEqual(scan.hits, [], "key seen in another process's cmdline/environ")
        self.assertTrue(scan.cast_send_seen, f"scan never observed the cast send child ({scan.scans} scans)")
        self.assertTrue(tx.startswith("0x") and len(tx) == 66, tx)
        sender = subprocess.check_output([CAST, "tx", tx, "from", "--rpc-url", self.rpc], text=True).strip()
        self.assertEqual(sender.lower(), ANVIL_ADDR0)
        status = subprocess.check_output(
            [CAST, "receipt", tx, "status", "--rpc-url", self.rpc], text=True
        ).strip()
        self.assertTrue(status.lower().startswith(("1", "true", "0x1")) or "success" in status.lower(), status)
        to = subprocess.check_output([CAST, "tx", tx, "to", "--rpc-url", self.rpc], text=True).strip()
        self.assertEqual(to.lower(), DUMMY_TO.lower())
        gas = subprocess.check_output([CAST, "tx", tx, "gas", "--rpc-url", self.rpc], text=True).strip()
        self.assertEqual(int(gas), gas_limit)
        used = int(subprocess.check_output([CAST, "receipt", tx, "gasUsed", "--rpc-url", self.rpc], text=True).split()[0])
        self.assertLessEqual(used, gas_limit)

    def test_control_scanner_catches_the_old_private_key_flag(self):
        """Negative control: the old `--private-key` argv is visible to the scan."""
        scan = ProcScan(self.key0[2:])
        scan.start()
        try:
            subprocess.run(
                [CAST, "send", DUMMY_TO, "--private-key", self.key0, "--rpc-url", self.rpc, "--legacy"],
                capture_output=True, text=True, timeout=60, env=prov.child_env(),
            )
        finally:
            scan.stop.set()
            scan.join(timeout=5)
        self.assertTrue(any(h.endswith("/cmdline") for h in scan.hits), scan.hits)


if __name__ == "__main__":
    unittest.main()
