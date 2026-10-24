#!/usr/bin/env python3
"""Offline unit tests for pl-prod-soak-check.py (no network).

  python3 crates/fd-pl/scripts/test_pl_prod_soak_check.py
"""
import importlib.util
import os
import unittest

_HERE = os.path.dirname(os.path.realpath(__file__))
_spec = importlib.util.spec_from_file_location("soak", os.path.join(_HERE, "pl-prod-soak-check.py"))
soak = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(soak)

GOOD_CFG = {
    "present": True, "parsed": True, "qc_cfg_digest": "d1", "network_id": 2300,
    "qc_header_version": 4, "qc_start_height": 353091, "drift": False, "drift_reasons": [],
}


def seat(sid, tip=100, h="aa", **kw):
    b = {
        "id": sid, "network_id": 2300, "tip_height": tip, "tip_hash_prefix": h,
        "qc_header_version": 4, "qc_start_height": 353091, "vote_rounds_from_height": 369998,
        "lc_start_height": 116957, "lottery_order": list("123456"), "online_seats": list("123456"),
        "bonded": [{"id": x} for x in "123456"], "liveness": {}, "bridge_cfg": None,
    }
    b.update(kw)
    return b


def crit(msgs, needle):
    return any(m.startswith("CRITICAL") and needle in m for m in msgs)


class Soak(unittest.TestCase):
    def test_healthy(self):
        sev, msgs = soak.evaluate(seat("v1", bridge_cfg=GOOD_CFG), [("a", seat("v3", bridge_cfg=GOOD_CFG))])
        self.assertEqual(sev, 0, msgs)

    def test_stale_hub_with_peer_ahead_is_critical(self):
        sev, msgs = soak.evaluate(seat("v1", tip=100), [("a", seat("v3", tip=400, h="bb"))])
        self.assertEqual(sev, 2, msgs)
        self.assertTrue(crit(msgs, "lag hub v1"), msgs)

    def test_lagging_peer_warn_and_crit(self):
        sev, msgs = soak.evaluate(seat("v1", tip=400), [("a", seat("v3", tip=370, h="bb"))])
        self.assertEqual(sev, 1, msgs)
        sev, msgs = soak.evaluate(seat("v1", tip=400), [("a", seat("v3", tip=100, h="bb"))])
        self.assertTrue(crit(msgs, "lag a v3"), msgs)

    def test_divergence_between_peers_not_only_vs_hub(self):
        sev, msgs = soak.evaluate(
            seat("v1", tip=99, h="aa"), [("a", seat("v3", tip=100, h="bb")), ("b", seat("v6", tip=100, h="cc"))]
        )
        self.assertTrue(crit(msgs, "diverge at h=100"), msgs)

    def test_bridge_network_id_must_match_node(self):
        wrong = dict(GOOD_CFG, network_id=2200)
        # Same wrong value on every seat: digests agree, still critical.
        sev, msgs = soak.evaluate(seat("v1", bridge_cfg=wrong), [("a", seat("v3", bridge_cfg=wrong))])
        self.assertEqual(sev, 2, msgs)
        self.assertTrue(crit(msgs, "bridge.json network_id=2200 expected 2300"), msgs)

    def test_drift_reasons_reported(self):
        d = dict(GOOD_CFG, drift=True, drift_reasons=["file_changed_since_start"])
        sev, msgs = soak.evaluate(seat("v1", bridge_cfg=GOOD_CFG), [("a", seat("v13", bridge_cfg=d))])
        self.assertTrue(crit(msgs, "file_changed_since_start"), msgs)

    def test_digest_mismatch_and_runtime_fields(self):
        d2 = dict(GOOD_CFG, qc_cfg_digest="d2")
        sev, msgs = soak.evaluate(seat("v1", bridge_cfg=GOOD_CFG), [("a", seat("v3", bridge_cfg=d2))])
        self.assertTrue(crit(msgs, "qc_cfg_digest differs"), msgs)
        sev, msgs = soak.evaluate(seat("v1"), [("a", seat("v13", vote_rounds_from_height=0))])
        self.assertTrue(crit(msgs, "vote_rounds_from_height=0"), msgs)

    # --- v2.9.48 Phase 2 pause window ---------------------------------------
    PAUSE = dict(vote_rounds_pause_from=371037, vote_rounds_resume_at=375000)

    def test_inside_window_round_scoped_false_is_expected(self):
        b = dict(tip=371600, round_scoped_votes=False, **self.PAUSE)
        sev, msgs = soak.evaluate(seat("v1", **b), [("a", seat("v3", **b))])
        self.assertEqual(sev, 0, msgs)
        self.assertTrue(any("resumes in 3399 blocks" in m for m in msgs), msgs)

    def test_after_resume_round_scoped_false_is_critical(self):
        b = dict(tip=375000, round_scoped_votes=False, **self.PAUSE)
        sev, msgs = soak.evaluate(seat("v1", **b), [("a", seat("v3", **b))])
        self.assertTrue(crit(msgs, "round_scoped_votes=false"), msgs)

    def test_wrong_window_is_config_critical(self):
        good = dict(tip=371600, round_scoped_votes=False, **self.PAUSE)
        bad = dict(good, vote_rounds_resume_at=380000)
        sev, msgs = soak.evaluate(seat("v1", **good), [("a", seat("v3", **bad))])
        self.assertTrue(crit(msgs, "vote_rounds_pause=371037..380000 expected 371037..375000"), msgs)

    def test_pre_upgrade_seat_near_resume_is_critical(self):
        new = dict(tip=374500, round_scoped_votes=False, **self.PAUSE)
        # zzz-legacy-unjam override on an old binary: no pause fields, vr=999999999.
        old = dict(tip=374500, round_scoped_votes=False, vote_rounds_from_height=999999999)
        exp = dict(soak.DEFAULT_EXPECT, vote_rounds_from=0)
        sev, msgs = soak.evaluate(seat("v1", **new), [("a", seat("v10", **old))], exp)
        self.assertTrue(crit(msgs, "a(v10) not round-scoped at resume 375000"), msgs)
        # Far from resume: only informational.
        far_new, far_old = dict(new, tip=371600), dict(old, tip=371600)
        sev, msgs = soak.evaluate(seat("v1", **far_new), [("a", seat("v10", **far_old))], exp)
        self.assertFalse(any("not round-scoped at resume" in m for m in msgs), msgs)

    def test_timeout_is_not_unreachable(self):
        sev, msgs = soak.evaluate(seat("v1"), [("a", {"_timeout": "status from a timed out after 45s"})])
        self.assertEqual(sev, 1, msgs)
        self.assertTrue(any(m.startswith("WARN peer status timeout a") for m in msgs), msgs)
        self.assertFalse(any("unreachable" in m for m in msgs), msgs)

    def test_ssh_via_status_cmd_filters_big_blobs(self):
        old = soak.SSH_VIA
        try:
            soak.SSH_VIA = "falcon@host1"
            cmd = soak.status_cmd("ctl", "100.64.0.7:19307")
            self.assertEqual(cmd[-2], "falcon@host1")
            self.assertIn("falcon_qc", cmd[-1])
            self.assertIn("python3 -c", cmd[-1])
        finally:
            soak.SSH_VIA = old

    # --- disk (v2.9.53) ---
    DF_FALCON2 = (
        "Filesystem 1-blocks Used Available Capacity Mounted on\n"
        "/dev/mapper/ubuntu--vg-ubuntu--lv 248215199744 236990300160 1757184 100% /\n"
        "/dev/mapper/ubuntu--vg-ubuntu--lv 248215199744 236990300160 1757184 100% /\n"
    )
    DF_MHR = (
        "Filesystem 1-blocks Used Available Capacity Mounted on\n"
        "/dev/sda1 80290492416 20005298176 56957591552 26% /\n"
        "tmpfs 1997152256 10182656 1986969600 1% /tmp\n"
    )

    def test_parse_df_dedupes_mounts(self):
        rows = soak.parse_df(self.DF_FALCON2)
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["mount"], "/")
        self.assertLess(rows[0]["free_pct"], 0.01)
        rows = soak.parse_df(self.DF_MHR)
        self.assertEqual([r["mount"] for r in rows], ["/", "/tmp"])

    def test_disk_full_host_is_critical(self):
        disks = [("falcon@host2", soak.parse_df(self.DF_FALCON2)), ("root@mhr", soak.parse_df(self.DF_MHR))]
        sev, msgs = soak.evaluate(seat("v1", bridge_cfg=GOOD_CFG), [], disks=disks)
        self.assertEqual(sev, 2, msgs)
        self.assertTrue(crit(msgs, "disk falcon@host2:/"), msgs)
        # Healthy hosts are still reported (per-host free).
        self.assertTrue(any(m.startswith("OK disk root@mhr:/tmp") for m in msgs), msgs)

    def test_disk_warn_band_and_unreachable(self):
        g = 10**9
        warn_rows = [{"fs": "x", "mount": "/", "total": 100 * g, "avail": 8 * g, "free_pct": 8.0}]
        sev, msgs = soak.evaluate(seat("v1", bridge_cfg=GOOD_CFG), [], disks=[("h", warn_rows), ("dead", None)])
        self.assertEqual(sev, 1, msgs)
        self.assertTrue(any(m.startswith("WARN disk h:/") for m in msgs), msgs)
        self.assertTrue(any("WARN disk dead" in m for m in msgs), msgs)

    def test_seat_status_disk_field(self):
        d = {"known": True, "avail_bytes": 3 * 10**9, "total_bytes": 100 * 10**9, "free_pct": 3.0,
             "level": "low", "skipped_dumps": 2, "last_skip": "hub join snapshot: low disk"}
        sev, msgs = soak.evaluate(seat("v1", bridge_cfg=GOOD_CFG), [("a", seat("v3", bridge_cfg=GOOD_CFG, disk=d))])
        self.assertEqual(sev, 2, msgs)
        self.assertTrue(crit(msgs, "seat a v3"), msgs)
        self.assertTrue(any("skipped 2 optional" in m for m in msgs), msgs)
        # slim keeps the disk block
        self.assertEqual(soak.slim({"disk": d})["disk"]["free_pct"], 3.0)

    def test_disk_cmd_is_read_only_df(self):
        cmd = soak.disk_cmd("falcon@host2")
        self.assertEqual(cmd[-2], "falcon@host2")
        self.assertTrue(cmd[-1].startswith("df -P -B1 "), cmd)

    def test_parse_pause(self):
        self.assertEqual(soak.parse_pause("371037..375000"), (371037, 375000))
        self.assertEqual(soak.parse_pause("off"), (0, 0))
        self.assertIsNone(soak.parse_pause(""))


if __name__ == "__main__":
    unittest.main()
