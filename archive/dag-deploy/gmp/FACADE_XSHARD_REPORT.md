# GMP facade + cross-shard dig

**Pass:** True

- route_ok=True (acct5→s0, acct25→s1)
- same_shard ok/err=20/0
- xshard accept=True credit_ok=True debit_exec=True
- acct5 seq 5→6 acct25 bal 1000000000→1000000005
- final status: `credited`
- facade stats: `{'same_shard_err': 0, 'same_shard_ok': 20, 'xshard_credit_err': 0, 'xshard_credit_ok': 1, 'xshard_idempotent_hits': 0, 'xshard_refund_err': 0, 'xshard_refund_ok': 0, 'xshard_started': 1, 'xshard_timeout': 0}`

Cross-shard lab protocol: source pays bridge → wait exec → dest bridge pays recipient.
