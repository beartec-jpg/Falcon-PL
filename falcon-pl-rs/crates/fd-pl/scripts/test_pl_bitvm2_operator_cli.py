"""#47: the operator's btc-bitvm2-user-kickoff flags must exist in falcon-pl-ctl,
and the Kickoff path must not read setup secrets (no sudo cat, no setup files)."""
import pathlib
import re
import unittest

HERE = pathlib.Path(__file__).resolve().parent
OP = (HERE / "pl-bitvm2-operator.py").read_text()
CTL = (HERE.parent / "src/bin/falcon_pl_ctl.rs").read_text()


def ctl_flags(variant: str) -> set[str]:
    m = re.search(variant + r" \{(.*?)\n    \},", CTL, re.S)
    assert m, variant
    return {"--" + f.replace("_", "-") for f in re.findall(r"\n\s+(\w+):", m.group(1))}


class Cli(unittest.TestCase):
    def test_kickoff_flags_match_ctl(self):
        body = OP[OP.index("def kickoff_user_from_instance"):OP.index("def payout_instance_to_operator")]
        used = set(re.findall(r'"(--[a-z-]+)"', body))
        self.assertTrue(used)
        self.assertEqual(used - ctl_flags("BtcBitvm2UserKickoff"), set())

    def test_no_setup_secret_reads(self):
        self.assertNotIn("sudo", OP)
        self.assertNotIn("setup-1", OP)
        self.assertNotIn("setup-secret-files", OP)


if __name__ == "__main__":
    unittest.main()
